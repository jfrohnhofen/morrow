//! HTTP server and captive portal.
//!
//! A small HTTP/1.1 subset: request line, headers, optional body, one response,
//! connection closed. That is all the pages need, and hand-rolling it keeps full
//! control over the captive-portal semantics, which a general-purpose server
//! would fight.
//!
//! # Captive portal behaviour
//!
//! While the access point is up, *every* GET returns the setup page with a
//! `200`. That is deliberate and is what makes the form appear by itself:
//!
//! * Android probes `/generate_204` and expects `204` with an empty body.
//!   Anything else makes it show "Sign in to network".
//! * iOS and macOS probe `/hotspot-detect.html` and expect a body containing
//!   `Success`. Anything else opens the Captive Network Assistant, and serving
//!   the page there puts the form directly in that sheet.
//! * Windows probes `/connecttest.txt` and expects `Microsoft Connect Test`.
//!
//! Serving the real page to all of them satisfies none of their expectations,
//! which is exactly the intent.

use core::fmt::Write as _;
// For `write_all`: the inherent `write` returns how many bytes it accepted,
// which for any page here is less than all of them.
use embedded_io_async::Write as _;

use embassy_net::tcp::TcpSocket;
use embassy_net::Stack;
use embassy_time::{Duration, with_timeout};
use heapless::String;
use morrow_ui::{form, page};

use crate::state::{self, Alarm, NetMode};
use crate::{flash, settings, views};

/// Port to listen on.
const PORT: u16 = 80;

/// Largest request this will read. Headers from a phone run to a few hundred
/// bytes; anything near this is not a browser.
const REQ_MAX: usize = 2048;

/// Render buffer size. Leaves room for the page and zone list to grow
/// without silently truncating.
pub const PAGE_MAX: usize = 24 * 1024;

/// How long a connection may sit idle before being dropped.
const TIMEOUT: Duration = Duration::from_secs(10);

/// Largest firmware image accepted, matching the ACTIVE slot in `memory.x`.
pub const MAX_IMAGE: usize = 256 * 1024;

/// Header carrying the CRC-32 the uploader computed over the image.
const CRC_HEADER: &str = "x-image-crc32:";

/// Serves HTTP. Two of these run so a stalled connection cannot block the page.
#[embassy_executor::task(pool_size = 2)]
pub async fn task(
    stack: Stack<'static>,
    rx: &'static mut [u8],
    tx: &'static mut [u8],
    body: &'static mut String<PAGE_MAX>,
) -> ! {
    loop {
        let mut sock = TcpSocket::new(stack, rx, tx);
        sock.set_timeout(Some(TIMEOUT));

        if let Err(e) = sock.accept(PORT).await {
            defmt::warn!("http: accept failed: {:?}", e);
            continue;
        }

        if let Err(e) = serve(&mut sock, body).await {
            defmt::debug!("http: connection ended: {:?}", e);
        }
        sock.close();
        // Let the peer see the FIN before the socket is recycled.
        let _ = with_timeout(Duration::from_secs(2), sock.flush()).await;
        sock.abort();
    }
}

/// Reads one request and writes one response.
async fn serve(
    sock: &mut TcpSocket<'_>,
    body: &mut String<PAGE_MAX>,
) -> Result<(), embassy_net::tcp::Error> {
    let mut req = [0u8; REQ_MAX];
    let mut n = 0;

    // Read until the headers are complete, then until the body is too.
    let (head_len, content_len) = loop {
        let got = match with_timeout(TIMEOUT, sock.read(&mut req[n..])).await {
            Ok(Ok(0)) | Err(_) => return Ok(()),
            Ok(Ok(k)) => k,
            Ok(Err(e)) => return Err(e),
        };
        n += got;
        if let Some(end) = find_headers_end(&req[..n]) {
            break (end, content_length(&req[..end]));
        }
        if n == REQ_MAX {
            return respond(sock, 431, "text/plain", "headers too large").await;
        }
    };

    // Resolve the route while the head is still borrowed, so nothing borrowed
    // from `req` outlives the point where the body read needs it mutably.
    let (route, crc, join_ssid) = {
        let head = core::str::from_utf8(&req[..head_len]).unwrap_or("");
        let mut line = head.lines().next().unwrap_or("").split(' ');
        let method = line.next().unwrap_or("");
        let full_path = line.next().unwrap_or("/");
        let (path, query) = match full_path.find('?') {
            Some(i) => (&full_path[..i], &full_path[i + 1..]),
            None => (full_path, ""),
        };
        defmt::info!("http: {=str} {=str}", method, path);
        let mut ssid_buf = [0u8; 64];
        let mut join_ssid = heapless::String::<64>::new();
        if let Some(s) = form::text(query, "ssid", &mut ssid_buf) {
            let _ = join_ssid.push_str(s);
        }
        (
            Route::parse(method, path),
            header(head, CRC_HEADER).and_then(parse_hex),
            join_ssid,
        )
    };

    // Firmware images are far too big to buffer, so they stream straight into
    // flash. Everything else is small enough to read whole.
    if route == Route::PostOta {
        return ota(sock, &req[head_len..n], content_len, crc).await;
    }

    // Read whatever body is left for the small form posts.
    let want = head_len + content_len;
    while n < want.min(REQ_MAX) {
        match with_timeout(TIMEOUT, sock.read(&mut req[n..])).await {
            Ok(Ok(0)) | Err(_) => break,
            Ok(Ok(k)) => n += k,
            Ok(Err(e)) => return Err(e),
        }
    }
    let payload = core::str::from_utf8(&req[head_len..n.min(want)]).unwrap_or("");
    let setup_mode = state::with(|a| a.net.mode) == NetMode::Setup;

    match route {
        Route::PostAlarm => {
            apply_alarm(payload);
            return redirect(sock, "/").await;
        }
        Route::PostLamp => {
            apply_lamp(payload);
            return redirect(sock, "/").await;
        }
        Route::PostTz => {
            apply_tz(payload);
            return redirect(sock, "/").await;
        }
        Route::PostWifi => {
            let ssid = apply_wifi(payload);
            body.clear();
            if page::connecting(body, views::VERSION, &ssid).is_err() {
                return respond(sock, 500, "text/plain", "page too large").await;
            }
            write_head(sock, 200, "text/html; charset=utf-8", body.len()).await?;
            sock.write_all(body.as_bytes()).await?;
            return Ok(());
        }
        Route::PostProvision => {
            state::with(|a| a.reprovision = true);
            return redirect(sock, "/").await;
        }
        Route::Scan => {
            // Not a redirect: the scan runs in the wifi task, so the browser is
            // sent a holding page that comes back once the radio is done.
            state::with(|a| {
                a.rescan = true;
                a.net.last_join_error = None;
                a.net.join_failures = 0;
            });
            body.clear();
            if page::scanning(body, views::VERSION).is_err() {
                return respond(sock, 500, "text/plain", "page too large").await;
            }
            write_head(sock, 200, "text/html; charset=utf-8", body.len()).await?;
            sock.write_all(body.as_bytes()).await?;
            return Ok(());
        }
        Route::PostUnknown => return respond(sock, 404, "text/plain", "not found").await,
        Route::Unsupported => {
            return respond(sock, 405, "text/plain", "method not allowed").await;
        }
        Route::PostOta | Route::Get { .. } | Route::Head { .. } => {}
    }

    let (root, join) = match route {
        Route::Get { root, join } => (root, join),
        Route::Head { root } => (root, false),
        _ => (false, false),
    };
    let head_only = matches!(route, Route::Head { .. });

    body.clear();
    let rendered = if setup_mode {
        if join {
            state::with(|a| {
                a.net.last_join_error = None;
                a.net.join_failures = 0;
            });
            let ssid = (!join_ssid.is_empty()).then_some(join_ssid.as_str());
            views::render_join(body, ssid)
        } else {
            views::render_setup(body)
        }
    } else if root {
        views::render_status(body)
    } else {
        return respond(sock, 404, "text/plain", "not found").await;
    };

    if rendered.is_err() {
        // Only reachable if a page outgrows PAGE_MAX. Say so rather than
        // shipping a half-written document that renders as garbage.
        defmt::error!("http: page exceeded {} bytes", PAGE_MAX);
        return respond(sock, 500, "text/plain", "page too large").await;
    }

    write_head(sock, 200, "text/html; charset=utf-8", body.len()).await?;
    if !head_only {
        sock.write_all(body.as_bytes()).await?;
    }
    Ok(())
}

/// What a request is asking for.
///
/// Resolved up front so no borrow of the request buffer has to survive into the
/// body read.
#[derive(Copy, Clone, PartialEq, Eq)]
enum Route {
    /// A page request. `root` distinguishes `/` from anything else.
    Get { root: bool, join: bool },
    /// As [`Route::Get`], but the body is suppressed.
    Head { root: bool },
    /// Alarm settings form.
    PostAlarm,
    /// Lamp brightness form.
    PostLamp,
    /// Timezone form, either a zone or a raw rule.
    PostTz,
    /// WiFi credentials form.
    PostWifi,
    /// Request to return to provisioning mode.
    PostProvision,
    /// Request to re-run the network scan.
    Scan,
    /// A firmware image.
    PostOta,
    /// A POST to something that does not exist.
    PostUnknown,
    /// Any other method.
    Unsupported,
}

impl Route {
    fn parse(method: &str, path: &str) -> Route {
        if path == "/scan" && (method == "GET" || method == "POST") {
            return Route::Scan;
        }
        match method {
            "GET" => Route::Get {
                root: path == "/",
                join: path == "/join",
            },
            "HEAD" => Route::Head { root: path == "/" },
            "POST" => match path {
                "/alarm" => Route::PostAlarm,
                "/lamp" => Route::PostLamp,
                "/tz" => Route::PostTz,
                "/wifi" => Route::PostWifi,
                "/provision" => Route::PostProvision,
                "/ota" => Route::PostOta,
                _ => Route::PostUnknown,
            },
            _ => Route::Unsupported,
        }
    }
}

/// Finds the end of the header block, including its blank line.
fn find_headers_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

/// Reads `Content-Length`, defaulting to zero.
fn content_length(head: &[u8]) -> usize {
    let Ok(s) = core::str::from_utf8(head) else {
        return 0;
    };
    s.lines()
        .find(|l| {
            l.len() > 15 && l.as_bytes()[..15].eq_ignore_ascii_case(b"content-length:")
        })
        .and_then(|l| l[15..].trim().parse().ok())
        .unwrap_or(0)
}

/// Returns the trimmed value of `name`, which must be given lowercase with its
/// trailing colon.
fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines()
        .find(|l| l.len() > name.len() && l.as_bytes()[..name.len()].eq_ignore_ascii_case(name.as_bytes()))
        .map(|l| l[name.len()..].trim())
}

/// Parses up to eight hex digits.
fn parse_hex(s: &str) -> Option<u32> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    if s.is_empty() || s.len() > 8 {
        return None;
    }
    let mut v = 0u32;
    for c in s.bytes() {
        v = (v << 4) | (c as char).to_digit(16)? ;
    }
    Some(v)
}

/// Streams an uploaded firmware image into the spare slot.
///
/// The image is written a sector at a time as it arrives, so nothing is
/// buffered and the erase cost is spread across the transfer rather than
/// stopping the device dead at the start. Only once the whole image has landed
/// and its checksum matches is the slot marked for swap; anything short of that
/// leaves the running firmware untouched.
async fn ota(
    sock: &mut TcpSocket<'_>,
    prefix: &[u8],
    len: usize,
    expected: Option<u32>,
) -> Result<(), embassy_net::tcp::Error> {
    let Some(expected) = expected else {
        return respond(sock, 400, "text/plain", "missing or malformed X-Image-Crc32").await;
    };
    if len == 0 || len > MAX_IMAGE {
        return respond(sock, 413, "text/plain", "image is empty or larger than the slot").await;
    }
    // A flash erase stalls everything for tens of milliseconds, which during a
    // sunrise would show as a visible stutter in the ramp.
    if state::with(|a| a.alarm) != Alarm::Idle {
        return respond(sock, 409, "text/plain", "an alarm is running; try again later").await;
    }

    let mut guard = flash::STORE.lock().await;
    let Some(store) = guard.as_mut() else {
        return respond(sock, 500, "text/plain", "flash unavailable").await;
    };

    defmt::info!("ota: receiving {} bytes, crc {:08x}", len, expected);

    let mut buf = [0u8; flash::OTA_CHUNK];
    let mut filled = 0usize;
    let mut offset = 0usize;
    let mut received = 0usize;
    let mut crc = 0xFFFF_FFFFu32;
    let mut rest = prefix;
    let mut truncated = false;

    while received < len && !truncated {
        // Top the sector buffer up from the bytes already read, then the socket.
        while filled < buf.len() && received < len {
            let room = buf.len().min(filled + (len - received)) - filled;
            let got = if !rest.is_empty() {
                let k = rest.len().min(room);
                buf[filled..filled + k].copy_from_slice(&rest[..k]);
                rest = &rest[k..];
                k
            } else {
                match with_timeout(TIMEOUT, sock.read(&mut buf[filled..filled + room])).await {
                    Ok(Ok(0)) | Err(_) => {
                        truncated = true;
                        break;
                    }
                    Ok(Ok(k)) => k,
                    Ok(Err(e)) => return Err(e),
                }
            };
            filled += got;
            received += got;
        }

        if filled == 0 {
            break;
        }
        crc = settings::crc32_update(crc, &buf[..filled]);
        if store.ota_write(offset, &buf[..filled]).is_err() {
            return respond(sock, 500, "text/plain", "flash write failed").await;
        }
        offset += filled;
        filled = 0;
    }

    if received != len {
        defmt::warn!("ota: truncated at {} of {} bytes", received, len);
        return respond(sock, 400, "text/plain", "upload truncated").await;
    }
    let crc = !crc;
    if crc != expected {
        defmt::warn!("ota: checksum {:08x}, expected {:08x}", crc, expected);
        return respond(sock, 400, "text/plain", "checksum mismatch; image discarded").await;
    }
    if store.ota_mark_updated().is_err() {
        return respond(sock, 500, "text/plain", "could not mark the update").await;
    }

    defmt::info!("ota: image accepted, rebooting into it");
    respond(sock, 200, "text/plain", "ok, rebooting\n").await?;
    let _ = with_timeout(Duration::from_secs(2), sock.flush()).await;
    // Let the response actually leave before the chip resets.
    embassy_time::Timer::after(Duration::from_millis(500)).await;
    cortex_m::peripheral::SCB::sys_reset();
}

/// Writes a status line, standard headers, and the blank line.
async fn write_head(
    sock: &mut TcpSocket<'_>,
    code: u16,
    ctype: &str,
    len: usize,
) -> Result<(), embassy_net::tcp::Error> {
    let reason = match code {
        200 => "OK",
        303 => "See Other",
        400 => "Bad Request",
        404 => "Not Found",
        409 => "Conflict",
        413 => "Payload Too Large",
        405 => "Method Not Allowed",
        431 => "Request Header Fields Too Large",
        _ => "Internal Server Error",
    };
    let mut h: String<192> = String::new();
    let _ = write!(
        h,
        "HTTP/1.1 {code} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {len}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n"
    );
    sock.write_all(h.as_bytes()).await?;
    Ok(())
}

/// Sends a short response with a plain body.
async fn respond(
    sock: &mut TcpSocket<'_>,
    code: u16,
    ctype: &str,
    text: &str,
) -> Result<(), embassy_net::tcp::Error> {
    write_head(sock, code, ctype, text.len()).await?;
    sock.write_all(text.as_bytes()).await?;
    Ok(())
}

/// Sends a 303 so a reload does not repost the form.
async fn redirect(sock: &mut TcpSocket<'_>, to: &str) -> Result<(), embassy_net::tcp::Error> {
    let mut h: String<160> = String::new();
    let _ = write!(
        h,
        "HTTP/1.1 303 See Other\r\nLocation: {to}\r\nContent-Length: 0\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n"
    );
    sock.write_all(h.as_bytes()).await?;
    Ok(())
}

/// Applies the alarm form.
fn apply_alarm(payload: &str) {
    state::with(|a| {
        if let Some(v) = form::number(payload, "hour", 23) {
            a.settings.hour = v as u8;
        }
        if let Some(v) = form::number(payload, "minute", 59) {
            a.settings.minute = v as u8;
        }
        if let Some(v) = form::number(payload, "mode", 3) {
            a.settings.mode = settings::Mode::from_index(v as u8);
        }
        if let Some(v) = form::number(payload, "sunrise", 240) {
            a.settings.sunrise_secs = v.max(1) * 60;
        }
        if let Some(v) = form::number(payload, "hold", 240) {
            a.settings.hold_secs = v * 60;
        }
        if let Some(v) = form::number(payload, "leds", settings::MAX_LEDS as u32) {
            a.settings.led_count = v.max(1) as u16;
        }
        a.settings_dirty = true;
        a.display_dirty = true;
    });
}

/// Applies the lamp form.
fn apply_lamp(payload: &str) {
    let Some(pct) = form::number(payload, "pct", 100) else {
        return;
    };
    state::with(|a| {
        a.lamp_target = (pct * crate::leds::FULL as u32 / 100) as u16;
        // A manual change dismisses a sunrise, matching what the dial does.
        a.alarm = crate::state::Alarm::Idle;
    });
}

/// Applies the timezone form: either a zone from the list, or a raw rule.
fn apply_tz(payload: &str) {
    let mut zbuf = [0u8; 64];
    let mut rbuf = [0u8; 96];
    let zone = form::text(payload, "zone", &mut zbuf);
    let rule = form::text(payload, "rule", &mut rbuf);

    state::with(|a| {
        if let Some(z) = zone {
            if z == "Custom" {
                if let Some(r) = rule {
                    if crate::tz::TzRule::parse(r).is_some() {
                        a.settings.tz_rule = settings::Text::from_str(r);
                        a.settings.tz_name = settings::Text::from_str("Custom");
                        a.settings_dirty = true;
                        a.display_dirty = true;
                        return;
                    } else {
                        defmt::warn!("http: rejected unparseable tz rule");
                    }
                }
            } else if let Some((name, r)) = crate::generated::tz_table::ZONES.iter().find(|(n, _)| *n == z) {
                a.settings.tz_name = settings::Text::from_str(name);
                a.settings.tz_rule = settings::Text::from_str(r);
                a.settings_dirty = true;
                a.display_dirty = true;
                return;
            }
        }
        if let Some(r) = rule {
            // Only accept a rule that parses, so a typo cannot leave the clock
            // stranded in a zone it cannot evaluate.
            if crate::tz::TzRule::parse(r).is_some() {
                a.settings.tz_rule = settings::Text::from_str(r);
                a.settings.tz_name = settings::Text::from_str("Custom");
                a.settings_dirty = true;
                a.display_dirty = true;
            } else {
                defmt::warn!("http: rejected unparseable tz rule");
            }
        }
    });
}

/// Applies the WiFi form and asks the radio to rejoin.
fn apply_wifi(payload: &str) -> heapless::String<32> {
    // Two ways in. The setup page posts `pick`, the SSID chosen from the scan
    // list, and leaves it empty for "other"; `ssid` is the text field, which is
    // what the status page posts and what "other" falls back to.
    let mut pick_buf = [0u8; 96];
    let picked = form::text(payload, "pick", &mut pick_buf).unwrap_or("");
    let mut ssid_buf = [0u8; 96];
    let typed = form::text(payload, "ssid", &mut ssid_buf).unwrap_or("");
    let ssid = if picked.is_empty() { typed } else { picked };

    let mut result = heapless::String::new();
    let _ = result.push_str(ssid);

    state::with(|a| {
        // An empty result leaves the stored SSID alone, so saving the status
        // page without touching the field cannot unconfigure the device.
        if !ssid.is_empty() {
            a.settings.ssid = settings::Text::from_str(ssid);
        }
        a.settings_dirty = true;
    });
    // Read the passphrase separately: an empty field means "leave it alone", so
    // that saving the status page does not wipe a working passphrase.
    let mut pbuf = [0u8; 96];
    if let Some(psk) = form::text(payload, "psk", &mut pbuf) {
        if !psk.is_empty() {
            state::with(|a| a.settings.psk = settings::Text::from_str(psk));
        }
    }
    state::with(|a| {
        a.net.join_failures = 0;
        a.net.last_join_error = None;
        a.net.from_portal = true;
        a.reprovision = false;
        a.net.mode = NetMode::Connecting;
    });
    result
}
