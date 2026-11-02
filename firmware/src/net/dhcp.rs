//! A DHCP server for the provisioning access point.
//!
//! `embassy-net` ships a DHCP *client* only, and a phone will not stay on a
//! network that cannot address it, so the server half is here.
//!
//! What it offers matters as much as the address: it points DNS at the device
//! itself, which is what lets [`super::dns`] hijack the captive-portal probes.
//! It also sends option 114 (RFC 8910), which hands modern clients the portal
//! URL directly instead of making them infer one from a hijacked probe.
//!
//! Like the DNS responder, every reply is gated on [`NetMode::Setup`]: running
//! a DHCP server on someone else's network would be actively harmful.

use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{IpEndpoint, IpListenEndpoint, Ipv4Address, Stack};
use embassy_time::Instant;

use crate::state::{self, NetMode};

/// Server port.
const SERVER_PORT: u16 = 67;
/// Client port.
const CLIENT_PORT: u16 = 68;
/// Marks the start of the options area in a BOOTP packet.
const MAGIC: [u8; 4] = [99, 130, 83, 99];
/// Smallest BOOTP packet worth parsing.
const MIN_LEN: usize = 240;

/// Lease length. Short, because the access point is short-lived by design.
const LEASE_SECS: u32 = 600;

/// How many clients can hold a lease at once.
const POOL: usize = 8;
/// Host part of the first address handed out.
const POOL_START: u8 = 50;

/// One handed-out address.
#[derive(Copy, Clone)]
struct Lease {
    mac: [u8; 6],
    host: u8,
    at: Instant,
}

/// Serves DHCP until the task is cancelled.
#[embassy_executor::task]
pub async fn task(stack: Stack<'static>, server: Ipv4Address, portal_url: &'static str) -> ! {
    let mut rx_meta = [PacketMetadata::EMPTY; 8];
    let mut rx = [0u8; 1024];
    let mut tx_meta = [PacketMetadata::EMPTY; 8];
    let mut tx = [0u8; 1024];
    let mut sock = UdpSocket::new(stack, &mut rx_meta, &mut rx, &mut tx_meta, &mut tx);

    if let Err(e) = sock.bind(IpListenEndpoint { addr: None, port: SERVER_PORT }) {
        defmt::error!("dhcp: bind failed: {:?}", e);
        core::future::pending::<()>().await;
    }
    defmt::info!("dhcp: server on :{}", SERVER_PORT);

    let mut leases: [Option<Lease>; POOL] = [None; POOL];
    let mut buf = [0u8; 1024];
    let mut out = [0u8; 548];

    loop {
        let Ok((n, _)) = sock.recv_from(&mut buf).await else {
            continue;
        };
        if state::with(|a| a.net.mode) != NetMode::Setup {
            continue;
        }
        let Some(len) = handle(&buf[..n], &mut out, server, portal_url, &mut leases) else {
            continue;
        };
        // Replies go to the broadcast address: the client has no address yet,
        // so it cannot be reached by unicast.
        let dst = IpEndpoint::new(Ipv4Address::BROADCAST.into(), CLIENT_PORT);
        if let Err(e) = sock.send_to(&out[..len], dst).await {
            defmt::warn!("dhcp: send failed: {:?}", e);
        }
    }
}

/// Finds or creates a lease for `mac`, returning its host octet.
fn lease_for(leases: &mut [Option<Lease>; POOL], mac: [u8; 6]) -> u8 {
    if let Some(l) = leases.iter().flatten().find(|l| l.mac == mac) {
        return l.host;
    }
    // Prefer a free slot; otherwise recycle the oldest, since a full pool on a
    // setup access point means stale entries rather than real contention.
    let idx = leases
        .iter()
        .position(Option::is_none)
        .unwrap_or_else(|| {
            let mut oldest = 0;
            for (i, l) in leases.iter().enumerate() {
                if let (Some(a), Some(b)) = (l, &leases[oldest]) {
                    if a.at < b.at {
                        oldest = i;
                    }
                }
            }
            oldest
        });
    let host = POOL_START + idx as u8;
    leases[idx] = Some(Lease {
        mac,
        host,
        at: Instant::now(),
    });
    host
}

/// Parses a request and builds the matching reply.
fn handle(
    req: &[u8],
    out: &mut [u8],
    server: Ipv4Address,
    portal_url: &str,
    leases: &mut [Option<Lease>; POOL],
) -> Option<usize> {
    if req.len() < MIN_LEN || req[0] != 1 || req.get(236..240)? != MAGIC {
        return None;
    }

    // Option 53 carries the message type.
    let mut msg_type = 0u8;
    let mut i = 240;
    while i < req.len() {
        match req[i] {
            0 => {
                i += 1;
                continue;
            }
            255 => break,
            _ => {}
        }
        let len = *req.get(i + 1)? as usize;
        if req[i] == 53 {
            msg_type = *req.get(i + 2)?;
        }
        i += 2 + len;
    }

    // 1 = DISCOVER answered with 2 = OFFER; 3 = REQUEST answered with 5 = ACK.
    let reply_type = match msg_type {
        1 => 2,
        3 => 5,
        _ => return None,
    };

    let mut mac = [0u8; 6];
    mac.copy_from_slice(req.get(28..34)?);
    let host = lease_for(leases, mac);
    let offered = Ipv4Address::new(
        server.octets()[0],
        server.octets()[1],
        server.octets()[2],
        host,
    );

    out[..MIN_LEN].fill(0);
    out[0] = 2; // BOOTREPLY
    out[1] = 1; // Ethernet
    out[2] = 6; // MAC length
    out[4..8].copy_from_slice(req.get(4..8)?); // transaction id
    out[10..12].copy_from_slice(req.get(10..12)?); // flags
    out[16..20].copy_from_slice(&offered.octets()); // yiaddr
    out[20..24].copy_from_slice(&server.octets()); // siaddr
    out[28..34].copy_from_slice(&mac);
    out[236..240].copy_from_slice(&MAGIC);

    let mut n = 240;
    let put = |out: &mut [u8], n: &mut usize, code: u8, val: &[u8]| -> Option<()> {
        *out.get_mut(*n)? = code;
        *out.get_mut(*n + 1)? = val.len() as u8;
        out.get_mut(*n + 2..*n + 2 + val.len())?.copy_from_slice(val);
        *n += 2 + val.len();
        Some(())
    };

    put(out, &mut n, 53, &[reply_type])?;
    put(out, &mut n, 54, &server.octets())?; // server identifier
    put(out, &mut n, 51, &LEASE_SECS.to_be_bytes())?; // lease time
    put(out, &mut n, 1, &[255, 255, 255, 0])?; // subnet mask
    put(out, &mut n, 3, &server.octets())?; // router
    // DNS points at us, which is what lets the portal hijack probe lookups.
    put(out, &mut n, 6, &server.octets())?;
    // RFC 8910: hand the portal URL over directly. Clients that honour it skip
    // the probe-and-redirect dance entirely.
    put(out, &mut n, 114, portal_url.as_bytes())?;
    *out.get_mut(n)? = 255; // end
    n += 1;

    // Pad to the BOOTP minimum; some clients discard anything shorter.
    while n < 300 {
        out[n] = 0;
        n += 1;
    }
    Some(n)
}
