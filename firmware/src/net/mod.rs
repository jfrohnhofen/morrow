//! Networking: WiFi, time, and the services the setup flow needs.
//!
//! The device runs in one of two shapes, chosen by [`wifi::task`]:
//!
//! * **Provisioning** — a `morrow-setup` access point with a static address, a
//!   DHCP server, a DNS responder that hijacks every name, and the HTTP server
//!   showing the setup form. Together those make a captive portal that opens
//!   itself when a phone joins.
//! * **Joined** — DHCP client on the home network, mDNS answering for
//!   `morrow.local`, NTP keeping the clock honest, and the same HTTP server
//!   showing status and settings.
//!
//! The DHCP and DNS servers stay bound in both shapes but refuse to answer
//! outside provisioning, so the device never acts as a rogue server on a
//! network it merely joined.

use embassy_net::Ipv4Address;

pub mod dhcp;
pub mod dns;
pub mod http;
pub mod mdns;
pub mod ntp;
pub mod wifi;
pub mod wire;

/// Address the device takes while running the provisioning access point.
pub const AP_ADDR: Ipv4Address = Ipv4Address::new(192, 168, 4, 1);

/// Prefix length for the provisioning network.
pub const AP_PREFIX: u8 = 24;

/// Portal URL handed to clients via DHCP option 114, and printed on the panel.
pub const PORTAL_URL: &str = "http://192.168.4.1/";

/// Channel the access point runs on. Six is the middle of the 2.4 GHz band and
/// as good a default as any for a radio that is only up for a few minutes.
pub const AP_CHANNEL: u8 = 6;
