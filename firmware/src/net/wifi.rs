//! WiFi lifecycle: provisioning access point, joining, and recovery.
//!
//! One task owns the radio and the stack's IP configuration, so the two can
//! never disagree about which network the device is on.
//!
//! Recovery is the part worth stating plainly: a device that cannot reach its
//! network must become reachable *some* way, or it is a brick with a nice
//! display. After [`MAX_JOIN_FAILURES`] attempts it brings the access point
//! back up so the credentials can be corrected — which also covers the common
//! case of the network's passphrase having changed.

use cyw43::{Control, JoinOptions, PowerManagementMode, ScanOptions};
use embassy_net::{ConfigV4, Ipv4Cidr, Stack, StaticConfigV4};
use embassy_time::{Duration, Timer, with_timeout};

use crate::scan;

use super::{AP_ADDR, AP_CHANNEL, AP_PREFIX};
use crate::settings::Text;
use crate::state::{self, NetMode};

use crate::generated::qr::SETUP_SSID;

/// Consecutive failures before falling back to the access point.
const MAX_JOIN_FAILURES: u32 = 3;

/// How long to wait for DHCP before calling the join a failure.
const DHCP_TIMEOUT: Duration = Duration::from_secs(25);

/// Pause between join attempts, so a flapping router is not hammered.
const RETRY_DELAY: Duration = Duration::from_secs(5);

/// How often the signal strength shown on the status page is refreshed.
const RSSI_INTERVAL: Duration = Duration::from_secs(30);

/// Ceiling on a scan. A passive sweep of every channel takes a few seconds;
/// this stops a radio that never reports completion from stalling setup.
const SCAN_TIMEOUT: Duration = Duration::from_secs(12);

/// 802.11 capability bit marking a network as requiring encryption.
const CAP_PRIVACY: u16 = 0x0010;

/// Owns the radio and keeps the device either joined or provisionable.
#[embassy_executor::task]
pub async fn task(control: &'static mut Control<'static>, stack: Stack<'static>) -> ! {
    control.init(crate::generated::cyw43_fw::bytes(crate::generated::cyw43_fw::CLM)).await;
    // The radio must stay responsive to incoming connections, so no deep sleep.
    control
        .set_power_management(PowerManagementMode::PowerSave)
        .await;

    loop {
        let (ssid, psk, reprovision) =
            state::with(|a| (a.settings.ssid, a.settings.psk, a.reprovision));

        if ssid.is_empty() || reprovision {
            run_ap(control, stack).await;
        } else {
            run_station(control, stack, ssid, psk).await;
        }
    }
}

/// Brings up the provisioning access point and waits for credentials.
async fn run_ap(control: &mut Control<'static>, stack: Stack<'static>) {
    // Scan before the radio is committed to running an access point, so the
    // setup page can list what is actually in range.
    scan_networks(control).await;

    defmt::info!("wifi: starting access point {=str}", SETUP_SSID);
    control.leave().await;

    // Static addressing: we are the DHCP server here, not a client.
    stack.set_config_v4(ConfigV4::Static(StaticConfigV4 {
        address: Ipv4Cidr::new(AP_ADDR, AP_PREFIX),
        gateway: Some(AP_ADDR),
        dns_servers: heapless::Vec::from_slice(&[AP_ADDR]).unwrap_or_default(),
    }));

    // Open rather than WPA2, so the join QR stays a version-2 code at two
    // pixels per module. A passphrase would push it to version 3, which is too
    // small to scan reliably on this panel. The window is a few minutes on a
    // link that carries only the credentials being submitted.
    control.start_ap_open(SETUP_SSID, AP_CHANNEL).await;

    state::with(|a| {
        a.net.mode = NetMode::Setup;
        a.net.addr = Some(Ipv4Cidr::new(AP_ADDR, AP_PREFIX));
        a.net.rssi = None;
        a.net.ssid = Text::from_str(SETUP_SSID);
        a.reprovision = false;
        a.display_dirty = true;
    });

    // Hold the access point until credentials arrive through the portal.
    loop {
        Timer::after(Duration::from_millis(500)).await;

        // A rescan is asked for from the portal, never on a timer: the radio
        // has to leave the access point's channel to sweep the others, so a
        // scan nobody asked for would drop whoever is mid-setup. Deliberately
        // without closing the access point — a second of loss is survivable for
        // the TCP connection that is about to reload the page, where a close
        // and reopen would not be.
        if state::with(|a| core::mem::take(&mut a.rescan)) {
            defmt::info!("wifi: rescan requested from the portal");
            state::with(|a| {
                a.net.last_join_error = None;
                a.net.join_failures = 0;
            });
            scan_networks(control).await;
        }

        let ready = state::with(|a| {
            a.net.mode == NetMode::Connecting && !a.settings.ssid.is_empty()
        });
        if ready {
            defmt::info!("wifi: credentials received, leaving setup mode");
            control.close_ap().await;
            Timer::after(Duration::from_millis(100)).await;
            return;
        }
    }
}

/// Fills the scan table with what is currently in range.
async fn scan_networks(control: &mut Control<'static>) {
    defmt::info!("wifi: scanning");
    scan::with(|s| s.clear());

    let found = with_timeout(SCAN_TIMEOUT, async {
        let mut scanner = control.scan(ScanOptions::default()).await;
        let mut n = 0;
        while let Some(bss) = scanner.next().await {
            let len = (bss.ssid_len as usize).min(bss.ssid.len());
            // A hidden network beacons an empty or padded SSID; there is
            // nothing useful to show for it.
            let Ok(ssid) = core::str::from_utf8(&bss.ssid[..len]) else {
                continue;
            };
            scan::with(|s| {
                s.insert(
                    ssid.trim_end_matches('\0'),
                    bss.rssi as i32,
                    bss.capability & CAP_PRIVACY != 0,
                )
            });
            n += 1;
        }
        n
    })
    .await;

    match found {
        Ok(n) => defmt::info!("wifi: scan saw {} beacons", n),
        Err(_) => defmt::warn!("wifi: scan timed out, using what was found"),
    }
}

/// Joins the configured network and stays on it until something goes wrong.
async fn run_station(
    control: &mut Control<'static>,
    stack: Stack<'static>,
    ssid: Text<32>,
    psk: Text<64>,
) {
    defmt::info!("wifi: joining {=str}", ssid.as_str());
    state::with(|a| {
        a.net.mode = NetMode::Connecting;
        a.net.ssid = ssid;
        a.net.addr = None;
        a.display_dirty = true;
    });

    let opts = if psk.is_empty() {
        JoinOptions::new_open()
    } else if psk.as_str().len() < 8 {
        defmt::warn!("wifi: passphrase too short, must be at least 8 bytes");
        control.leave().await;
        return fail(stack, ssid).await;
    } else {
        JoinOptions::new(psk.as_str().as_bytes())
    };

    if let Err(e) = control.join(ssid.as_str(), opts).await {
        defmt::warn!("wifi: join failed: {:?}", e);
        control.leave().await;
        return fail(stack, ssid).await;
    }

    // Ask for an address with our hostname set, so routers list and resolve "morrow".
    let mut dhcp_config = embassy_net::DhcpConfig::default();
    let mut hostname = heapless::String::new();
    let _ = hostname.push_str(crate::views::HOSTNAME);
    dhcp_config.hostname = Some(hostname);
    stack.set_config_v4(ConfigV4::Dhcp(dhcp_config));
    if with_timeout(DHCP_TIMEOUT, stack.wait_config_up()).await.is_err() {
        defmt::warn!("wifi: no DHCP lease within {} s", DHCP_TIMEOUT.as_secs());
        control.leave().await;
        return fail(stack, ssid).await;
    }

    let addr = stack.config_v4().map(|c| c.address);
    defmt::info!("wifi: joined, address {:?}", addr);
    let _ = stack.join_multicast_group(embassy_net::IpAddress::Ipv4(super::mdns::GROUP));
    state::with(|a| {
        a.net.mode = NetMode::Online;
        a.net.addr = addr;
        a.net.join_failures = 0;
        a.net.last_join_error = None;
        a.net.from_portal = false;
        a.display_dirty = true;
    });

    // Stay here while the link holds, refreshing signal strength and watching
    // for a request to go back to provisioning.
    loop {
        Timer::after(RSSI_INTERVAL).await;

        if state::with(|a| a.reprovision) {
            defmt::info!("wifi: setup mode requested");
            control.leave().await;
            return;
        }
        if state::with(|a| a.net.mode) == NetMode::Connecting {
            // New credentials arrived through the status page.
            defmt::info!("wifi: new credentials, rejoining");
            control.leave().await;
            return;
        }
        if !stack.is_link_up() {
            defmt::warn!("wifi: link lost");
            control.leave().await;
            return fail(stack, ssid).await;
        }

        let rssi = control.get_rssi().await;
        state::with(|a| {
            a.net.rssi = Some(rssi);
            a.net.addr = stack.config_v4().map(|c| c.address);
        });
    }
}

/// Records a failed attempt and decides whether to fall back to provisioning.
async fn fail(stack: Stack<'static>, ssid: Text<32>) {
    stack.set_config_v4(ConfigV4::None);
    let failures = state::with(|a| {
        a.net.join_failures += 1;
        a.net.mode = NetMode::Connecting;
        a.net.addr = None;
        a.net.rssi = None;
        a.net.join_failures
    });

    if failures >= MAX_JOIN_FAILURES {
        defmt::warn!(
            "wifi: {} failed attempts, falling back to setup mode",
            failures
        );
        // Credentials are kept: the access point shows them pre-filled so only
        // the passphrase needs retyping, and a transient outage that resolves
        // itself still leaves the device configured.
        state::with(|a| {
            a.reprovision = true;
            if a.net.from_portal {
                a.net.last_join_error = Some(ssid);
                a.net.from_portal = false;
            }
        });
    } else {
        Timer::after(RETRY_DELAY).await;
    }
}
