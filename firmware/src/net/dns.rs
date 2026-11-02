//! Captive-portal DNS responder.
//!
//! Answers every A query with the device's own address so that whatever name a
//! phone probes — `captive.apple.com`, `connectivitycheck.gstatic.com`,
//! `msftconnecttest.com` — resolves here and the HTTP server gets to answer.
//! That is what makes the setup page open by itself after joining.
//!
//! This only runs while the provisioning access point is up. Answering every
//! name on a network you do not own would be hostile, so every reply is gated
//! on [`NetMode::Setup`].

use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{IpListenEndpoint, Ipv4Address, Stack};

use super::wire::{CLASS_IN, TYPE_A, TYPE_ANY, skip_name};
use crate::state::{self, NetMode};

/// Standard DNS port.
const PORT: u16 = 53;

/// Short TTL: the answer is a lie that should expire quickly once the device is
/// off the air.
const TTL: u32 = 10;

/// Serves DNS until the task is cancelled.
#[embassy_executor::task]
pub async fn task(stack: Stack<'static>, addr: Ipv4Address) -> ! {
    let mut rx_meta = [PacketMetadata::EMPTY; 8];
    let mut rx = [0u8; 512];
    let mut tx_meta = [PacketMetadata::EMPTY; 8];
    let mut tx = [0u8; 512];
    let mut sock = UdpSocket::new(stack, &mut rx_meta, &mut rx, &mut tx_meta, &mut tx);

    if let Err(e) = sock.bind(IpListenEndpoint { addr: None, port: PORT }) {
        defmt::error!("dns: bind failed: {:?}", e);
        // Nothing to retry against; park rather than spin.
        core::future::pending::<()>().await;
    }
    defmt::info!("dns: captive responder on :{}", PORT);

    let mut buf = [0u8; 512];
    let mut out = [0u8; 512];
    loop {
        let Ok((n, from)) = sock.recv_from(&mut buf).await else {
            continue;
        };
        // Only hijack names while we are the access point.
        if state::with(|a| a.net.mode) != NetMode::Setup {
            continue;
        }
        if let Some(len) = respond(&buf[..n], &mut out, addr) {
            let _ = sock.send_to(&out[..len], from).await;
        }
    }
}

/// Builds a reply pointing every queried name at `addr`.
///
/// Returns `None` for anything that is not a single-question standard query,
/// which is all a captive portal ever needs to handle.
fn respond(req: &[u8], out: &mut [u8], addr: Ipv4Address) -> Option<usize> {
    if req.len() < 12 {
        return None;
    }
    // Ignore responses, and anything that is not a standard query.
    let flags = u16::from_be_bytes([req[2], req[3]]);
    if flags & 0x8000 != 0 || flags & 0x7800 != 0 {
        return None;
    }
    if u16::from_be_bytes([req[4], req[5]]) != 1 {
        return None;
    }

    let qend = skip_name(req, 12)?;
    let qtype = u16::from_be_bytes([*req.get(qend)?, *req.get(qend + 1)?]);
    let qclass = u16::from_be_bytes([*req.get(qend + 2)?, *req.get(qend + 3)?]);
    let qlen = qend + 4;
    if qclass != CLASS_IN {
        return None;
    }

    // Copy the header and question through unchanged.
    out.get_mut(..qlen)?.copy_from_slice(req.get(..qlen)?);
    // Response, authoritative, recursion available.
    out[2] = 0x85;
    out[3] = 0x80;

    // Only A records get an answer. Returning an empty NOERROR for AAAA is what
    // pushes a dual-stack client to try IPv4, which is the address we have.
    if qtype != TYPE_A && qtype != TYPE_ANY {
        out[6] = 0;
        out[7] = 0;
        return Some(qlen);
    }

    out[6] = 0;
    out[7] = 1; // one answer

    let mut n = qlen;
    // Point back at the question's name rather than repeating it.
    out.get_mut(n..n + 2)?.copy_from_slice(&[0xC0, 0x0C]);
    n += 2;
    out.get_mut(n..n + 2)?.copy_from_slice(&TYPE_A.to_be_bytes());
    n += 2;
    out.get_mut(n..n + 2)?.copy_from_slice(&CLASS_IN.to_be_bytes());
    n += 2;
    out.get_mut(n..n + 4)?.copy_from_slice(&TTL.to_be_bytes());
    n += 4;
    out.get_mut(n..n + 2)?.copy_from_slice(&4u16.to_be_bytes());
    n += 2;
    out.get_mut(n..n + 4)?.copy_from_slice(&addr.octets());
    n += 4;
    Some(n)
}
