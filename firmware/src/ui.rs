//! The screen state machine and all drawing.
//!
//! The six original states and every transition between them are carried over
//! from `main.py` unchanged, as are the drawing coordinates — which is why the
//! numbers below look arbitrary: they are the panel positions the original
//! firmware used, and matching them is the point.
//!
//! Two things are deliberately not identical:
//!
//! * The inactivity timeout is a fixed [`IDLE_TIMEOUT`]. The original tested a
//!   flag once per ~6.6 s tick, so its real timeout was anywhere from 6.6 s to
//!   13.2 s depending on where in the tick you stopped turning. A fixed value
//!   inside that range is more predictable and no less faithful.
//! * [`UiState::Setup`] is new, and only reachable with no network configured.

use embassy_time::Duration;

use crate::bitmaps::{ALARM, DIGITS, EVERY_DAY, OFF, TIME, WEEKDAYS, WEEKENDS};
use crate::dial::Event;
use crate::display::{Display, PH, PW};
use crate::leds::{FULL, MANUAL_STEP};
use crate::settings::Mode;
use crate::state::{Alarm, App, NetMode, UiState};

use crate::generated::qr::{SETUP_QR, SETUP_QR_SIZE, SETUP_QR_STRIDE, SETUP_SSID};

/// How long the dial must be still before the UI steps back.
///
/// Editing screens commit and fall back to the clock; the clock sleeps the
/// panel.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(8);

/// Pixels per QR module. Two keeps modules at about 0.46 mm on a 1.3-inch
/// panel, which a phone camera handles comfortably; one is marginal.
const QR_SCALE: i32 = 2;

/// Quiet zone in modules. The spec asks for four; two is the widest that still
/// fits the 64-pixel axis at [`QR_SCALE`], and scanners cope.
const QR_QUIET: i32 = 2;

/// Applies a dial event to the shared state.
///
/// Mirrors `handle_dial_event` in `main.py`, including which transitions commit
/// settings to flash.
pub fn handle(app: &mut App, event: Event) {
    app.touch();

    // Any input during a sunrise dismisses it, whatever the event.
    if app.ui == UiState::Alarm {
        defmt::info!("ui: sunrise dismissed");
        app.alarm = Alarm::Idle;
        app.lamp_target = 0;
        app.ui = UiState::Idle;
        return;
    }

    // Provisioning owns the screen, but the lamp still works as a lamp.
    if app.ui == UiState::Setup {
        match event {
            Event::Left => nudge(app, -(MANUAL_STEP as i32)),
            Event::Right => nudge(app, MANUAL_STEP as i32),
            _ => {}
        }
        return;
    }

    match (app.ui, event) {
        (UiState::Idle, Event::Release) => app.ui = UiState::Clock,
        (UiState::Idle, Event::Left) => nudge(app, -(MANUAL_STEP as i32)),
        (UiState::Idle, Event::Right) => nudge(app, MANUAL_STEP as i32),

        (UiState::Clock, Event::Release) => app.ui = UiState::SetDays,
        (UiState::Clock, Event::Left) => nudge(app, -(MANUAL_STEP as i32)),
        (UiState::Clock, Event::Right) => nudge(app, MANUAL_STEP as i32),

        (UiState::SetDays, Event::Release) => {
            // With the alarm off there is no time to edit, so skip straight out.
            if app.settings.mode == Mode::Off {
                app.ui = UiState::Clock;
                app.settings_dirty = true;
            } else {
                app.ui = UiState::SetHour;
            }
        }
        (UiState::SetDays, Event::Left) => cycle_mode(app, -1),
        (UiState::SetDays, Event::Right) => cycle_mode(app, 1),

        (UiState::SetHour, Event::Release) => app.ui = UiState::SetMinute,
        (UiState::SetHour, Event::Left) => adjust_hour(app, -1),
        (UiState::SetHour, Event::Right) => adjust_hour(app, 1),

        (UiState::SetMinute, Event::Release) => {
            app.ui = UiState::Clock;
            app.settings_dirty = true;
        }
        (UiState::SetMinute, Event::Left) => adjust_minute(app, -5),
        (UiState::SetMinute, Event::Right) => adjust_minute(app, 5),

        // Press is tracked as activity only; the original acted on release too.
        _ => {}
    }
}

/// Steps the UI back one level after the dial has gone quiet.
pub fn timeout(app: &mut App) {
    match app.ui {
        s if s.is_editing() => {
            app.ui = UiState::Clock;
            app.settings_dirty = true;
            app.display_dirty = true;
        }
        UiState::Clock => {
            app.ui = UiState::Idle;
            app.display_dirty = true;
        }
        _ => {}
    }
}

/// Changes the lamp level by `delta`, saturating at both ends.
fn nudge(app: &mut App, delta: i32) {
    app.lamp_target = (app.lamp_target as i32 + delta).clamp(0, FULL as i32) as u16;
}

/// Moves through the recurrence modes, wrapping.
fn cycle_mode(app: &mut App, delta: i32) {
    let n = Mode::ALL.len() as i32;
    let i = (app.settings.mode.index() as i32 + delta).rem_euclid(n);
    app.settings.mode = Mode::from_index(i as u8);
}

/// Moves the alarm hour, wrapping at midnight.
fn adjust_hour(app: &mut App, delta: i32) {
    app.settings.hour = (app.settings.hour as i32 + delta).rem_euclid(24) as u8;
}

/// Moves the alarm minute, carrying into the hour.
///
/// The original adjusted the minute and then special-cased the exact values
/// `-5` and `60` to carry. Working in total minutes is equivalent for the
/// multiples of five the dial produces, and also correct for a time set to some
/// other minute over HTTP — which the original would have mangled.
fn adjust_minute(app: &mut App, delta: i32) {
    let total = (app.settings.hour as i32 * 60 + app.settings.minute as i32 + delta)
        .rem_euclid(24 * 60);
    app.settings.hour = (total / 60) as u8;
    app.settings.minute = (total % 60) as u8;
}

/// Draws the current screen.
pub async fn render(display: &mut Display, app: &App) {
    if app.ui == UiState::Setup {
        render_setup(display, app);
        display.on().await;
        display.flush().await;
        return;
    }

    if !app.ui.shows_display() {
        display.off().await;
        return;
    }

    render_clock(display, app);
    display.on().await;
    display.flush().await;
}

/// Draws the clock and alarm screen.
///
/// Every coordinate here matches `main.py`. The panel is mounted portrait, so
/// the two digits of each number sit one above the other in buffer space and
/// read side by side in the hand.
fn render_clock(display: &mut Display, app: &App) {
    display.clear();

    // Current time, drawn only once the clock has been set — exactly as the
    // original skipped it until DCF77 had decoded a frame.
    if let Some(now) = app.clock.local(&app.settings) {
        display.blit(&TIME, 13, 16);
        display.blit(&DIGITS[(now.hour / 10) as usize], 30, 13);
        display.blit(&DIGITS[(now.hour % 10) as usize], 30, 22);
        display.rect(34, 31, 2, 2, true);
        display.rect(39, 31, 2, 2, true);
        display.blit(&DIGITS[(now.minute / 10) as usize], 30, 35);
        display.blit(&DIGITS[(now.minute % 10) as usize], 30, 44);
    }

    display.blit(&ALARM, 60, 8);

    // Recurrence label. Each sits at its own offset because the words are
    // different lengths and were centred by hand.
    match app.settings.mode {
        Mode::EveryDay => display.blit(&EVERY_DAY, 78, 6),
        Mode::Weekdays => display.blit(&WEEKDAYS, 78, 8),
        Mode::Weekends => display.blit(&WEEKENDS, 78, 7),
        Mode::Off => {}
    }

    if app.settings.mode == Mode::Off {
        display.blit(&OFF, 78, 24);
    } else {
        let s = &app.settings;
        display.blit(&DIGITS[(s.hour / 10) as usize], 95, 13);
        display.blit(&DIGITS[(s.hour % 10) as usize], 95, 22);
        display.rect(99, 31, 2, 2, true);
        display.rect(105, 31, 2, 2, true);
        display.blit(&DIGITS[(s.minute / 10) as usize], 95, 35);
        display.blit(&DIGITS[(s.minute % 10) as usize], 95, 44);
    }

    // Invert whichever field is being edited.
    match app.ui {
        UiState::SetDays => display.invert(77, 5, 10, 54),
        UiState::SetHour => display.invert(94, 12, 16, 18),
        UiState::SetMinute => display.invert(94, 34, 16, 18),
        _ => {}
    }
}

/// Draws the provisioning screen: a join QR over the details in text.
///
/// Laid out in physical space, upright, since this screen has no legacy
/// coordinates to preserve. Scanning the QR joins the access point, at which
/// point the captive portal opens the form by itself — so the printed address
/// is only a fallback for a phone that will not scan.
fn render_setup(display: &mut Display, app: &App) {
    display.clear();

    let side = (SETUP_QR_SIZE as i32 + 2 * QR_QUIET) * QR_SCALE;
    let x = (PW - side) / 2;
    display.qr(
        &SETUP_QR,
        SETUP_QR_SIZE,
        SETUP_QR_STRIDE,
        x,
        10,
        QR_SCALE,
        QR_QUIET,
    );

    let mut y = 10 + side + 14;
    // Everything below the QR is optional detail; clip rather than wrap so a
    // longer string can never spill off the panel into nothing.
    let line = |d: &mut Display, s: &str, y: i32| {
        if y + crate::font::FONT_H <= PH {
            d.text_centered(s, y, true);
        }
    };
    line(display, "SCAN TO JOIN", y);
    y += 14;
    line(display, SETUP_SSID, y);

    // If a join was attempted and failed in this setup session, say so:
    // otherwise a wrong password looks identical to never having been configured.
    if app.net.last_join_error.is_some() {
        y += 14;
        line(display, "JOIN FAILED", y);
    }
}

/// Forces the screen to match the network state.
///
/// Provisioning takes over the panel, and leaving it drops back to the sleeping
/// clock rather than stranding the UI on a stale screen.
pub fn sync_with_net(app: &mut App) {
    let setup = app.net.mode == NetMode::Setup;
    if setup && app.ui != UiState::Setup {
        app.ui = UiState::Setup;
        app.display_dirty = true;
    } else if !setup && app.ui == UiState::Setup {
        app.ui = UiState::Idle;
        app.display_dirty = true;
    }
}
