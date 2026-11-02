//! Serves the morrow web UI on the host, against mock state.
//!
//! The pages live in `morrow-ui`, which has no hardware dependencies, so the
//! exact bytes the device would send can be rendered here and opened in a
//! browser. Forms are wired to the mock state, so clicking through behaves like
//! the real thing — no flashing, no device needed.
//!
//! ```text
//! nix run .#devserver              # http://127.0.0.1:8080
//! nix run .#devserver -- --setup   # open on the provisioning page instead
//! nix run .#devserver -- --port 9000
//! ```
//!
//! Page sizes are logged on every request, because the firmware renders into a
//! fixed buffer and a page that outgrows it would be truncated on the device
//! while looking perfect here.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Mutex;
use std::time::Instant;

use morrow_ui::{Alarm, DateTime, Network, Setup, Status, form, page};

/// Mock device state, mutated by the forms.
struct Mock {
    hour: u8,
    minute: u8,
    mode: u8,
    sunrise_secs: u32,
    hold_secs: u32,
    led_count: u16,
    tz_name: String,
    tz_rule: String,
    ssid: String,
    lamp_pct: u8,
    setup_mode: bool,
    pending_confirm: bool,
    booted: Instant,
}

impl Mock {
    fn new(setup_mode: bool) -> Mock {
        Mock {
            hour: 7,
            minute: 30,
            mode: 1,
            sunrise_secs: 30 * 60,
            hold_secs: 0,
            led_count: 656,
            tz_name: "Europe/Berlin".into(),
            tz_rule: "CET-1CEST,M3.5.0,M10.5.0/3".into(),
            ssid: "hausnetz".into(),
            lamp_pct: 0,
            setup_mode,
            // Flip with --pending to preview the post-update warning.
            pending_confirm: std::env::args().any(|a| a == "--pending"),
            booted: Instant::now(),
        }
    }
}

/// Reads the curated zone list out of tzdata, the way `build.rs` does, so the
/// dropdown previewed here matches the one the firmware ships.
fn zones() -> Vec<(String, String)> {
    let dir = std::env::var("TZDIR").unwrap_or_else(|_| "/usr/share/zoneinfo".into());
    let names = [
        "UTC", "Europe/Berlin", "Europe/London", "Europe/Dublin", "Europe/Lisbon",
        "Europe/Madrid", "Europe/Paris", "Europe/Amsterdam", "Europe/Brussels",
        "Europe/Zurich", "Europe/Vienna", "Europe/Prague", "Europe/Warsaw", "Europe/Rome",
        "Europe/Stockholm", "Europe/Oslo", "Europe/Copenhagen", "Europe/Helsinki",
        "Europe/Athens", "Europe/Istanbul", "Europe/Kyiv", "Europe/Moscow",
        "America/New_York", "America/Chicago", "America/Denver", "America/Los_Angeles",
        "America/Anchorage", "America/Sao_Paulo", "Asia/Jerusalem", "Asia/Dubai",
        "Asia/Kolkata", "Asia/Singapore", "Asia/Hong_Kong", "Asia/Shanghai", "Asia/Tokyo",
        "Asia/Seoul", "Australia/Perth", "Australia/Sydney", "Pacific/Auckland",
    ];
    names
        .iter()
        .filter_map(|n| {
            let b = std::fs::read(std::path::Path::new(&dir).join(n)).ok()?;
            if !b.starts_with(b"TZif") {
                return None;
            }
            let end = b.iter().rposition(|&c| c != 0)?;
            let start = b[..end].iter().rposition(|&c| c == b'\n')? + 1;
            let rule = std::str::from_utf8(&b[start..=end]).ok()?.trim().to_string();
            Some((n.to_string(), rule))
        })
        .collect()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let setup_mode = args.iter().any(|a| a == "--setup");
    let port = args
        .windows(2)
        .find(|w| w[0] == "--port")
        .and_then(|w| w[1].parse().ok())
        .unwrap_or(8080u16);

    let zones = zones();
    if zones.is_empty() {
        eprintln!("warning: no zones resolved; set TZDIR or enter the dev shell");
    }
    let state = Mutex::new(Mock::new(setup_mode));

    let listener = TcpListener::bind(("127.0.0.1", port)).expect("bind");
    println!("morrow web ui  ->  http://127.0.0.1:{port}");
    println!("  {} zones from {}", zones.len(), std::env::var("TZDIR").unwrap_or_default());
    println!("  {} \n", if setup_mode { "serving the provisioning page at /" } else { "serving the status page at /" });

    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                if let Err(e) = handle(s, &state, &zones) {
                    eprintln!("request failed: {e}");
                }
            }
            Err(e) => eprintln!("accept failed: {e}"),
        }
    }
}

/// Reads one request, mutates the mock if it is a form post, and replies.
fn handle(
    mut stream: TcpStream,
    state: &Mutex<Mock>,
    zones: &[(String, String)],
) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("GET").to_string();
    let raw_path = parts.next().unwrap_or("/").to_string();
    let (path, query) = match raw_path.find('?') {
        Some(i) => (raw_path[..i].to_string(), raw_path[i + 1..].to_string()),
        None => (raw_path, String::new()),
    };

    let mut len = 0usize;
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h)? == 0 || h.trim().is_empty() {
            break;
        }
        if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
            len = v.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; len];
    if len > 0 {
        reader.read_exact(&mut body)?;
    }
    let body = String::from_utf8_lossy(&body).to_string();

    let mut st = state.lock().unwrap();
    let mut redirect = None;

    if method == "POST" {
        match path.as_str() {
            "/alarm" => {
                if let Some(v) = form::number(&body, "hour", 23) { st.hour = v as u8 }
                if let Some(v) = form::number(&body, "minute", 59) { st.minute = v as u8 }
                if let Some(v) = form::number(&body, "mode", 3) { st.mode = v as u8 }
                if let Some(v) = form::number(&body, "sunrise", 240) {
                    st.sunrise_secs = v.max(1) * 60
                }
                if let Some(v) = form::number(&body, "hold", 240) { st.hold_secs = v * 60 }
                if let Some(v) = form::number(&body, "leds", 1000) { st.led_count = v.max(1) as u16 }
            }
            "/lamp" => {
                if let Some(v) = form::number(&body, "pct", 100) { st.lamp_pct = v as u8 }
            }
            "/tz" => {
                let mut zbuf = [0u8; 64];
                let mut rbuf = [0u8; 64];
                let zone = form::text(&body, "zone", &mut zbuf).unwrap_or("").to_string();
                let rule = form::text(&body, "rule", &mut rbuf).unwrap_or("").to_string();
                if zone == "Custom" || (zone.is_empty() && !rule.is_empty()) {
                    st.tz_rule = rule;
                    st.tz_name = "Custom".into();
                } else if let Some((n, r)) = zones.iter().find(|(n, _)| *n == zone) {
                    st.tz_name = n.clone();
                    st.tz_rule = r.clone();
                }
            }
            "/wifi" => {
                // Same precedence as the firmware: the scan list wins, the text
                // field is the "other" fallback.
                let mut pick = [0u8; 64];
                let picked = form::text(&body, "pick", &mut pick).unwrap_or("").to_string();
                let mut buf = [0u8; 64];
                let typed = form::text(&body, "ssid", &mut buf).unwrap_or("").to_string();
                let chosen = if picked.is_empty() { typed } else { picked };
                if !chosen.is_empty() {
                    st.ssid = chosen.clone();
                }
                st.setup_mode = false;
                drop(st);
                let mut out = String::new();
                page::connecting(&mut out, env!("CARGO_PKG_VERSION"), &chosen).unwrap();
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n",
                    out.len()
                );
                stream.write_all(resp.as_bytes())?;
                stream.write_all(out.as_bytes())?;
                println!("{method} {path} -> 200, {} bytes", out.len());
                return Ok(());
            }
            "/provision" => {
                st.setup_mode = true;
                redirect = Some("/");
            }
            // The mock list is fixed, but serving the real holding page keeps
            // the flow previewable: it bounces back to / by itself.
            "/scan" => {
                drop(st);
                let mut out = String::new();
                page::scanning(&mut out, env!("CARGO_PKG_VERSION")).unwrap();
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n",
                    out.len()
                );
                stream.write_all(resp.as_bytes())?;
                stream.write_all(out.as_bytes())?;
                println!("{method} {path} -> 200, {} bytes", out.len());
                return Ok(());
            }
            _ => {}
        }
        if redirect.is_none() {
            redirect = Some("/");
        }
    }

    let mut out = String::new();
    if let Some(loc) = redirect {
        drop(st);
        let resp = format!("HTTP/1.1 303 See Other\r\nLocation: {loc}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        stream.write_all(resp.as_bytes())?;
        println!("{method} {path} -> 303 {loc}");
        return Ok(());
    }

    let zrefs: Vec<(&str, &str)> = zones.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let uptime = st.booted.elapsed().as_secs();

    if path == "/scan" {
        drop(st);
        let mut out = String::new();
        page::scanning(&mut out, env!("CARGO_PKG_VERSION")).unwrap();
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n",
            out.len()
        );
        stream.write_all(resp.as_bytes())?;
        stream.write_all(out.as_bytes())?;
        println!("{method} {path} -> 200, {} bytes", out.len());
        return Ok(());
    }

    if st.setup_mode || path == "/setup" || path == "/join" {
        let nets = [
            Network { ssid: "hausnetz", rssi: -47, secured: true },
            Network { ssid: "hausnetz-5g", rssi: -61, secured: true },
            Network { ssid: "FRITZ!Box 7590 XY", rssi: -73, secured: true },
            Network { ssid: "kabelbox-gast", rssi: -88, secured: false },
        ];
        if path == "/join" {
            let mut buf = [0u8; 64];
            let ssid = form::text(&query, "ssid", &mut buf);
            page::setup_join(&mut out, env!("CARGO_PKG_VERSION"), "morrow", ssid).unwrap();
        } else {
            page::setup(
                &mut out,
                &Setup {
                    hostname: "morrow",
                    firmware: env!("CARGO_PKG_VERSION"),
                    networks: &nets,
                    last_error: None,
                },
            )
            .unwrap();
        }
    } else {
        let mode_label = ["off", "weekdays", "weekends", "every day"][st.mode as usize];
        page::status(
            &mut out,
            &Status {
                hostname: "morrow",
                firmware: env!("CARGO_PKG_VERSION"),
                net_mode: "online",
                ssid: &st.ssid,
                ip: Some([192, 168, 1, 42]),
                rssi: Some(-52),
                now: Some(DateTime { year: 2026, month: 10, day: 28, hour: 21, minute: 14 }),
                tz_abbr: "CET",
                tz_offset_secs: 3600,
                tz_name: &st.tz_name,
                tz_rule: &st.tz_rule,
                tzdata_version: "2026e",
                next_transition: Some(DateTime { year: 2027, month: 3, day: 28, hour: 2, minute: 0 }),
                ntp_server: "pool.ntp.org",
                synced: true,
                last_sync_ago: Some(312),
                uptime_secs: uptime.max(1),
                alarm: Alarm {
                    hour: st.hour,
                    minute: st.minute,
                    mode: st.mode,
                    mode_label,
                    sunrise_secs: st.sunrise_secs,
                    hold_secs: st.hold_secs,
                    state: "idle",
                    next: Some(DateTime { year: 2026, month: 10, day: 29, hour: st.hour, minute: st.minute }),
                    led_count: st.led_count,
                },
                lamp_pct: st.lamp_pct,
                relay_on: st.lamp_pct > 0,
                zones: &zrefs,
                pending_confirm: st.pending_confirm,
                max_image: 256 * 1024,
            },
        )
        .unwrap();
    }

    println!("{method} {path} -> 200, {} bytes", out.len());
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n",
        out.len()
    );
    stream.write_all(resp.as_bytes())?;
    stream.write_all(out.as_bytes())?;
    Ok(())
}

// Keeps clippy quiet about the unused import when features shift.
#[allow(dead_code)]
fn _unused(_: HashMap<(), ()>) {}
