#!/usr/bin/env bash
# Round-trips the on-flash settings record on the host.
#
# settings.rs is firmware-coupled through embassy-rp's flash driver, but the
# codec itself is pure. This strips the hardware half and the defmt macros, then
# exercises encode/decode, CRC rejection, and field clamping.
set -euo pipefail
W=$(mktemp -d)
trap 'rm -rf "$W"' EXIT

python3 - "$W" <<'PY'
import re, sys, pathlib
w = pathlib.Path(sys.argv[1])
s = pathlib.Path('firmware/src/settings.rs').read_text()

s = re.sub(r'^use (defmt|embassy_rp|embedded_storage)::.*\n', '', s, flags=re.M)
s = re.sub(r'^use crate::tz::.*\n', '', s, flags=re.M)
# Drop the flash-coupled half; the codec below is what is under test.
s = re.sub(r'/// Reads whichever slot holds the newer record\..*?(?=/// CRC-32)', '', s, flags=re.S)
s = s.replace('#[derive(Copy, Clone, PartialEq, Eq, Format)]', '#[derive(Copy, Clone, PartialEq, Eq, Debug)]')
s = s.replace('#[derive(Copy, Clone, Format)]', '#[derive(Copy, Clone, Debug)]')
s = re.sub(r'impl<const N: usize> Format for Text<N> \{.*?\n\}\n\n', '', s, flags=re.S)
s = re.sub(r'defmt::(warn|info|error)!\([^;]*\);', '', s, flags=re.S)
s = s.replace('use embassy_rp::flash::{Blocking, ERASE_SIZE, Flash};\n', '')
s = re.sub(r'^//!.*\n', '', s, flags=re.M)
# Expose the private codec and give Text a Debug impl for assertions.
s = s.replace('fn encode(&self)', 'pub fn encode(&self)')
s = s.replace('fn decode(b: &[u8])', 'pub fn decode(b: &[u8])')
s = s.replace('\npub fn crc32', '\nfn crc32').replace('\nfn crc32', '\npub fn crc32')
s += '''
impl<const N: usize> core::fmt::Debug for Text<N> {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "{:?}", self.as_str())
    }
}
'''
s = 'const ERASE_SIZE: usize = 4096;\nuse crate::tz::{MAX_RULE_LEN, TzRule};\n' + s
(w / 'settings.rs').write_text(s)

tz = pathlib.Path('firmware/src/tz.rs').read_text()
tz = re.sub(r'^impl defmt::Format for Abbr \{.*?\n\}\n', '', tz, flags=re.S|re.M)
tz = tz.replace('#[derive(Copy, Clone, defmt::Format)]', '#[derive(Copy, Clone)]')
tz = re.sub(r'^//!.*\n', '', tz, flags=re.M)
(w / 'tz.rs').write_text(tz)
PY

cat > "$W/main.rs" <<'RS'
mod tz { include!("tz.rs"); }
mod settings { include!("settings.rs"); }
use settings::*;

fn main() {
    let mut pass = 0; let mut fail = 0;
    let mut check = |name: &str, ok: bool| {
        if ok { pass += 1; println!("  ok    {name}"); }
        else  { fail += 1; println!("  FAIL  {name}"); }
    };

    // Round-trip every field, including maximum-length strings.
    let mut s = Settings::DEFAULT;
    s.hour = 23; s.minute = 59; s.mode = Mode::Weekends;
    s.sunrise_secs = 4 * 3600; s.hold_secs = 90 * 60;
    s.tz_name = Text::from_str("Australia/Lord_Howe");
    s.tz_rule = Text::from_str("<+1030>-10:30<+11>-11,M10.1.0,M4.1.0");
    s.ssid = Text::from_str("a-network-with-a-32-char-name-xy");
    s.psk = Text::from_str(&"p".repeat(63));
    s.led_count = 300;

    let mut rec = s.encode();
    rec[4..8].copy_from_slice(&7u32.to_le_bytes());
    let sum = crc32(&rec[12..]);
    rec[8..12].copy_from_slice(&sum.to_le_bytes());

    let (seq, back) = Settings::decode(&rec).expect("decodes");
    check("sequence survives", seq == 7);
    check("hour/minute/mode", back.hour == 23 && back.minute == 59 && back.mode == Mode::Weekends);
    check("durations", back.sunrise_secs == 4*3600 && back.hold_secs == 90*60);
    check("tz name", back.tz_name.as_str() == "Australia/Lord_Howe");
    check("tz rule (44 bytes)", back.tz_rule.as_str() == s.tz_rule.as_str());
    check("ssid at 32 bytes", back.ssid.as_str() == s.ssid.as_str());
    check("psk at 63 bytes", back.psk.as_str() == s.psk.as_str());
    check("led count survives", back.led_count == 300);

    // A single flipped bit anywhere in the payload must be caught.
    let mut caught = 0; let mut total = 0;
    for byte in 12..rec.len() {
        for bit in 0..8 {
            let mut bad = rec; bad[byte] ^= 1 << bit; total += 1;
            if Settings::decode(&bad).is_none() { caught += 1; }
        }
    }
    check(&format!("crc catches single-bit flips ({caught}/{total})"), caught == total);

    // Wrong magic is refused.
    let mut bad = rec; bad[0] = b'X';
    check("wrong magic refused", Settings::decode(&bad).is_none());

    // A blank sector (erased flash reads as 0xFF) must not decode.
    check("erased flash refused", Settings::decode(&[0xFFu8; 256]).is_none());
    check("zeroed flash refused", Settings::decode(&[0x00u8; 256]).is_none());

    // Out-of-range fields are clamped, not trusted.
    let mut wild = Settings::DEFAULT;
    wild.hour = 99; wild.minute = 200; wild.sunrise_secs = 0; wild.hold_secs = 99999; wild.led_count = 9999;
    let mut r = wild.encode();
    let c = crc32(&r[12..]); r[8..12].copy_from_slice(&c.to_le_bytes());
    let (_, got) = Settings::decode(&r).expect("decodes");
    check("hour clamped", got.hour < 24);
    check("minute clamped", got.minute < 60);
    check("sunrise floor", got.sunrise_secs >= 60);
    check("hold ceiling", got.hold_secs <= 4*3600);
    check("led count ceiling", got.led_count <= MAX_LEDS);

    // An unparseable rule falls back to UTC rather than wedging the clock.
    let mut brk = Settings::DEFAULT;
    brk.tz_rule = Text::from_str("!!! not a rule !!!");
    let mut r = brk.encode();
    let c = crc32(&r[12..]); r[8..12].copy_from_slice(&c.to_le_bytes());
    let (_, got) = Settings::decode(&r).expect("decodes");
    check("bad tz rule falls back to UTC", got.tz_rule.as_str() == "UTC0");

    println!("\n{pass} passed, {fail} failed");
    if fail > 0 { std::process::exit(1); }
}
RS
if ! rustc -O --edition 2021 -o "$W/t" "$W/main.rs" 2>"$W/err"; then grep -E '^error' -A8 "$W/err" | head -40; exit 1; fi
"$W/t"
