//! Results of the last WiFi scan.
//!
//! Kept out of [`crate::state::App`] on purpose: that struct is copied whole,
//! ten times a second, by the display task, and this table is several hundred
//! bytes that no screen reads. It is only wanted by the provisioning page.

use core::cell::RefCell;

use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;

use crate::settings::Text;

/// How many networks the setup page offers.
///
/// Enough to cover a normal home; more would just make the list a scroll.
pub const MAX: usize = 8;

/// One network seen by a scan.
#[derive(Copy, Clone)]
pub struct Entry {
    /// Advertised SSID.
    pub ssid: Text<32>,
    /// Signal strength in dBm.
    pub rssi: i32,
    /// Does joining need a passphrase?
    pub secured: bool,
}

/// The strongest networks from the last scan, in descending signal order.
pub struct Scan {
    /// Networks found, best first.
    pub entries: [Option<Entry>; MAX],
}

impl Scan {
    const fn new() -> Scan {
        Scan {
            entries: [None; MAX],
        }
    }

    /// Empties the table, before a fresh scan fills it.
    pub fn clear(&mut self) {
        self.entries = [None; MAX];
    }

    /// Records a sighting, keeping the list sorted and deduplicated.
    ///
    /// A mesh or a dual-band router advertises the same SSID from several
    /// radios; only the strongest is worth showing, since that is the one the
    /// client will pick anyway.
    pub fn insert(&mut self, ssid: &str, rssi: i32, secured: bool) {
        if ssid.is_empty() {
            return;
        }

        // Already known? Keep whichever sighting was stronger.
        for e in self.entries.iter_mut().flatten() {
            if e.ssid.as_str() == ssid {
                if rssi > e.rssi {
                    e.rssi = rssi;
                    e.secured = secured;
                    self.sort();
                }
                return;
            }
        }

        let entry = Entry {
            ssid: Text::from_str(ssid),
            rssi,
            secured,
        };

        if let Some(slot) = self.entries.iter_mut().find(|s| s.is_none()) {
            *slot = Some(entry);
        } else {
            // Table is full: displace the weakest, but only if this beats it.
            let weakest = self
                .entries
                .iter_mut()
                .min_by_key(|s| s.map(|e| e.rssi).unwrap_or(i32::MIN));
            if let Some(slot) = weakest {
                if slot.is_none_or(|e| rssi > e.rssi) {
                    *slot = Some(entry);
                }
            }
        }
        self.sort();
    }

    /// Orders by descending signal, with empty slots last.
    fn sort(&mut self) {
        self.entries
            .sort_unstable_by_key(|s| core::cmp::Reverse(s.map(|e| e.rssi).unwrap_or(i32::MIN)));
    }
}

/// The last scan's results.
static SCAN: Mutex<CriticalSectionRawMutex, RefCell<Scan>> = Mutex::new(RefCell::new(Scan::new()));

/// Runs `f` with exclusive access to the scan results.
pub fn with<R>(f: impl FnOnce(&mut Scan) -> R) -> R {
    SCAN.lock(|cell| f(&mut cell.borrow_mut()))
}
