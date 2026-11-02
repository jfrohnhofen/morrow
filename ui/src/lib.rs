//! The morrow web UI, with no dependency on the firmware or its hardware.
//!
//! Splitting this out means the pages can be rendered anywhere a
//! [`core::fmt::Write`] exists: on the device straight into a socket buffer, and
//! on a development machine by `morrow-devserver`, which serves them against
//! mock state so the HTML and CSS can be iterated on in a browser without
//! reflashing.
//!
//! Nothing here allocates, so the same code runs in `no_std`.

#![cfg_attr(not(feature = "std"), no_std)]
#![deny(missing_docs)]

pub mod digits;
pub mod form;
pub mod page;

/// A broken-down local date and time.
#[derive(Copy, Clone, PartialEq, Eq)]
pub struct DateTime {
    /// Gregorian year.
    pub year: i32,
    /// Month, 1..=12.
    pub month: u8,
    /// Day of month, 1..=31.
    pub day: u8,
    /// Hour, 0..=23.
    pub hour: u8,
    /// Minute, 0..=59.
    pub minute: u8,
}

/// Alarm configuration and current progress.
pub struct Alarm<'a> {
    /// Alarm hour, 0..=23.
    pub hour: u8,
    /// Alarm minute, 0..=59.
    pub minute: u8,
    /// Recurrence mode index, matching the firmware's ordering.
    pub mode: u8,
    /// Human-readable recurrence, e.g. `weekdays`.
    pub mode_label: &'a str,
    /// Sunrise ramp length in seconds.
    pub sunrise_secs: u32,
    /// Hold at full brightness in seconds; zero switches off at the top.
    pub hold_secs: u32,
    /// What the alarm is doing right now, e.g. `idle` or `rising`.
    pub state: &'a str,
    /// When it next fires, if it is enabled and the clock is set.
    pub next: Option<DateTime>,
    /// Configured number of LEDs in the lamp strip.
    pub led_count: u16,
}

/// Everything the status page shows.
pub struct Status<'a> {
    /// mDNS hostname, without the `.local` suffix.
    pub hostname: &'a str,
    /// Firmware version string.
    pub firmware: &'a str,
    /// Network state, e.g. `online`.
    pub net_mode: &'a str,
    /// SSID currently joined.
    pub ssid: &'a str,
    /// IPv4 address, if configured.
    pub ip: Option<[u8; 4]>,
    /// Signal strength in dBm, if joined.
    pub rssi: Option<i32>,
    /// Current local time, or `None` before the first NTP sync.
    pub now: Option<DateTime>,
    /// Active zone abbreviation, e.g. `CEST`.
    pub tz_abbr: &'a str,
    /// Active UTC offset in seconds.
    pub tz_offset_secs: i32,
    /// Selected IANA zone name.
    pub tz_name: &'a str,
    /// POSIX rule actually in force.
    pub tz_rule: &'a str,
    /// Version of the tzdata the built-in rules came from.
    pub tzdata_version: &'a str,
    /// Next DST transition under the active rule.
    ///
    /// Shown so a rule that has gone stale is visible: if this names a date that
    /// no longer matches the law, the rule needs updating.
    pub next_transition: Option<DateTime>,
    /// NTP server the clock is kept against, as configured in the firmware.
    pub ntp_server: &'a str,
    /// Has the clock been set from NTP?
    pub synced: bool,
    /// Seconds since the last successful sync.
    pub last_sync_ago: Option<u64>,
    /// Seconds since boot.
    pub uptime_secs: u64,
    /// Alarm configuration and progress.
    pub alarm: Alarm<'a>,
    /// Lamp brightness as a percentage.
    pub lamp_pct: u8,
    /// Is the relay energised?
    pub relay_on: bool,
    /// Selectable zones, as (IANA name, POSIX rule).
    pub zones: &'a [(&'a str, &'a str)],
    /// Is the running image still awaiting confirmation?
    ///
    /// True in the window after an update, before the firmware has convinced
    /// the bootloader it works. A reset during that window reverts.
    pub pending_confirm: bool,
    /// Largest firmware image the spare slot can take, in bytes.
    pub max_image: usize,
}

/// A network seen by the most recent scan.
pub struct Network<'a> {
    /// Advertised SSID.
    pub ssid: &'a str,
    /// Signal strength in dBm.
    pub rssi: i32,
    /// Does joining need a passphrase?
    pub secured: bool,
}

/// Everything the provisioning page shows.
pub struct Setup<'a> {
    /// mDNS hostname the device will claim once joined.
    pub hostname: &'a str,
    /// Firmware version string.
    pub firmware: &'a str,
    /// Networks from the last scan, strongest first.
    pub networks: &'a [Network<'a>],
    /// Whether a previous join attempt failed, and for which SSID.
    pub last_error: Option<&'a str>,
}
