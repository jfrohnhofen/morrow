//! Builds the web UI's view structs from live device state.
//!
//! Keeps [`morrow_ui`] free of any knowledge of the firmware: it is handed
//! plain data and renders it. That separation is what lets `morrow-devserver`
//! render the same pages on a development machine.

use embassy_time::Instant;
use heapless::String;
use morrow_ui::{Alarm as AlarmView, DateTime as UiDateTime, Network, Setup, Status, page};

use crate::state::{self, Alarm, NetMode};
use crate::{alarm, leds, scan, tz};

use crate::generated::tz_table::{TZDATA_VERSION, ZONES};

/// Hostname claimed over mDNS, without the `.local` suffix.
pub const HOSTNAME: &str = "morrow";

/// Version reported on the pages.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Converts a Unix instant into the UI's date type under `rule`.
fn at(utc: i64, rule: &tz::TzRule) -> UiDateTime {
    let (d, _, _) = tz::local(utc, rule);
    UiDateTime {
        year: d.year,
        month: d.month,
        day: d.day,
        hour: d.hour,
        minute: d.minute,
    }
}

/// Renders the status page into `out`.
///
/// Fails only if the page outgrows the buffer, which the caller reports as a
/// 500 rather than sending a truncated document.
pub fn render_status<const N: usize>(out: &mut String<N>) -> core::fmt::Result {
    // Snapshot under the lock, render outside it: rendering touches several
    // kilobytes and must not sit inside a critical section.
    let snap = state::with(|a| {
        (
            a.settings,
            a.clock,
            a.net,
            a.alarm,
            a.system_mode,
            a.lamp_current,
            a.lamp_target,
        )
    });
    let (settings, clock, net, alarm_phase, system_mode, lamp_current, lamp_target) = snap;

    // Asking the bootloader costs a flash read, so only do it when the page is
    // actually being rendered — and skip it if the flash is busy, since a
    // status page is never worth blocking an update for.
    let pending_confirm = match crate::flash::STORE.try_lock() {
        Ok(mut g) => g
            .as_mut()
            .and_then(|s| s.ota_state())
            .is_some_and(|st| st == embassy_boot_rp::State::Swap),
        Err(_) => false,
    };

    let rule = settings.tz();
    let now_utc = clock.now();
    let (abbr, offset) = match now_utc {
        Some(utc) => {
            let (_, abbr, off) = tz::local(utc, &rule);
            (abbr, off)
        }
        None => (tz::Abbr::default(), 0),
    };

    let alarm_state = if system_mode == state::SystemMode::Setup {
        "disabled (setup mode)"
    } else {
        match alarm_phase {
            Alarm::Idle => "idle",
            Alarm::Rising { .. } => "rising",
            Alarm::Holding { .. } => "holding",
        }
    };

    // Report the target, not the slew-limited current value: it is what the
    // device is heading for, which is what someone reading the page means.
    let pct = (lamp_target as u32 * 100 / leds::FULL as u32) as u8;

    let ip = net.addr.map(|c| c.address().octets());

    let status = Status {
        hostname: HOSTNAME,
        firmware: VERSION,
        net_mode: net.mode.label(),
        ssid: settings.ssid.as_str(),
        ip,
        rssi: net.rssi,
        now: now_utc.map(|u| at(u, &rule)),
        tz_abbr: abbr.as_str(),
        tz_offset_secs: offset,
        tz_name: settings.tz_name.as_str(),
        tz_rule: settings.tz_rule.as_str(),
        tzdata_version: TZDATA_VERSION,
        next_transition: now_utc
            .and_then(|u| rule.next_transition(u))
            .map(|u| at(u, &rule)),
        ntp_server: crate::NTP_SERVER,
        synced: clock.is_valid(),
        last_sync_ago: clock.last_sync.map(|t| t.elapsed().as_secs()),
        uptime_secs: Instant::now().as_secs(),
        alarm: AlarmView {
            hour: settings.hour,
            minute: settings.minute,
            mode: settings.mode.index(),
            mode_label: settings.mode.label(),
            sunrise_secs: settings.sunrise_secs,
            hold_secs: settings.hold_secs,
            state: alarm_state,
            next: if system_mode == state::SystemMode::Setup {
                None
            } else {
                now_utc
                    .and_then(|u| alarm::next_occurrence(u, &settings))
                    .map(|u| at(u, &rule))
            },
            led_count: settings.led_count,
        },
        lamp_pct: pct,
        relay_on: lamp_current > 0,
        zones: &ZONES,
        pending_confirm,
        max_image: crate::net::http::MAX_IMAGE,
    };

    page::status(out, &status)
}

/// Renders the provisioning page into `out`.
pub fn render_setup<const N: usize>(out: &mut String<N>) -> core::fmt::Result {
    let last_err = state::with(|a| a.net.last_join_error);

    // Snapshot the scan table, then borrow from the snapshot: the view holds
    // string references, so the data has to outlive the render.
    let found = scan::with(|s| s.entries);
    let mut networks: heapless::Vec<Network, { scan::MAX }> = heapless::Vec::new();
    for entry in found.iter().flatten() {
        let _ = networks.push(Network {
            ssid: entry.ssid.as_str(),
            rssi: entry.rssi,
            secured: entry.secured,
        });
    }

    page::setup(
        out,
        &Setup {
            hostname: HOSTNAME,
            firmware: VERSION,
            networks: &networks,
            last_error: last_err.as_ref().map(|s| s.as_str()),
        },
    )
}

/// Renders the credential entry form for joining a network into `out`.
pub fn render_join<const N: usize>(
    out: &mut String<N>,
    ssid: Option<&str>,
) -> core::fmt::Result {
    page::setup_join(out, VERSION, HOSTNAME, ssid)
}

/// Keeps the net-mode import honest for callers that only need the table.
#[allow(dead_code)]
fn _assert_mode(m: NetMode) -> &'static str {
    m.label()
}
