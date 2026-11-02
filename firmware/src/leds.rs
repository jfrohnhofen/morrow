//! WS2812 strip, brightness ramp, and relay sequencing.
//!
//! # Why 16-bit brightness works on an 8-bit strip
//!
//! WS2812s take 8 bits per channel, which at the bottom of a sunrise is far too
//! coarse — the first few steps would be visible jumps. The MicroPython
//! firmware solved this with spatial dithering, and that scheme is kept intact:
//! the high byte of a 16-bit level goes to every pixel, and the low byte decides
//! how many pixels get one extra count. Scattering those pixels across the strip
//! in a fixed pseudo-random order ([`LED_ORDER`]) turns the residue into
//! texture rather than a visible band.
//!
//! That yields 255 x 256 = 65,280 distinct levels, so the ramp is driven as a
//! `u16` rather than the 0..=255 index the original used.
//!
//! # Why the ramp is driven by time
//!
//! The original stepped brightness once per tick, which coupled sunrise length
//! to the tick rate and made it ~28 minutes by accident. Here [`Lamp::target`]
//! is set from elapsed time over the configured duration, so the duration is a
//! real setting and smoothness is unaffected: a 30-minute ramp at 30 fps
//! consumes about 1.2 of 65,280 levels per frame.

use embassy_rp::Peri;
use embassy_rp::gpio::{Level, Output};
use embassy_rp::peripherals::{DMA_CH1, PIN_2, PIN_3, PIO1};
use embassy_rp::pio::Pio;
use embassy_rp::pio_programs::ws2812::{Grb, PioWs2812, PioWs2812Program};
use embassy_time::{Duration, Instant};
use smart_leds::RGB8;

use crate::bitmaps::{LED_COUNT, LED_ORDER};

/// Full brightness.
pub const FULL: u16 = u16::MAX;

/// Ceiling on gamma output, matching the original gamma table's maximum.
///
/// Holding this at `0xFF00` keeps the invariant that the dithered `+1` can never
/// overflow a channel: when the high byte reaches 255 the low byte is zero, so
/// no pixel is incremented.
const GAMMA_MAX: u32 = 65280;

/// The dimmest level that still lights anything, equal to the original
/// firmware's `state = 1`.
///
/// Held during the relay's settling window so the supply sees a trivial load
/// before the strip ramps up.
const MIN_VISIBLE: u16 = 257;

/// Largest change per frame, carried over from the original's 4-per-update cap
/// on a 0..=255 scale. It keeps manual dial adjustments from jumping.
const SLEW_PER_FRAME: u16 = 4 * 257;

/// Brightness change per dial detent, the original's 20 on a 0..=255 scale.
pub const MANUAL_STEP: u16 = 20 * 257;

/// How long the relay is given to settle before the strip is driven properly.
const RELAY_SETTLE: Duration = Duration::from_millis(900);

/// Gamma curve: a square law, as the original table was.
///
/// The original stored 256 precomputed entries of `round(i^2 * 256 / 255)`.
/// Evaluating it instead costs one multiply and no flash, and deviates by at
/// most 254 of 65,280 — under one count of the strip's own 8-bit resolution at
/// the top of the range, where the eye is least sensitive anyway.
#[inline]
pub fn gamma(pos: u16) -> u16 {
    let p = pos as u32;
    ((p * p) >> 16).min(GAMMA_MAX) as u16
}

/// The lamp: strip, relay, and the slew-limited level in between.
pub struct Lamp {
    /// Where the level is headed.
    target: u16,
    /// Where it actually is, after slew limiting.
    current: u16,
    relay: Output<'static>,
    /// True once the relay has had [`RELAY_SETTLE`] to close.
    relay_on: bool,
    /// When the relay was energised, if it is.
    relay_since: Option<Instant>,
    ws: PioWs2812<'static, PIO1, 0, LED_COUNT, Grb>,
    pixels: [RGB8; LED_COUNT],
}

impl Lamp {
    /// Claims the relay pin, PIO1, and a DMA channel.
    ///
    /// WS2812 output lives on PIO1 because `cyw43-pio` takes PIO0 to talk to the
    /// WiFi chip. The original had PIO0 free and used it.
    pub fn new(
        relay: Peri<'static, PIN_3>,
        data: Peri<'static, PIN_2>,
        pio: Peri<'static, PIO1>,
        dma: Peri<'static, DMA_CH1>,
        irqs: crate::Irqs,
    ) -> Lamp {
        let Pio {
            mut common, sm0, ..
        } = Pio::new(pio, irqs);
        let program = PioWs2812Program::new(&mut common);
        let ws = PioWs2812::new(&mut common, sm0, dma, irqs, data, &program);

        Lamp {
            target: 0,
            current: 0,
            relay: Output::new(relay, Level::Low),
            relay_on: false,
            relay_since: None,
            ws,
            pixels: [RGB8::default(); LED_COUNT],
        }
    }

    /// The level currently being rendered.
    pub fn current(&self) -> u16 {
        self.current
    }

    /// Sets the target level outright.
    pub fn set(&mut self, level: u16) {
        self.target = level;
    }

    /// Advances one frame, pushing to the strip only if something changed.
    ///
    /// Returns whether a frame was sent. The early exit matters: a settled lamp
    /// skips the strip write entirely, which is 30.0 ms of DMA for 1000 pixels.
    pub async fn tick(&mut self) -> bool {
        if self.current == self.target {
            return false;
        }

        // Energise the relay before asking the strip for real current, and give
        // it time to close.
        if self.target > 0 && !self.relay_on {
            if self.relay_since.is_none() {
                defmt::info!("lamp: relay on");
                self.relay.set_high();
                self.relay_since = Some(Instant::now());
            }
            self.relay_on = self
                .relay_since
                .is_some_and(|t| t.elapsed() >= RELAY_SETTLE);
        }

        if !self.relay_on {
            // Hold at the floor while the relay settles.
            self.current = MIN_VISIBLE;
        } else if self.target.abs_diff(self.current) <= SLEW_PER_FRAME {
            self.current = self.target;
        } else if self.target > self.current {
            self.current += SLEW_PER_FRAME;
        } else {
            self.current -= SLEW_PER_FRAME;
        }

        if self.current == 0 {
            defmt::info!("lamp: relay off");
            self.relay.set_low();
            self.relay_on = false;
            self.relay_since = None;
        }

        self.render();
        self.ws.write(&self.pixels).await;
        true
    }

    /// Fills the pixel buffer for the current level.
    fn render(&mut self) {
        let count = crate::state::with(|a| a.settings.led_count as usize).clamp(1, LED_COUNT);
        let out = gamma(self.current) as u32;
        let base = (out >> 8) as u8;
        // How many pixels carry the low byte's residue.
        let extra = ((out & 0xFF) as usize * count) >> 8;

        // The strip is driven white, so all three channels track together.
        let flat = RGB8 {
            r: base,
            g: base,
            b: base,
        };
        self.pixels[..count].fill(flat);
        self.pixels[count..].fill(RGB8::default());

        // `base + 1` cannot overflow: `extra` is only non-zero when the low byte
        // is, and `GAMMA_MAX` guarantees the low byte is zero once base hits 255.
        let up = base + 1;
        for &i in LED_ORDER.iter().filter(|&&idx| (idx as usize) < count).take(extra) {
            self.pixels[i as usize] = RGB8 {
                r: up,
                g: up,
                b: up,
            };
        }
    }
}
