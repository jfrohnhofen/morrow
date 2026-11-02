//! mDNS responder, so the device answers to `morrow.local`.
//!
//! Enough of RFC 6762 to be found: A records for the hostname, and a
//! `_http._tcp` advertisement so it also shows up in service browsers. No
//! probing, no conflict resolution, no caching — a single fixed name on a home
//! network does not need them.
//!
//! Only runs once joined to a real network; while the provisioning access point
//! is up there is nobody to answer.

use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{IpAddress, IpEndpoint, IpListenEndpoint, Ipv4Address, Stack};

use super::wire::{CLASS_IN, TYPE_A, TYPE_ANY, TYPE_PTR, TYPE_SRV, TYPE_TXT, name_matches, skip_name, write_name};
use crate::state::{self, NetMode};

/// The mDNS port.
const PORT: u16 = 5353;
/// The IPv4 mDNS multicast group.
pub const GROUP: Ipv4Address = Ipv4Address::new(224, 0, 0, 251);
/// TTL for our records, as recommended for hostname records.
const TTL: u32 = 120;
/// Port the HTTP server listens on, advertised in the SRV record.
const HTTP_PORT: u16 = 80;

/// Joins the multicast group and answers queries.
#[embassy_executor::task]
pub async fn task(stack: Stack<'static>, hostname: &'static str) -> ! {
    if let Err(e) = stack.join_multicast_group(IpAddress::Ipv4(GROUP)) {
        defmt::warn!("mdns: could not join group: {:?}", e);
    }

    let mut rx_meta = [PacketMetadata::EMPTY; 8];
    let mut rx = [0u8; 768];
    let mut tx_meta = [PacketMetadata::EMPTY; 8];
    let mut tx = [0u8; 768];
    let mut sock = UdpSocket::new(stack, &mut rx_meta, &mut rx, &mut tx_meta, &mut tx);

    if let Err(e) = sock.bind(IpListenEndpoint { addr: None, port: PORT }) {
        defmt::error!("mdns: bind failed: {:?}", e);
        core::future::pending::<()>().await;
    }
    defmt::info!("mdns: answering for {}.local", hostname);

    let mut buf = [0u8; 768];
    let mut out = [0u8; 768];
    loop {
        let Ok((n, src)) = sock.recv_from(&mut buf).await else {
            continue;
        };
        let (mode, addr) = state::with(|a| (a.net.mode, a.net.addr.map(|c| c.address())));
        if mode != NetMode::Online {
            continue;
        }
        let Some(addr) = addr else {
            continue;
        };
        if let Some(len) = respond(&buf[..n], &mut out, hostname, addr) {
            defmt::info!("mdns: answering for {} -> {:?}", hostname, addr);
            let _ = sock.send_to(&out[..len], src).await;
            if src.endpoint.port == PORT {
                let dst = IpEndpoint::new(IpAddress::Ipv4(GROUP), PORT);
                let _ = sock.send_to(&out[..len], dst).await;
            }
        }
    }
}

/// Builds an answer if the query is for one of our names.
fn respond(req: &[u8], out: &mut [u8], hostname: &str, addr: Ipv4Address) -> Option<usize> {
    if req.len() < 12 {
        return None;
    }
    // Ignore responses; we only answer questions.
    if u16::from_be_bytes([req[2], req[3]]) & 0x8000 != 0 {
        return None;
    }
    let qdcount = u16::from_be_bytes([req[4], req[5]]);
    if qdcount == 0 {
        return None;
    }

    // Decide what was asked across all questions before building anything.
    let mut want_a = false;
    let mut want_svc = false;
    let mut pos = 12;
    for _ in 0..qdcount {
        let qstart = pos;
        let qend = skip_name(req, pos)?;
        let qtype = u16::from_be_bytes([*req.get(qend)?, *req.get(qend + 1)?]);
        pos = qend + 4;

        if name_matches(req, qstart, &[hostname, "local"])
            && (qtype == TYPE_A || qtype == TYPE_ANY)
        {
            want_a = true;
        }
        if name_matches(req, qstart, &["_http", "_tcp", "local"])
            && (qtype == TYPE_PTR || qtype == TYPE_ANY)
        {
            want_svc = true;
        }
        if name_matches(req, qstart, &["_services", "_dns-sd", "_udp", "local"]) {
            want_svc = true;
        }
    }
    if !want_a && !want_svc {
        return None;
    }

    out[..12].fill(0);
    out[2] = 0x84; // response, authoritative
    let mut answers = 0u16;
    let mut n = 12;

    // Cache-flush bit: we are authoritative for these names, so listeners
    // should replace rather than accumulate.
    const FLUSH: u16 = 0x8000;

    let record = |out: &mut [u8],
                      n: &mut usize,
                      labels: &[&str],
                      rtype: u16,
                      rdata: &dyn Fn(&mut [u8], usize) -> Option<usize>|
     -> Option<()> {
        *n += write_name(out, *n, labels)?;
        out.get_mut(*n..*n + 2)?.copy_from_slice(&rtype.to_be_bytes());
        *n += 2;
        out.get_mut(*n..*n + 2)?
            .copy_from_slice(&(CLASS_IN | FLUSH).to_be_bytes());
        *n += 2;
        out.get_mut(*n..*n + 4)?.copy_from_slice(&TTL.to_be_bytes());
        *n += 4;
        let len_at = *n;
        *n += 2;
        let written = rdata(out, *n)?;
        out.get_mut(len_at..len_at + 2)?
            .copy_from_slice(&(written as u16).to_be_bytes());
        *n += written;
        Some(())
    };

    if want_a {
        record(out, &mut n, &[hostname, "local"], TYPE_A, &|o, at| {
            o.get_mut(at..at + 4)?.copy_from_slice(&addr.octets());
            Some(4)
        })?;
        answers += 1;
    }

    if want_svc {
        // PTR: the service type points at our instance.
        record(out, &mut n, &["_http", "_tcp", "local"], TYPE_PTR, &|o, at| {
            write_name(o, at, &[hostname, "_http", "_tcp", "local"])
        })?;
        answers += 1;

        // SRV: where the instance actually lives.
        record(
            out,
            &mut n,
            &[hostname, "_http", "_tcp", "local"],
            TYPE_SRV,
            &|o, at| {
                o.get_mut(at..at + 2)?.copy_from_slice(&0u16.to_be_bytes()); // priority
                o.get_mut(at + 2..at + 4)?.copy_from_slice(&0u16.to_be_bytes()); // weight
                o.get_mut(at + 4..at + 6)?
                    .copy_from_slice(&HTTP_PORT.to_be_bytes());
                Some(6 + write_name(o, at + 6, &[hostname, "local"])?)
            },
        )?;
        answers += 1;

        // TXT: one empty string, which is the minimum a valid TXT record needs.
        record(
            out,
            &mut n,
            &[hostname, "_http", "_tcp", "local"],
            TYPE_TXT,
            &|o, at| {
                *o.get_mut(at)? = 0;
                Some(1)
            },
        )?;
        answers += 1;

        if want_a {
            // Saves the querier a second round trip.
            record(out, &mut n, &[hostname, "local"], TYPE_A, &|o, at| {
                o.get_mut(at..at + 4)?.copy_from_slice(&addr.octets());
                Some(4)
            })
            .ok()?;
        }
    }

    out[6..8].copy_from_slice(&answers.to_be_bytes());
    Some(n)
}

/// Lets `Option` chain where a `Result`-shaped helper is more convenient.
trait OkExt {
    fn ok(self) -> Option<()>;
}

impl OkExt for Option<()> {
    fn ok(self) -> Option<()> {
        self
    }
}
