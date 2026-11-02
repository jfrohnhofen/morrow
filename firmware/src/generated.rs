//! Build-time generated tables.
//!
//! `build.rs` writes these into `OUT_DIR`; including them once here and
//! re-exporting keeps a single copy, rather than one per `include!` site.

/// The WiFi-join QR shown during provisioning, and the SSID it encodes.
pub mod qr {
    #![allow(dead_code)]
    include!(concat!(env!("OUT_DIR"), "/setup_qr.rs"));
}

/// Curated timezones, read from tzdata at build time.
pub mod tz_table {
    #![allow(dead_code)]
    include!(concat!(env!("OUT_DIR"), "/tz_table.rs"));
}

/// Where the CYW43 firmware blobs live in flash.
///
/// They are not embedded in the image. At 233 KB they would make up 57% of
/// every over-the-air update while never changing between builds, so they get
/// their own flash region, written once, and are read from there.
///
/// The flake pins the blobs and `build.rs` records their addresses, so neither
/// the binaries nor the offsets are ever typed by hand.
pub mod cyw43_fw {
    #![allow(dead_code)]
    include!(concat!(env!("OUT_DIR"), "/cyw43_firmware.rs"));

    /// Borrows a blob from the XIP-mapped flash window.
    pub fn bytes(blob: (u32, usize)) -> &'static [u8] {
        let (addr, len) = blob;
        // SAFETY: the address and length come from build.rs, which measured the
        // same files it tells the flasher to write there. Flash is mapped
        // read-only for execute-in-place at 0x10000000, so the region stays
        // readable for the life of the program.
        unsafe { core::slice::from_raw_parts(addr as *const u8, len) }
    }

    /// Borrows a blob as the word-aligned type `cyw43` expects.
    pub fn aligned(blob: (u32, usize)) -> &'static cyw43::Aligned<cyw43::A4, [u8]> {
        debug_assert!(blob.0.is_multiple_of(4), "cyw43 blob must be word-aligned");
        let slice = bytes(blob);
        // SAFETY: `Aligned<A4, [u8]>` is `#[repr(C)]` with a zero-sized
        // alignment marker followed by the value, so the value sits at offset 0
        // and the type has identical size and pointer metadata to `[u8]`. The
        // only additional requirement is four-byte alignment, which build.rs
        // guarantees by rounding each blob's start up.
        unsafe { &*(slice as *const [u8] as *const cyw43::Aligned<cyw43::A4, [u8]>) }
    }

    /// Have the blobs actually been flashed?
    ///
    /// Erased flash reads as `0xFF`. Handing that to `cyw43` faults deep inside
    /// the driver, so a half-provisioned device is caught here and reported
    /// plainly instead.
    pub fn present() -> bool {
        bytes(FIRMWARE).iter().take(64).any(|&b| b != 0xFF)
            && bytes(CLM).iter().take(16).any(|&b| b != 0xFF)
    }
}
