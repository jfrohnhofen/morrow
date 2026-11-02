//! Minimal SNTP client.
//!
//! NTP carries UTC and nothing else — there is no timezone in the protocol — so
//! this returns Unix seconds and [`crate::tz`] does the rest.
//!
//! Only the bare client mode of RFC 4330 is implemented: one packet out, one
//! packet in, read the transmit timestamp. No clock discipline, no round-trip
//! correction. An alarm clock cares about seconds, the RP2040 crystal drifts
//! about 1.7 s a day, and resyncing hourly keeps the error far below anything a
//! sunrise would reveal.

use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{IpEndpoint, Stack};
use embassy_time::{Duration, with_timeout};

/// Seconds between the NTP epoch (1900-01-01) and the Unix epoch.
const NTP_TO_UNIX: u64 = 2_208_988_800;

/// NTP's 32-bit second counter wraps in 2036; past that, timestamps restart
/// from zero and belong to the next era.
const ERA: u64 = 1 << 32;

/// A timestamp below this is read as era 1 rather than as the year 1900.
///
/// Anything genuinely from before 2000 is a broken server, and treating it as
/// era 1 is both more useful and no less correct.
const ERA_THRESHOLD: u64 = 3_155_673_600; // 2000-01-01 in NTP seconds

/// How long to wait for a reply before giving up on this server.
const TIMEOUT: Duration = Duration::from_secs(5);

/// Queries `server` and returns the time in Unix seconds.
///
/// `server` may be a hostname or a literal address. Returns `None` on any
/// failure; the caller is expected to retry later rather than treat this as
/// fatal, since a missing sync only means the clock keeps coasting.
pub async fn query(stack: Stack<'_>, server: &str, port: u16) -> Option<i64> {
    let addr = match server.parse::<core::net::Ipv4Addr>() {
        Ok(a) => embassy_net::IpAddress::Ipv4(a),
        Err(_) => {
            let found = stack
                .dns_query(server, embassy_net::dns::DnsQueryType::A)
                .await
                .ok()?;
            *found.first()?
        }
    };

    let mut rx_meta = [PacketMetadata::EMPTY; 4];
    let mut rx = [0u8; 128];
    let mut tx_meta = [PacketMetadata::EMPTY; 4];
    let mut tx = [0u8; 128];
    let mut sock = UdpSocket::new(stack, &mut rx_meta, &mut rx, &mut tx_meta, &mut tx);
    // Port 0 asks the stack for an ephemeral one.
    sock.bind(0).ok()?;

    // LI = 0 (no warning), VN = 4, Mode = 3 (client).
    let mut req = [0u8; 48];
    req[0] = 0x23;
    sock.send_to(&req, IpEndpoint::new(addr, port)).await.ok()?;

    let mut buf = [0u8; 128];
    let (n, _) = with_timeout(TIMEOUT, sock.recv_from(&mut buf)).await.ok()?.ok()?;
    if n < 48 {
        defmt::warn!("ntp: short reply ({} bytes)", n);
        return None;
    }

    // Mode 4 is a server reply; anything else is not an answer to our question.
    let mode = buf[0] & 0x07;
    if mode != 4 && mode != 5 {
        defmt::warn!("ntp: unexpected mode {}", mode);
        return None;
    }
    // Stratum 0 means "kiss of death" — the server is refusing, not answering.
    if buf[1] == 0 {
        defmt::warn!("ntp: kiss-of-death packet, ignoring server");
        return None;
    }

    // Transmit timestamp: seconds at offset 40, fraction at 44 (discarded).
    let secs = u32::from_be_bytes([buf[40], buf[41], buf[42], buf[43]]) as u64;
    if secs == 0 {
        return None;
    }
    let secs = if secs < ERA_THRESHOLD { secs + ERA } else { secs };
    Some((secs - NTP_TO_UNIX) as i64)
}
