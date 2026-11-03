//! Shared state.
//!
//! Everything crossing a task boundary lives in [`APP`], behind a blocking
//! critical-section mutex. Every access is a handful of field reads or writes,
//! so blocking is both cheap and easier to reason about than an async mutex:
//! there is no `await` inside a lock, which makes deadlock structurally
//! impossible.

use core::cell::RefCell;

use embassy_net::Ipv4Cidr;
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_time::{Duration, Instant};

use crate::settings::{Settings, Text};
use crate::tz::{self, DateTime};

/// Wall clock, kept as an offset from the monotonic timer.
///
/// The RP2040 has no battery-backed clock, so there is no time at all until the
/// first NTP reply. After that the crystal carries it — roughly 20 ppm, about
/// 1.7 s of drift per day — and an hourly resync keeps that irrelevant.
#[derive(Copy, Clone, Default)]
pub struct Clock {
    /// Unix seconds at monotonic instant zero.
    offset: Option<i64>,
    /// When the last successful sync landed.
    pub last_sync: Option<Instant>,
}

impl Clock {
    /// Current time in Unix seconds, or `None` before the first sync.
    pub fn now(&self) -> Option<i64> {
        self.offset
            .map(|o| o + Instant::now().as_secs() as i64)
    }

    /// Applies a fresh time from NTP.
    pub fn set(&mut self, unix: i64) {
        self.offset = Some(unix - Instant::now().as_secs() as i64);
        self.last_sync = Some(Instant::now());
    }

    /// Has the clock ever been set?
    pub fn is_valid(&self) -> bool {
        self.offset.is_some()
    }

    /// Broken-down local time under `settings`, or `None` before the first sync.
    pub fn local(&self, settings: &Settings) -> Option<DateTime> {
        let utc = self.now()?;
        Some(tz::local(utc, &settings.tz()).0)
    }
}

/// Which screen the UI is on.
///
/// The first six are the original firmware's states, unchanged. [`Setup`] is new
/// and only reachable when no network has been provisioned.
///
/// [`Setup`]: UiState::Setup
#[derive(Copy, Clone, PartialEq, Eq, defmt::Format)]
pub enum UiState {
    /// Display asleep, dial adjusts the lamp.
    Idle,
    /// Clock and alarm shown.
    Clock,
    /// Editing which days the alarm fires on.
    SetDays,
    /// Editing the alarm hour.
    SetHour,
    /// Editing the alarm minute.
    SetMinute,
    /// Sunrise in progress.
    Alarm,
    /// Access point up, showing the join QR.
    Setup,
}

impl UiState {
    /// Is this one of the three editing screens?
    pub fn is_editing(self) -> bool {
        matches!(self, UiState::SetDays | UiState::SetHour | UiState::SetMinute)
    }

    /// Does this screen light the panel?
    pub fn shows_display(self) -> bool {
        !matches!(self, UiState::Idle | UiState::Alarm)
    }
}

/// How far along a sunrise is.
#[derive(Copy, Clone, PartialEq, Eq)]
pub enum Alarm {
    /// Not firing.
    Idle,
    /// Brightening, having started at this instant.
    Rising { started: Instant },
    /// At full brightness until this instant.
    Holding { until: Instant },
}

/// Overall operating mode of the system.
#[derive(Copy, Clone, PartialEq, Eq, defmt::Format)]
pub enum SystemMode {
    /// Setup mode: alarm disabled, menu functions disabled, button shows QR code.
    /// This is the only mode available when no local time is available.
    Setup,
    /// Operational mode: alarm active, clock and alarm menu enabled.
    Operational,
}

/// What the network is doing.
#[derive(Copy, Clone, PartialEq, Eq, defmt::Format)]
pub enum NetMode {
    /// Radio not yet up.
    Booting,
    /// Access point running for provisioning.
    Setup,
    /// Trying to join the configured network.
    Connecting,
    /// Joined, with an address.
    Online,
}

impl NetMode {
    /// Label used on the status page.
    pub fn label(self) -> &'static str {
        match self {
            NetMode::Booting => "booting",
            NetMode::Setup => "setup",
            NetMode::Connecting => "connecting",
            NetMode::Online => "online",
        }
    }
}

/// Network status, for the status page and the setup screen.
#[derive(Copy, Clone)]
pub struct Net {
    /// Current mode.
    pub mode: NetMode,
    /// Address and prefix, once configured.
    pub addr: Option<Ipv4Cidr>,
    /// Signal strength in dBm, when joined.
    pub rssi: Option<i32>,
    /// SSID currently joined or advertised.
    pub ssid: Text<32>,
    /// Consecutive failed join attempts.
    pub join_failures: u32,
    /// SSID that failed to join during setup provisioning, if any.
    pub last_join_error: Option<Text<32>>,
    /// Whether the current join attempt originated from the setup portal.
    pub from_portal: bool,
    /// Previous mode, for detecting transitions in sync_with_net.
    pub prev_mode: NetMode,
}

impl Net {
    const fn new() -> Net {
        Net {
            mode: NetMode::Booting,
            addr: None,
            rssi: None,
            ssid: Text::new(),
            join_failures: 0,
            last_join_error: None,
            from_portal: false,
            prev_mode: NetMode::Booting,
        }
    }
}

/// Everything shared between tasks.
///
/// `Copy` so a task can take a snapshot inside the lock and render from it
/// outside, keeping critical sections down to a few field reads.
#[derive(Copy, Clone)]
pub struct App {
    /// Persisted settings, as currently in force.
    pub settings: Settings,
    /// Wall clock.
    pub clock: Clock,
    /// Overall system mode (Setup or Operational).
    pub system_mode: SystemMode,
    /// Current screen.
    pub ui: UiState,
    /// Sunrise progress.
    pub alarm: Alarm,
    /// Network status.
    pub net: Net,
    /// Level the lamp task should drive towards.
    pub lamp_target: u16,
    /// Level the lamp task is actually rendering, for the status page.
    pub lamp_current: u16,
    /// Set when settings change and need writing to flash.
    pub settings_dirty: bool,
    /// Set when the display needs redrawing.
    pub display_dirty: bool,
    /// Set to drop back to the provisioning access point.
    pub reprovision: bool,
    /// Set to re-run the network scan while the access point is up.
    pub rescan: bool,
    /// Unix minute the alarm last fired, so it fires once per occurrence.
    pub last_fire_minute: Option<i64>,
    /// When the dial was last touched, for the inactivity timeout.
    pub last_input: Option<Instant>,
    /// When the dial button was pressed down, for long-press detection.
    pub press_start: Option<Instant>,
    /// Incremented by the display task on every pass.
    ///
    /// The watchdog task feeds only when this moves, so a wedged UI loop
    /// resets the device rather than being papered over.
    pub ui_heartbeat: u32,
}

impl App {
    const fn new() -> App {
        App {
            settings: Settings::DEFAULT,
            clock: Clock {
                offset: None,
                last_sync: None,
            },
            system_mode: SystemMode::Setup,
            ui: UiState::Idle,
            alarm: Alarm::Idle,
            net: Net::new(),
            lamp_target: 0,
            lamp_current: 0,
            settings_dirty: false,
            display_dirty: true,
            reprovision: false,
            rescan: false,
            last_fire_minute: None,
            last_input: None,
            press_start: None,
            ui_heartbeat: 0,
        }
    }

    /// Records dial activity and marks the display for redraw.
    pub fn touch(&mut self) {
        self.last_input = Some(Instant::now());
        self.display_dirty = true;
    }

    /// Has the dial been untouched for `timeout`?
    pub fn idle_for(&self, timeout: Duration) -> bool {
        self.last_input.is_none_or(|t| t.elapsed() >= timeout)
    }
}

/// The one shared state instance.
pub static APP: Mutex<CriticalSectionRawMutex, RefCell<App>> =
    Mutex::new(RefCell::new(App::new()));

/// Runs `f` with exclusive access to the shared state.
///
/// Keep the closure short and never block inside it: it runs in a critical
/// section, so it also delays interrupts.
pub fn with<R>(f: impl FnOnce(&mut App) -> R) -> R {
    APP.lock(|cell| f(&mut cell.borrow_mut()))
}
