//! Build-time code generation.
//!
//! Three things are generated into `OUT_DIR` rather than committed:
//!
//! * `cyw43_firmware.rs` — where the WiFi blobs live in flash. They are *not*
//!   embedded: at 233 KB they would be 57% of every over-the-air update while
//!   never actually changing, so they get their own flash region and the
//!   firmware reads them from the XIP window. A manifest is written alongside
//!   so the flashing tool knows what to put where.
//! * `tz_table.rs` — the curated timezone list, with each zone's POSIX rule read
//!   out of the tzdata that `TZDIR` points at. Bumping nixpkgs refreshes the
//!   rules; no rule is ever typed by hand here.
//! * `setup_qr.rs` — the WiFi-join QR shown during setup, encoded here so the
//!   firmware carries a bitmap instead of a QR encoder.

use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// SSID of the provisioning access point. Case matters: SSIDs are compared
/// byte-for-byte, so this must match what the QR encodes exactly.
const SETUP_SSID: &str = "morrow-setup";

/// Largest QR the 64-pixel axis of the panel can show at 2 physical pixels per
/// module with a 2-module quiet zone: (25 + 4) * 2 = 58 <= 64. Version 3 would
/// need 66 and is rejected rather than silently rendered unscannably small.
const MAX_QR_MODULES: usize = 25;

/// Zones offered in the setup UI. The POSIX rule for each is read from tzdata at
/// build time, never hardcoded. Anything missing is reachable through the
/// free-form rule override in the web UI, which is also the escape hatch if a
/// government changes a rule before nixpkgs catches up.
const ZONES: &[&str] = &[
    "UTC",
    "Europe/Berlin",
    "Europe/London",
    "Europe/Dublin",
    "Europe/Lisbon",
    "Europe/Madrid",
    "Europe/Paris",
    "Europe/Amsterdam",
    "Europe/Brussels",
    "Europe/Zurich",
    "Europe/Vienna",
    "Europe/Prague",
    "Europe/Warsaw",
    "Europe/Rome",
    "Europe/Stockholm",
    "Europe/Oslo",
    "Europe/Copenhagen",
    "Europe/Helsinki",
    "Europe/Athens",
    "Europe/Istanbul",
    "Europe/Kyiv",
    "Europe/Moscow",
    "America/New_York",
    "America/Chicago",
    "America/Denver",
    "America/Los_Angeles",
    "America/Anchorage",
    "America/Sao_Paulo",
    "Asia/Jerusalem",
    "Asia/Dubai",
    "Asia/Kolkata",
    "Asia/Singapore",
    "Asia/Hong_Kong",
    "Asia/Shanghai",
    "Asia/Tokyo",
    "Asia/Seoul",
    "Australia/Perth",
    "Australia/Sydney",
    "Pacific/Auckland",
];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=memory.x");
    println!("cargo:rerun-if-env-changed=CYW43_FIRMWARE_DIR");
    println!("cargo:rerun-if-env-changed=TZDIR");

    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));

    // memory.x has to sit next to the linker's search path.
    fs::copy("memory.x", out.join("memory.x")).expect("copy memory.x");
    println!("cargo:rustc-link-search={}", out.display());

    gen_firmware_paths(&out);
    gen_tz_table(&out);
    gen_setup_qr(&out);
}

/// Start of the flash region holding the CYW43 blobs.
///
/// Must match the `CYW43 blobs` row in `memory.x`. Nothing in the linker script
/// covers this region, because the firmware only ever reads it.
const BLOB_BASE: u32 = 0x1008_8000;

/// Size of that region.
const BLOB_REGION: u32 = 240 * 1024;

/// Emits the addresses of the CYW43 blobs, and a manifest for the flasher.
///
/// The blobs are laid out back to back from [`BLOB_BASE`], each start rounded
/// up to a 256-byte page so the addresses can be expressed in a UF2.
fn gen_firmware_paths(out: &Path) {
    let dir = env::var("CYW43_FIRMWARE_DIR").unwrap_or_else(|_| {
        panic!(
            "CYW43_FIRMWARE_DIR is not set. Enter the dev shell with `nix develop` \
             (or `direnv allow`), which pins the CYW43 firmware and points this at it."
        )
    });

    let mut body = format!(
        "/// Start of the flash region holding the CYW43 blobs.\n\
         pub const BLOB_BASE: u32 = {BLOB_BASE:#010x};\n\n\
         /// Size of that region.\n\
         pub const BLOB_REGION: u32 = {BLOB_REGION};\n\n"
    );
    let mut manifest = String::new();
    let mut addr = BLOB_BASE;

    for (konst, file, doc) in [
        ("FIRMWARE", "43439A0.bin", "Main CYW43439 WiFi firmware."),
        ("CLM", "43439A0_clm.bin", "Country and regulatory limits."),
        ("NVRAM", "nvram_rp2040.bin", "Board-specific NVRAM for the Pico W."),
    ] {
        let path = Path::new(&dir).join(file);
        let len = fs::metadata(&path)
            .unwrap_or_else(|e| panic!("missing CYW43 blob {}: {e}", path.display()))
            .len() as u32;

        body.push_str(&format!(
            "/// {doc}\n\
             ///\n\
             /// Flashed at {addr:#010x}, {len} bytes.\n\
             pub const {konst}: (u32, usize) = ({addr:#010x}, {len});\n\n"
        ));
        manifest.push_str(&format!("{addr:#010x} {len} {}\n", path.display()));

        addr += len;
        // Page-align the next blob. `cyw43` only needs four-byte alignment, but
        // UF2 addresses each 256-byte page, so anything finer cannot be
        // expressed in a drag-and-drop image.
        addr = addr.next_multiple_of(256);
    }

    let used = addr - BLOB_BASE;
    assert!(
        used <= BLOB_REGION,
        "CYW43 blobs need {used} bytes but the region is only {BLOB_REGION}"
    );
    println!(
        "cargo:warning=cyw43 blobs: {used} bytes at {BLOB_BASE:#x} ({}% of the region)",
        used * 100 / BLOB_REGION
    );

    fs::write(out.join("cyw43_firmware.rs"), body).expect("write cyw43_firmware.rs");

    // The flashing tool needs the same addresses; writing them here keeps one
    // source of truth rather than two lists that can drift apart.
    let target = env::var("CARGO_TARGET_DIR").unwrap_or_else(|_| {
        Path::new(&env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace root")
            .join("target")
            .to_string_lossy()
            .into_owned()
    });
    let _ = fs::create_dir_all(&target);
    let _ = fs::write(Path::new(&target).join("blobs.manifest"), manifest);
}

/// Reads the POSIX rule footer out of each zone's TZif file.
///
/// Every version-2-or-later TZif ends with a newline-delimited POSIX TZ string
/// describing the rule currently in force — `CET-1CEST,M3.5.0,M10.5.0/3` for
/// Berlin. That is all a wall clock needs; the megabytes ahead of it are
/// historical transitions going back to the 1800s.
fn posix_rule(tzdir: &str, zone: &str) -> Option<String> {
    let bytes = fs::read(Path::new(tzdir).join(zone)).ok()?;
    if !bytes.starts_with(b"TZif") {
        return None;
    }
    // Trailing NULs, then the rule, then the newline that terminates it.
    let end = bytes.iter().rposition(|&b| b != 0)?;
    let start = bytes[..end].iter().rposition(|&b| b == b'\n')? + 1;
    let rule = core::str::from_utf8(&bytes[start..=end]).ok()?.trim();
    if rule.is_empty() {
        return None;
    }
    Some(rule.to_string())
}

/// Emits the zone table, plus the tzdata version it came from.
fn gen_tz_table(out: &Path) {
    let tzdir = env::var("TZDIR").unwrap_or_else(|_| "/usr/share/zoneinfo".to_string());

    let mut rows = Vec::new();
    let mut missing = Vec::new();
    for zone in ZONES {
        match posix_rule(&tzdir, zone) {
            Some(rule) => rows.push((*zone, rule)),
            None => missing.push(*zone),
        }
    }
    assert!(
        missing.is_empty(),
        "no POSIX rule found in {tzdir} for: {missing:?}"
    );
    assert!(!rows.is_empty(), "no zones resolved from {tzdir}");

    let longest = rows.iter().map(|(_, r)| r.len()).max().unwrap();
    assert!(
        longest <= 48,
        "POSIX rule longer than the 48-byte settings field: {longest} bytes"
    );

    // `TZDIR` points into a versioned nix store path, so record it for the
    // status page: it tells you how fresh the baked-in rules are.
    let version = Path::new(&tzdir)
        .ancestors()
        .find_map(|p| {
            let n = p.file_name()?.to_str()?;
            n.split('-').find(|s| s.starts_with("20")).map(str::to_string)
        })
        .unwrap_or_else(|| "unknown".to_string());

    let mut f = fs::File::create(out.join("tz_table.rs")).expect("create tz_table.rs");
    writeln!(
        f,
        "/// Version of the tzdata these rules were read out of.\n\
         pub const TZDATA_VERSION: &str = {version:?};\n\n\
         /// Curated zones offered in the setup UI, as (IANA name, POSIX rule).\n\
         ///\n\
         /// Read from tzdata at build time. The web UI can override the rule\n\
         /// directly, so this list only has to cover the common cases.\n\
         pub static ZONES: [(&str, &str); {}] = [",
        rows.len()
    )
    .unwrap();
    for (zone, rule) in &rows {
        writeln!(f, "    ({zone:?}, {rule:?}),").unwrap();
    }
    writeln!(f, "];").unwrap();
    println!("cargo:warning=tz table: {} zones from tzdata {version}", rows.len());
}

/// Encodes the WiFi-join QR and emits it as a packed bitmap.
///
/// The payload is a standard `WIFI:` URI, which both iOS and Android join
/// natively when scanned. It contains `;` separators, which are outside QR's
/// alphanumeric charset, so this always encodes as byte mode — meaning the
/// lowercase SSID costs nothing over an uppercase one.
fn gen_setup_qr(out: &Path) {
    use qrcodegen::{QrCode, QrCodeEcc};

    let payload = format!("WIFI:T:nopass;S:{SETUP_SSID};;");
    let qr = QrCode::encode_text(&payload, QrCodeEcc::Low).expect("encode setup QR");
    let size = qr.size() as usize;
    assert!(
        size <= MAX_QR_MODULES,
        "setup QR is {size} modules; more than {MAX_QR_MODULES} cannot be shown at 2px \
         per module on the panel's 64-pixel axis. Shorten the SSID."
    );

    // One bit per module, row-major, MSB leftmost.
    let stride = size.div_ceil(8);
    let mut bits = vec![0u8; stride * size];
    for y in 0..size {
        for x in 0..size {
            if qr.get_module(x as i32, y as i32) {
                bits[y * stride + x / 8] |= 0x80 >> (x % 8);
            }
        }
    }

    let mut f = fs::File::create(out.join("setup_qr.rs")).expect("create setup_qr.rs");
    writeln!(
        f,
        "/// SSID of the provisioning access point.\n\
         pub const SETUP_SSID: &str = {SETUP_SSID:?};\n\n\
         /// The `WIFI:` URI the setup QR encodes.\n\
         pub const SETUP_QR_PAYLOAD: &str = {payload:?};\n\n\
         /// Side length of the setup QR, in modules.\n\
         pub const SETUP_QR_SIZE: usize = {size};\n\n\
         /// Bytes per QR row.\n\
         pub const SETUP_QR_STRIDE: usize = {stride};\n\n\
         /// The setup QR, one bit per module, row-major, most-significant bit leftmost.\n\
         pub static SETUP_QR: [u8; {}] = {:?};",
        bits.len(),
        bits
    )
    .unwrap();
    println!("cargo:warning=setup QR: {size}x{size} modules for {payload:?}");
}
