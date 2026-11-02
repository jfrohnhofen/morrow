//! SH1106 128x64 OLED driver and 1-bit framebuffer.
//!
//! The panel is wired as 128 columns by 64 rows but mounted in portrait, so the
//! physical display is 64 wide by 128 tall. Two coordinate spaces follow from
//! that:
//!
//! * **Buffer space** — the panel's own 128x64 addressing. The clock and alarm
//!   screens draw here, at the exact coordinates the MicroPython firmware used,
//!   which is why their glyphs are stored sideways.
//! * **Physical space** — 64 wide by 128 tall, upright as the user sees it.
//!   Setup mode draws here, since its text and QR are new and have no legacy
//!   coordinates to preserve.
//!
//! [`Display::px`] converts between them. The conversion is a transpose: the
//! panel's hardware mirror and its rotated mounting compose to exactly that.

#![allow(clippy::identity_op)]

use embassy_rp::i2c::{Async, I2c};
use embassy_rp::peripherals::I2C0;
use embassy_time::{Duration, with_timeout};

use crate::bitmaps::Bitmap;
use crate::font::{FONT_ADVANCE, FONT_H, glyph};

/// Panel width in buffer space.
pub const W: i32 = 128;
/// Panel height in buffer space.
pub const H: i32 = 64;
/// Framebuffer size: one bit per pixel, packed in 8-row pages.
const BUF: usize = (W * H / 8) as usize;

/// Physical width, the panel being mounted portrait.
pub const PW: i32 = H;
/// Physical height, the panel being mounted portrait.
pub const PH: i32 = W;

// SH1106 command set, as used by the original driver. Several commands pack a
// field into their low bits, so they are written as `BASE | value` even when the
// value is zero — that mirrors the datasheet and says which field is meant.

const SET_DISP: u8 = 0xAE;
const LOW_COLUMN_ADDRESS: u8 = 0x00;
const HIGH_COLUMN_ADDRESS: u8 = 0x10;
const SET_SEG_REMAP: u8 = 0xA0;
const SET_PAGE_ADDRESS: u8 = 0xB0;
const SET_SCAN_DIR: u8 = 0xC0;

/// The SH1106 has 132 columns and centres a 128-pixel panel in them, so every
/// page write starts two columns in.
const COL_OFFSET: u8 = 2;

/// Addresses to probe, in order. These modules are strapped to one or the other.
const ADDRESSES: [u16; 2] = [0x3C, 0x3D];

/// Ceiling on one probe attempt.
///
/// A two-byte write at 400 kHz takes about 60 us and a missing panel NAKs
/// immediately, so only a stuck bus — SDA held low by a confused device —
/// reaches this. It matters because this runs before any task is spawned, so a
/// hang here is a watchdog reset loop with nothing on the panel to say why.
const PROBE_TIMEOUT: Duration = Duration::from_millis(50);

/// A framebuffer plus the I2C link to the panel.
pub struct Display {
    i2c: I2c<'static, I2C0, Async>,
    addr: u16,
    buf: [u8; BUF],
    /// Tracks the panel's own on/off state so redundant commands are skipped.
    powered: bool,
}

impl Display {
    /// Probes for the panel and applies the original firmware's orientation.
    pub async fn new(mut i2c: I2c<'static, I2C0, Async>) -> Display {
        let mut addr = ADDRESSES[0];
        for candidate in ADDRESSES {
            // Probe with a real one-command write, never a zero-length one.
            // `write_async` with an empty iterator starts no transfer at all,
            // then waits for the stop condition that therefore never comes, so
            // a zero-length probe hangs here for ever instead of reporting a
            // NAK — and this runs before any task exists, so the watchdog the
            // bootloader armed resets the board eight seconds later, silently,
            // for ever. The blocking half of the same driver rejects empty
            // writes outright; the async half simply does not check.
            //
            // Display-off is the command to probe with: harmless, idempotent,
            // and where the panel starts out anyway.
            let probe = with_timeout(PROBE_TIMEOUT, i2c.write_async(candidate, [0x80, SET_DISP]));
            match probe.await {
                Ok(Ok(())) => {
                    addr = candidate;
                    defmt::info!("display: found panel at {:#04x}", addr);
                    break;
                }
                Ok(Err(e)) => defmt::info!("display: {:#04x} did not answer: {}", candidate, e),
                Err(_) => defmt::warn!("display: {:#04x} probe timed out", candidate),
            }
        }
        defmt::info!("display: i2c address {:#04x}", addr);

        let mut d = Display {
            i2c,
            addr,
            buf: [0; BUF],
            powered: false,
        };
        // Horizontal flip and normal scan direction, matching how the panel is
        // soldered in. Without these the image is mirrored.
        d.cmd(SET_SEG_REMAP | 0x01).await;
        d.cmd(SET_SCAN_DIR | 0x00).await;
        d
    }

    /// Sends a single command byte.
    ///
    /// The leading `0x80` is a control byte with Co set and D/C clear, which is
    /// how the SH1106 distinguishes commands from pixel data.
    async fn cmd(&mut self, c: u8) {
        if let Err(e) = self.i2c.write_async(self.addr, [0x80, c]).await {
            defmt::warn!("display: command {:#04x} failed: {}", c, e);
        }
    }

    /// Turns the panel on, if it is not already.
    pub async fn on(&mut self) {
        if !self.powered {
            self.cmd(SET_DISP | 0x01).await;
            self.powered = true;
        }
    }

    /// Turns the panel off, if it is not already.
    ///
    /// Only the panel sleeps; the framebuffer is left intact.
    pub async fn off(&mut self) {
        if self.powered {
            self.cmd(SET_DISP | 0x00).await;
            self.powered = false;
        }
    }

    /// Clears the framebuffer.
    pub fn clear(&mut self) {
        self.buf.fill(0);
    }

    /// Sets or clears a pixel in buffer space.
    pub fn pixel(&mut self, x: i32, y: i32, on: bool) {
        if x < 0 || y < 0 || x >= W || y >= H {
            return;
        }
        let idx = ((y / 8) * W + x) as usize;
        let bit = 1u8 << (y % 8);
        if on {
            self.buf[idx] |= bit;
        } else {
            self.buf[idx] &= !bit;
        }
    }

    /// Reads a pixel in buffer space.
    pub fn get(&self, x: i32, y: i32) -> bool {
        if x < 0 || y < 0 || x >= W || y >= H {
            return false;
        }
        let idx = ((y / 8) * W + x) as usize;
        self.buf[idx] >> (y % 8) & 1 == 1
    }

    /// Sets a pixel in physical space, where the origin is the panel's top-left
    /// as mounted and seen by the user.
    ///
    /// The panel runs with `SET_SEG_REMAP | 0x01`, which mirrors it horizontally
    /// in hardware, and it is mounted rotated a quarter turn. A mirror composed
    /// with that rotation is exactly a transpose, so physical `(x, y)` is buffer
    /// `(y, x)` — no offset and no flip.
    ///
    /// Confirmed against the stored word bitmaps: `TIME`, `ALARM` and
    /// `WEEKDAYS` only read as words under this transform.
    pub fn px(&mut self, x: i32, y: i32, on: bool) {
        self.pixel(y, x, on);
    }

    /// Draws a bitmap at buffer coordinates, like MicroPython's `blit`.
    ///
    /// Only set pixels are written, so bitmaps compose rather than overwrite —
    /// matching `framebuf.blit` without a key colour.
    pub fn blit(&mut self, bm: &Bitmap, x: i32, y: i32) {
        for by in 0..bm.h {
            for bx in 0..bm.w {
                if bm.pixel(bx, by) {
                    self.pixel(x + bx, y + by, true);
                }
            }
        }
    }

    /// Fills a rectangle in buffer space, like MicroPython's `rect` with fill.
    pub fn rect(&mut self, x: i32, y: i32, w: i32, h: i32, on: bool) {
        for j in y..y + h {
            for i in x..x + w {
                self.pixel(i, j, on);
            }
        }
    }

    /// Inverts a rectangle in buffer space.
    ///
    /// This is how the original firmware highlighted the field being edited.
    pub fn invert(&mut self, x: i32, y: i32, w: i32, h: i32) {
        for j in y..y + h {
            for i in x..x + w {
                let v = self.get(i, j);
                self.pixel(i, j, !v);
            }
        }
    }

    /// Fills a rectangle in physical space.
    pub fn fill_px(&mut self, x: i32, y: i32, w: i32, h: i32, on: bool) {
        for j in y..y + h {
            for i in x..x + w {
                self.px(i, j, on);
            }
        }
    }

    /// Draws a string in physical space, upright, with its top-left at `(x, y)`.
    pub fn text(&mut self, s: &str, x: i32, y: i32, on: bool) {
        let mut cx = x;
        for c in s.chars() {
            let g = glyph(c);
            for (col, bits) in g.iter().enumerate() {
                for row in 0..FONT_H {
                    if bits >> row & 1 == 1 {
                        self.px(cx + col as i32, y + row, on);
                    }
                }
            }
            cx += FONT_ADVANCE;
        }
    }

    /// Draws a string in physical space, centred horizontally.
    pub fn text_centered(&mut self, s: &str, y: i32, on: bool) {
        let w = crate::font::text_width(s);
        self.text(s, (PW - w) / 2, y, on);
    }

    /// Draws a QR code in physical space with its top-left at `(x, y)`.
    ///
    // Every parameter here is a distinct, meaningful quantity; bundling them
    // into a struct would add ceremony without adding clarity.
    #[allow(clippy::too_many_arguments)]
    ///
    /// Rendered dark-on-light — a lit quiet-zone background with unlit modules.
    /// That is the polarity the QR spec assumes and every scanner handles;
    /// inverted codes are only read by some. `quiet` is in modules and `scale`
    /// is pixels per module.
    pub fn qr(
        &mut self,
        bits: &[u8],
        size: usize,
        stride: usize,
        x: i32,
        y: i32,
        scale: i32,
        quiet: i32,
    ) {
        let side = (size as i32 + 2 * quiet) * scale;
        self.fill_px(x, y, side, side, true);
        for my in 0..size {
            for mx in 0..size {
                let byte = bits[my * stride + mx / 8];
                if byte >> (7 - mx % 8) & 1 == 1 {
                    self.fill_px(
                        x + (mx as i32 + quiet) * scale,
                        y + (my as i32 + quiet) * scale,
                        scale,
                        scale,
                        false,
                    );
                }
            }
        }
    }

    /// Pushes the framebuffer to the panel.
    ///
    /// Eight page writes of 129 bytes each. At 400 kHz that is about 23 ms, so
    /// this is called only when something actually changed, never per frame.
    pub async fn flush(&mut self) {
        for page in 0..(H / 8) as u8 {
            self.cmd(SET_PAGE_ADDRESS | page).await;
            self.cmd(LOW_COLUMN_ADDRESS | COL_OFFSET).await;
            self.cmd(HIGH_COLUMN_ADDRESS | 0).await;

            let start = page as usize * W as usize;
            let page_data = &self.buf[start..start + W as usize];
            // 0x40 is the control byte with Co clear and D/C set: pixel data.
            let mut frame = [0u8; 1 + W as usize];
            frame[0] = 0x40;
            frame[1..].copy_from_slice(page_data);
            if let Err(e) = self.i2c.write_async(self.addr, frame).await {
                defmt::warn!("display: page {} write failed: {}", page, e);
                return;
            }
        }
    }
}
