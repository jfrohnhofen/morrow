//! Alarm scheduling and the sunrise ramp.
//!
//! The original firmware tested the alarm once per ~6.6 s tick and required the
//! seconds field to be `<= 5`. Those two numbers do not line up: a tick landing
//! in the wrong place skips the window entirely and the alarm silently does not
//! go off that day. Here the match is on hour and minute, with
//! [`App::last_fire_minute`] making it fire exactly once per occurrence
//! regardless of how often this runs.
//!
//! The ramp is driven from elapsed time rather than by stepping once per tick,
//! which is what makes the duration a real setting instead of a side effect of
//! the loop rate.

use embassy_time::{Duration, Instant};

use crate::leds::FULL;
use crate::settings::Settings;
use crate::state::{Alarm, App, UiState};
use crate::tz;

/// How often [`tick`] should be called.
///
/// Fast enough that the ramp advances in imperceptible steps — 18,000 of them
/// across a 30-minute sunrise — and far faster than the one-minute granularity
/// the trigger needs.
pub const TICK: Duration = Duration::from_millis(100);

/// Advances the alarm: checks whether to start, and drives the ramp.
pub fn tick(app: &mut App) {
    let Some(utc) = app.clock.now() else {
        // No clock yet, so no idea whether it is time. Coast.
        return;
    };
    let (local, _, _) = tz::local(utc, &app.settings.tz());
    let minute = utc.div_euclid(60);

    let due = app.settings.mode.fires_on(local.weekday)
        && local.hour == app.settings.hour
        && local.minute == app.settings.minute;

    if due && app.alarm == Alarm::Idle && app.last_fire_minute != Some(minute) {
        defmt::info!(
            "alarm: firing, sunrise over {} s",
            app.settings.sunrise_secs
        );
        app.last_fire_minute = Some(minute);
        app.alarm = Alarm::Rising {
            started: Instant::now(),
        };
        app.ui = UiState::Alarm;
        app.display_dirty = true;
    }

    match app.alarm {
        Alarm::Idle => {}
        Alarm::Rising { started } => {
            let total = app.settings.sunrise_secs.max(1) as u64;
            let elapsed = started.elapsed().as_millis() / 1000;
            if elapsed >= total {
                if app.settings.hold_secs > 0 {
                    app.lamp_target = FULL;
                    app.alarm = Alarm::Holding {
                        until: Instant::now()
                            + Duration::from_secs(app.settings.hold_secs as u64),
                    };
                } else {
                    // Matches the original: at the top of the ramp the lamp goes
                    // out rather than staying lit.
                    defmt::info!("alarm: sunrise complete");
                    finish(app);
                }
            } else {
                // Scale in u64 so the multiply cannot overflow.
                let ms = started.elapsed().as_millis();
                app.lamp_target = (ms * FULL as u64 / (total * 1000)).min(FULL as u64) as u16;
            }
        }
        Alarm::Holding { until } => {
            app.lamp_target = FULL;
            if Instant::now() >= until {
                defmt::info!("alarm: hold complete");
                finish(app);
            }
        }
    }
}

/// Ends a sunrise and returns the UI to rest.
fn finish(app: &mut App) {
    app.alarm = Alarm::Idle;
    app.lamp_target = 0;
    if app.ui == UiState::Alarm {
        app.ui = UiState::Idle;
        app.display_dirty = true;
    }
}

/// When the alarm next goes off, in Unix seconds.
///
/// `None` when the alarm is off or the clock has never been set. Searches eight
/// days so that a once-weekly schedule always resolves, whichever day it is.
pub fn next_occurrence(utc: i64, s: &Settings) -> Option<i64> {
    if s.mode.mask() == 0 {
        return None;
    }
    let rule = s.tz();
    for day in 0..8i64 {
        // Probe local noon on each day to pick the date, which keeps the search
        // clear of both DST transitions and midnight.
        let probe = utc + day * 86400;
        let (d, _, _) = tz::local(probe, &rule);
        let days = tz::days_from_civil(d.year, d.month as u32, d.day as u32);
        let weekday = tz::weekday_from_days(days);
        if !s.mode.fires_on(weekday) {
            continue;
        }
        // Convert the target local time back to UTC using the offset in force
        // around then, then re-resolve so a transition in between is respected.
        let naive = days * 86400 + s.hour as i64 * 3600 + s.minute as i64 * 60;
        let (_, _, off) = tz::local(naive - 3600, &rule);
        let candidate = naive - off as i64;
        let (_, _, off2) = tz::local(candidate, &rule);
        let candidate = naive - off2 as i64;
        if candidate > utc {
            return Some(candidate);
        }
    }
    None
}
