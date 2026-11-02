//! Persistent settings, stored in the top 8 KB of flash.
//!
//! Two 4 KB sectors are used alternately: the newer valid record wins, and a
//! write always targets the older one. Losing power mid-write therefore costs
//! at most the new record, never the old one.
//!
//! `memory.x` shortens the `FLASH` region by the same 8 KB, so if the firmware
//! ever grows into this area the link fails rather than the settings being
//! silently overwritten at runtime.

use defmt::Format;
use embassy_rp::flash::ERASE_SIZE;
use embedded_storage::nor_flash::{NorFlash, ReadNorFlash};

use crate::tz::{MAX_RULE_LEN, TzRule};

/// Total flash fitted to the Pico W.
pub const FLASH_SIZE: usize = 2 * 1024 * 1024;

/// Offset of the first settings sector.
const SLOT_A: u32 = (FLASH_SIZE - 2 * ERASE_SIZE) as u32;
/// Offset of the second settings sector.
const SLOT_B: u32 = (FLASH_SIZE - ERASE_SIZE) as u32;

/// Size of a stored record. Comfortably under the 4 KB erase granularity.
const RECORD: usize = 256;
/// Marks a sector as holding a record this firmware understands.
const MAGIC: &[u8; 4] = b"MRW1";

/// Longest WiFi SSID, per IEEE 802.11.
pub const MAX_SSID: usize = 32;
/// Longest WPA passphrase.
pub const MAX_PSK: usize = 64;
/// Longest IANA zone name kept for display.
pub const MAX_TZ_NAME: usize = 32;
/// Maximum number of LEDs supported by the frame buffer.
pub const MAX_LEDS: u16 = 1000;

/// How the alarm repeats. Values are weekday bitmasks, bit 0 being Monday, so
/// they can be tested directly against [`crate::tz::DateTime::weekday`].
#[derive(Copy, Clone, PartialEq, Eq, Format)]
#[repr(u8)]
pub enum Mode {
    /// Never fires.
    Off = 0,
    /// Monday through Friday.
    Weekdays = 1,
    /// Saturday and Sunday.
    Weekends = 2,
    /// Every day.
    EveryDay = 3,
}

impl Mode {
    /// All modes, in the order the encoder cycles through them.
    pub const ALL: [Mode; 4] = [Mode::Off, Mode::Weekdays, Mode::Weekends, Mode::EveryDay];

    /// The weekday bitmask this mode fires on.
    pub fn mask(self) -> u8 {
        match self {
            Mode::Off => 0b0000000,
            Mode::Weekdays => 0b0011111,
            Mode::Weekends => 0b1100000,
            Mode::EveryDay => 0b1111111,
        }
    }

    /// Does the alarm fire on this weekday? `weekday` is 0 for Monday.
    pub fn fires_on(self, weekday: u8) -> bool {
        self.mask() & (1 << weekday) != 0
    }

    /// Human-readable name, as shown in the web UI.
    pub fn label(self) -> &'static str {
        match self {
            Mode::Off => "off",
            Mode::Weekdays => "weekdays",
            Mode::Weekends => "weekends",
            Mode::EveryDay => "every day",
        }
    }

    /// Builds a mode from its stored index, falling back to [`Mode::Off`].
    pub fn from_index(i: u8) -> Mode {
        *Mode::ALL.get(i as usize).unwrap_or(&Mode::Off)
    }

    /// Index of this mode within [`Mode::ALL`].
    pub fn index(self) -> u8 {
        self as u8
    }
}

/// A short inline string, so settings need no allocator.
#[derive(Copy, Clone)]
pub struct Text<const N: usize> {
    buf: [u8; N],
    len: u8,
}

impl<const N: usize> Text<N> {
    /// An empty string.
    pub const fn new() -> Self {
        Text {
            buf: [0; N],
            len: 0,
        }
    }

    /// Builds from a `&str` in a const context, truncating at `N` bytes.
    ///
    /// Byte-truncating rather than char-truncating, since `const fn` cannot ask
    /// about character boundaries. Only used for ASCII literals in defaults.
    pub const fn literal(s: &str) -> Self {
        let b = s.as_bytes();
        let mut buf = [0u8; N];
        let mut i = 0;
        while i < b.len() && i < N {
            buf[i] = b[i];
            i += 1;
        }
        Text { buf, len: i as u8 }
    }

    /// Builds from a `&str`, truncating at `N` bytes on a character boundary.
    pub fn from_str(s: &str) -> Self {
        let mut out = Text::new();
        out.set(s);
        out
    }

    /// Replaces the contents, truncating at `N` bytes on a character boundary.
    pub fn set(&mut self, s: &str) {
        let mut end = s.len().min(N);
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        self.buf[..end].copy_from_slice(&s.as_bytes()[..end]);
        self.len = end as u8;
    }

    /// Borrows the contents.
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len as usize]).unwrap_or("")
    }

    /// Is the string empty?
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl<const N: usize> Default for Text<N> {
    fn default() -> Self {
        Text::new()
    }
}

impl<const N: usize> Format for Text<N> {
    fn format(&self, f: defmt::Formatter) {
        defmt::write!(f, "{}", self.as_str())
    }
}

/// Everything that survives a reboot.
#[derive(Copy, Clone, Format)]
pub struct Settings {
    /// Alarm hour, 0..=23.
    pub hour: u8,
    /// Alarm minute, 0..=59.
    pub minute: u8,
    /// Which days the alarm fires on.
    pub mode: Mode,
    /// How long the sunrise takes to reach full brightness, in seconds.
    pub sunrise_secs: u32,
    /// How long to hold at full brightness afterwards, in seconds.
    ///
    /// Zero reproduces the MicroPython behaviour exactly: the lamp switches off
    /// the moment the ramp tops out.
    pub hold_secs: u32,
    /// IANA zone name. Display only — [`Settings::tz_rule`] is what is applied.
    pub tz_name: Text<MAX_TZ_NAME>,
    /// POSIX `TZ` rule actually used to convert UTC to local time.
    pub tz_rule: Text<MAX_RULE_LEN>,
    /// SSID of the network to join, empty if not yet provisioned.
    pub ssid: Text<MAX_SSID>,
    /// Passphrase for that network, empty for an open network.
    pub psk: Text<MAX_PSK>,
    /// Number of LEDs in the WS2812 strip.
    pub led_count: u16,
}

impl Default for Settings {
    fn default() -> Self {
        Settings::DEFAULT
    }
}

impl Settings {
    /// Factory settings, used on a blank or unreadable flash.
    pub const DEFAULT: Settings = Settings {
        hour: 0,
        minute: 0,
        mode: Mode::Off,
        // The MicroPython firmware ramped over roughly 28 minutes as a side
        // effect of its tick rate rather than by choice. Rounded off here, and
        // now a real setting.
        sunrise_secs: 30 * 60,
        // Zero reproduces the original exactly: the lamp cuts out the instant
        // the ramp tops out, rather than staying lit.
        hold_secs: 0,
        tz_name: Text::literal("Europe/Berlin"),
        tz_rule: Text::literal("CET-1CEST,M3.5.0,M10.5.0/3"),
        ssid: Text::new(),
        psk: Text::new(),
        led_count: MAX_LEDS,
    };

    /// Parses [`Settings::tz_rule`], falling back to UTC if it is malformed.
    pub fn tz(&self) -> TzRule {
        TzRule::parse(self.tz_rule.as_str()).unwrap_or(TzRule::UTC)
    }

    /// Clamps every field into range, so a bad record cannot wedge the UI.
    fn sanitize(&mut self) {
        self.hour %= 24;
        self.minute %= 60;
        self.sunrise_secs = self.sunrise_secs.clamp(60, 4 * 3600);
        self.hold_secs = self.hold_secs.min(4 * 3600);
        self.led_count = self.led_count.clamp(1, MAX_LEDS);
        if TzRule::parse(self.tz_rule.as_str()).is_none() {
            defmt::warn!("settings: unparseable tz rule, falling back to UTC");
            self.tz_name = Text::from_str("UTC");
            self.tz_rule = Text::from_str("UTC0");
        }
    }

    /// Packs into the on-flash record layout.
    fn encode(&self) -> [u8; RECORD] {
        let mut b = [0u8; RECORD];
        b[12] = self.hour;
        b[13] = self.minute;
        b[14] = self.mode.index();
        b[16..20].copy_from_slice(&self.sunrise_secs.to_le_bytes());
        b[20..24].copy_from_slice(&self.hold_secs.to_le_bytes());

        let mut put = |at: usize, cap: usize, t: &[u8]| {
            b[at] = t.len() as u8;
            b[at + 1..at + 1 + t.len().min(cap)].copy_from_slice(&t[..t.len().min(cap)]);
        };
        put(24, MAX_TZ_NAME, self.tz_name.as_str().as_bytes());
        put(57, MAX_RULE_LEN, self.tz_rule.as_str().as_bytes());
        put(106, MAX_SSID, self.ssid.as_str().as_bytes());
        put(139, MAX_PSK, self.psk.as_str().as_bytes());
        b[204..206].copy_from_slice(&self.led_count.to_le_bytes());

        b[..4].copy_from_slice(MAGIC);
        b
    }

    /// Unpacks a record, returning `None` if the magic or checksum is wrong.
    fn decode(b: &[u8]) -> Option<(u32, Settings)> {
        if b.len() < RECORD || &b[..4] != MAGIC {
            return None;
        }
        let seq = u32::from_le_bytes(b[4..8].try_into().ok()?);
        let want = u32::from_le_bytes(b[8..12].try_into().ok()?);
        if crc32(&b[12..RECORD]) != want {
            return None;
        }

        let get = |at: usize, cap: usize| -> &str {
            let n = (b[at] as usize).min(cap);
            core::str::from_utf8(&b[at + 1..at + 1 + n]).unwrap_or("")
        };

        let raw_leds = u16::from_le_bytes(b[204..206].try_into().ok()?);
        let led_count = if raw_leds == 0 { MAX_LEDS } else { raw_leds };

        let mut s = Settings {
            hour: b[12],
            minute: b[13],
            mode: Mode::from_index(b[14]),
            sunrise_secs: u32::from_le_bytes(b[16..20].try_into().ok()?),
            hold_secs: u32::from_le_bytes(b[20..24].try_into().ok()?),
            tz_name: Text::from_str(get(24, MAX_TZ_NAME)),
            tz_rule: Text::from_str(get(57, MAX_RULE_LEN)),
            ssid: Text::from_str(get(106, MAX_SSID)),
            psk: Text::from_str(get(139, MAX_PSK)),
            led_count,
        };
        s.sanitize();
        Some((seq, s))
    }
}

/// Reads whichever slot holds the newer record.
///
/// Returns its sequence number and contents, or `(0, defaults)` on a blank or
/// corrupt flash — which is also what a first boot looks like.
pub fn load<F: ReadNorFlash>(flash: &mut F) -> (u32, Settings) {
    let mut best: Option<(u32, Settings)> = None;

    for slot in [SLOT_A, SLOT_B] {
        let mut buf = [0u8; RECORD];
        if flash.read(slot, &mut buf).is_err() {
            continue;
        }
        if let Some((seq, s)) = Settings::decode(&buf) {
            // Wrapping comparison, so the counter rolling over does not make
            // the older record look newer.
            if best.is_none_or(|(b, _)| seq.wrapping_sub(b) < u32::MAX / 2) {
                best = Some((seq, s));
            }
        }
    }

    match best {
        Some((seq, s)) => {
            defmt::info!("settings: loaded record seq={}", seq);
            (seq, s)
        }
        None => {
            defmt::info!("settings: no valid record, using defaults");
            (0, Settings::DEFAULT)
        }
    }
}

/// Writes `settings` to whichever slot is currently stale.
///
/// Returns the new sequence number, or `None` if the write failed. Erasing and
/// programming 4 KB runs with interrupts masked and execute-in-place suspended,
/// so this is only called when something actually changed.
pub fn save<F: NorFlash>(flash: &mut F, seq: u32, settings: &Settings) -> Option<u32> {
    let next = seq.wrapping_add(1);
    // Even sequence numbers live in A, odd in B, so a write always lands on the
    // slot that is not currently authoritative. Losing power mid-write costs
    // the new record, never the old one.
    let slot = if next.is_multiple_of(2) { SLOT_A } else { SLOT_B };

    let mut rec = settings.encode();
    rec[4..8].copy_from_slice(&next.to_le_bytes());
    let sum = crc32(&rec[12..RECORD]);
    rec[8..12].copy_from_slice(&sum.to_le_bytes());

    if flash.erase(slot, slot + ERASE_SIZE as u32).is_err() {
        defmt::error!("settings: erase failed");
        return None;
    }
    if flash.write(slot, &rec).is_err() {
        defmt::error!("settings: write failed");
        return None;
    }
    defmt::info!("settings: saved record seq={}", next);
    Some(next)
}

/// CRC-32 (IEEE 802.3), computed a bit at a time.
///
/// Also used to check incoming firmware images, hence `pub`. Feed it in one
/// call, or use [`crc32_update`] to accumulate across chunks.
///
/// A table would be faster but this runs a few hundred bytes on a settings
/// write, far off any hot path, and 1 KB of flash is better spent elsewhere.
pub fn crc32(data: &[u8]) -> u32 {
    !crc32_update(0xFFFF_FFFF, data)
}

/// Folds `data` into a running CRC-32 register.
///
/// Start from `0xFFFF_FFFF` and invert the result to finish — the streaming
/// form of [`crc32`], for data that arrives a sector at a time.
pub fn crc32_update(mut crc: u32, data: &[u8]) -> u32 {
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    crc
}
