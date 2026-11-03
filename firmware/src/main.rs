//! morrow — a sunrise alarm for the Raspberry Pi Pico W.
//!
//! A port of a MicroPython firmware that drove the same hardware from a DCF77
//! radio receiver. The receiver is gone; the time now comes from NTP over WiFi.
//! The clock and alarm screens, the encoder behaviour, the gamma curve and the
//! LED dither pattern are all carried over deliberately unchanged — the point
//! was to change how the device learns the time, not how it behaves.
//!
//! # Shape of the program
//!
//! Tasks own hardware; [`state::APP`] is the only thing shared between them.
//!
//! | Task | Rate | Owns |
//! |---|---|---|
//! | `lamp_task` | 30 Hz | WS2812 strip on PIO1, relay |
//! | `display_task` | 10 Hz | SH1106 over I2C0 |
//! | `dial_task` | per edge | Rotary encoder |
//! | `alarm_task` | 10 Hz | Alarm schedule and sunrise ramp |
//! | `settings_task` | 1 Hz | Flash writes |
//! | `wifi::task` | — | CYW43 radio, IP configuration |
//! | `http::task` x2 | — | TCP port 80 |
//! | `dhcp`/`dns`/`mdns` | — | UDP services |
//!
//! # Pinout
//!
//! Unchanged from the MicroPython firmware, minus the DCF77 receiver that was
//! on GP14. None of these clash with the GP23–25/29 lines the Pico W wires to
//! its radio.
//!
//! | Signal | Pin |
//! |---|---|
//! | WS2812 data | GP2 |
//! | Relay | GP3 |
//! | Encoder button | GP21 |
//! | Encoder CLK | GP15 |
//! | Encoder DT | GP20 |
//! | OLED SDA | GP16 |
//! | OLED SCL | GP17 |

#![no_std]
#![no_main]
#![deny(missing_docs)]

use cyw43_pio::{DEFAULT_CLOCK_DIVIDER, PioSpi};
use defmt_rtt as _;
use embassy_executor::Spawner;
use embassy_net::{Stack, StackResources};
use embassy_rp::bind_interrupts;
use embassy_rp::clocks::RoscRng;
use embassy_rp::gpio::{Level, Output};
use embassy_rp::i2c::{self, I2c};
use embassy_rp::peripherals::{DMA_CH0, DMA_CH1, I2C0, PIO0, PIO1};
use embassy_rp::pio::{self, Pio};
use embassy_rp::watchdog::Watchdog;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Ticker, Timer};
use heapless::String;
use panic_probe as _;
use static_cell::StaticCell;

mod alarm;
mod bitmaps;
mod dial;
mod display;
mod flash;
mod font;
mod generated;
mod leds;
mod net;
mod scan;
mod settings;
mod state;
mod tz;
mod ui;
mod views;

bind_interrupts!(struct Irqs {
    // PIO0 drives the SPI link to the WiFi chip.
    PIO0_IRQ_0 => pio::InterruptHandler<PIO0>;
    // PIO1 drives the LED strip. The original firmware used PIO0 for this; on a
    // Pico W that block belongs to the radio.
    PIO1_IRQ_0 => pio::InterruptHandler<PIO1>;
    // Every RP2040 DMA channel raises DMA_IRQ_0.
    DMA_IRQ_0 => embassy_rp::dma::InterruptHandler<DMA_CH0>,
                 embassy_rp::dma::InterruptHandler<DMA_CH1>;
    I2C0_IRQ => i2c::InterruptHandler<I2C0>;
});

/// Encoder events, from the pin-watching task to the one that applies them.
///
/// Four is ample: events are consumed within a frame, and a backlog deeper than
/// this would mean the UI task has stopped, in which case dropping input is the
/// right failure.
static DIAL: Channel<CriticalSectionRawMutex, dial::Event, 4> = Channel::new();

/// Frame interval for the lamp. A 1000-pixel strip takes 30.0 ms to clock out,
/// so 30 Hz leaves headroom; 33 Hz would be the hard ceiling.
const LAMP_FRAME: Duration = Duration::from_millis(33);

/// How often the UI re-examines its state. Fast enough to feel immediate,
/// slow enough that the 23 ms panel refresh is never the bottleneck.
const UI_TICK: Duration = Duration::from_millis(100);

/// NTP server. Resolved by DNS on each sync, so a changing pool is fine.
const NTP_SERVER: &str = "pool.ntp.org";

/// Interval between NTP syncs once the clock is set.
///
/// The RP2040 crystal drifts on the order of 1.7 s a day, so hourly keeps the
/// error far below anything a sunrise would reveal.
const NTP_INTERVAL: Duration = Duration::from_secs(3600);

/// Retry interval while the clock has never been set.
const NTP_RETRY: Duration = Duration::from_secs(30);

/// Watchdog period. The bootloader arms one before handing over; this takes it
/// on and keeps it fed for as long as the UI loop is alive.
const WATCHDOG_TIMEOUT: Duration = Duration::from_secs(8);

/// How often the watchdog is fed, well inside [`WATCHDOG_TIMEOUT`] so that a
/// flash erase stalling everything for 50 ms is never mistaken for a hang.
const WATCHDOG_FEED: Duration = Duration::from_secs(2);

/// How long the firmware must run before it confirms itself to the bootloader.
///
/// Until it does, any reset reverts to the previous image. Long enough to cover
/// start-up and settle, short enough that a good update is not left provisional.
const CONFIRM_AFTER: Duration = Duration::from_secs(30);

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_rp::init(Default::default());
    defmt::info!("morrow {=str} starting", views::VERSION);

    // ---- flash and settings ------------------------------------------------
    let (store, loaded) = flash::Store::init(p.FLASH);
    state::with(|a| a.settings = loaded);
    // One owner for the whole program: settings writes and firmware updates
    // both go through here, and the RP2040 cannot do two at once.
    *flash::STORE.lock().await = Some(store);
    defmt::info!(
        "settings: alarm {:02}:{:02} {}, sunrise {} s, tz {=str}",
        loaded.hour,
        loaded.minute,
        loaded.mode,
        loaded.sunrise_secs,
        loaded.tz_rule.as_str()
    );

    // ---- lamp (PIO1 + DMA_CH1) --------------------------------------------
    let lamp = leds::Lamp::new(p.PIN_3, p.PIN_2, p.PIO1, p.DMA_CH1, Irqs);

    // ---- display (I2C0) ----------------------------------------------------
    let mut i2c_cfg = i2c::Config::default();
    i2c_cfg.frequency = 400_000;
    let i2c = I2c::new_async(p.I2C0, p.PIN_17, p.PIN_16, Irqs, i2c_cfg);
    let display = display::Display::new(i2c).await;

    // ---- encoder -----------------------------------------------------------
    let dial = dial::Dial::new(p.PIN_21, p.PIN_20, p.PIN_15);

    // ---- tasks -------------------------------------------------------------
    spawner.spawn(defmt::unwrap!(lamp_task(lamp)));
    spawner.spawn(defmt::unwrap!(display_task(display)));
    spawner.spawn(defmt::unwrap!(dial_task(dial)));
    spawner.spawn(defmt::unwrap!(alarm_task()));
    spawner.spawn(defmt::unwrap!(settings_task()));
    spawner.spawn(defmt::unwrap!(watchdog_task(Watchdog::new(p.WATCHDOG))));
    spawner.spawn(defmt::unwrap!(confirm_task()));

    // ---- radio (PIO0 + DMA_CH0) -------------------------------------------
    //
    // The WiFi blobs live in their own flash region rather than in this image.
    // If the board was flashed without them, say so and carry on as an offline
    // lamp: the clock, alarm and dial all work, there is simply no way to learn
    // the time. Handing erased flash to `cyw43` would fault inside the driver.
    if !generated::cyw43_fw::present() {
        defmt::error!(
            "cyw43 firmware missing at {:#x} — run `morrow-flash provision`. \
             Running offline: the lamp works, the clock cannot be set.",
            generated::cyw43_fw::BLOB_BASE
        );
        defmt::info!("morrow: running (offline)");
        return;
    }

    let pwr = Output::new(p.PIN_23, Level::Low);
    let cs = Output::new(p.PIN_25, Level::High);
    let mut pio = Pio::new(p.PIO0, Irqs);
    let spi = PioSpi::new(
        &mut pio.common,
        pio.sm0,
        DEFAULT_CLOCK_DIVIDER,
        pio.irq0,
        cs,
        p.PIN_24,
        p.PIN_29,
        embassy_rp::dma::Channel::new(p.DMA_CH0, Irqs),
    );

    static CYW43: StaticCell<cyw43::State> = StaticCell::new();
    let cyw43_state = CYW43.init(cyw43::State::new());
    let (device, control, runner) = cyw43::new(
        cyw43_state,
        pwr,
        spi,
        generated::cyw43_fw::aligned(generated::cyw43_fw::FIRMWARE),
        generated::cyw43_fw::aligned(generated::cyw43_fw::NVRAM),
    )
    .await;
    spawner.spawn(defmt::unwrap!(cyw43_task(runner)));

    static CONTROL: StaticCell<cyw43::Control<'static>> = StaticCell::new();
    let control = CONTROL.init(control);

    // ---- network stack -----------------------------------------------------
    let seed = RoscRng.next_u64();
    static RESOURCES: StaticCell<StackResources<8>> = StaticCell::new();
    let (stack, net_runner) = embassy_net::new(
        device,
        // Replaced by wifi::task the moment it decides which mode to run in.
        embassy_net::Config::default(),
        RESOURCES.init(StackResources::new()),
        seed,
    );
    spawner.spawn(defmt::unwrap!(net_task(net_runner)));

    spawner.spawn(defmt::unwrap!(net::wifi::task(control, stack)));
    spawner.spawn(defmt::unwrap!(ntp_task(stack)));
    spawner.spawn(defmt::unwrap!(net::dns::task(stack, net::AP_ADDR)));
    spawner.spawn(defmt::unwrap!(net::dhcp::task(stack, net::AP_ADDR, net::PORTAL_URL)));
    spawner.spawn(defmt::unwrap!(net::mdns::task(stack, views::HOSTNAME)));

    // Two HTTP workers, each with its own buffers, so one slow client cannot
    // keep the page from loading for another.
    macro_rules! http_worker {
        ($rx:ident, $tx:ident, $body:ident) => {{
            static $rx: StaticCell<[u8; 1536]> = StaticCell::new();
            static $tx: StaticCell<[u8; 1536]> = StaticCell::new();
            static $body: StaticCell<String<{ net::http::PAGE_MAX }>> = StaticCell::new();
            spawner.spawn(defmt::unwrap!(net::http::task(
                stack,
                $rx.init([0; 1536]),
                $tx.init([0; 1536]),
                $body.init(String::new()),
            )));
        }};
    }
    http_worker!(RX_A, TX_A, BODY_A);
    http_worker!(RX_B, TX_B, BODY_B);

    defmt::info!("morrow: running");
}

/// Drives the CYW43 radio.
#[embassy_executor::task]
async fn cyw43_task(
    runner: cyw43::Runner<'static, cyw43::SpiBus<Output<'static>, PioSpi<'static, PIO0, 0>>>,
) -> ! {
    runner.run().await
}

/// Drives the TCP/IP stack.
#[embassy_executor::task]
async fn net_task(mut runner: embassy_net::Runner<'static, cyw43::NetDriver<'static>>) -> ! {
    runner.run().await
}

/// Renders the LED strip.
#[embassy_executor::task]
async fn lamp_task(mut lamp: leds::Lamp) -> ! {
    let mut ticker = Ticker::every(LAMP_FRAME);
    loop {
        ticker.next().await;
        let target = state::with(|a| a.lamp_target);
        lamp.set(target);
        // Returns false when nothing changed, which skips the 30.0 ms strip
        // write entirely while the lamp is settled.
        lamp.tick().await;
        let current = lamp.current();
        state::with(|a| a.lamp_current = current);
    }
}

/// Keeps the panel in step with the state.
#[embassy_executor::task]
async fn display_task(mut display: display::Display) -> ! {
    let mut ticker = Ticker::every(UI_TICK);
    let mut last_minute = u8::MAX;

    loop {
        ticker.next().await;

        let (snap, render) = state::with(|a| {
            // Proof of life for the watchdog task.
            a.ui_heartbeat = a.ui_heartbeat.wrapping_add(1);
            ui::sync_with_net(a);

            // Step back out of editing screens once the dial goes quiet.
            if a.last_input.is_some() && a.idle_for(ui::IDLE_TIMEOUT) {
                ui::timeout(a);
                a.last_input = None;
            }

            // Redraw when the displayed minute rolls over, so the clock stays
            // right without repainting ten times a second.
            let minute = a.clock.local(&a.settings).map(|t| t.minute).unwrap_or(0);
            if minute != last_minute {
                last_minute = minute;
                a.display_dirty = true;
            }

            let dirty = a.display_dirty;
            a.display_dirty = false;
            (*a, dirty)
        });

        if render {
            ui::render(&mut display, &snap).await;
        }
    }
}

/// Watches the encoder pins.
#[embassy_executor::task]
async fn dial_task(mut dial: dial::Dial) -> ! {
    dial.run(DIAL.sender()).await
}

/// Applies encoder events and advances the alarm.
#[embassy_executor::task]
async fn alarm_task() -> ! {
    let mut ticker = Ticker::every(alarm::TICK);
    let rx = DIAL.receiver();
    loop {
        // Whichever comes first: a turn of the dial, or the next tick.
        match embassy_futures::select::select(rx.receive(), ticker.next()).await {
            embassy_futures::select::Either::First(event) => {
                state::with(|a| ui::handle(a, event));
            }
            embassy_futures::select::Either::Second(()) => {
                state::with(alarm::tick);
            }
        }
    }
}

/// Writes settings to flash when they change.
#[embassy_executor::task]
async fn settings_task() -> ! {
    loop {
        Timer::after(Duration::from_secs(1)).await;
        // Snapshot and clear together, so a change arriving mid-write is not
        // lost: it simply re-arms the flag for the next pass.
        let pending = state::with(|a| {
            let dirty = a.settings_dirty;
            a.settings_dirty = false;
            dirty.then_some(a.settings)
        });
        if let Some(s) = pending {
            if let Some(store) = flash::STORE.lock().await.as_mut() {
                store.save(&s);
            }
        }
    }
}

/// Keeps the watchdog fed, but only while the UI loop is actually running.
///
/// Feeding unconditionally would only prove the executor is alive. Gating on
/// the display task's heartbeat makes the watchdog a real liveness check: if
/// the UI wedges — a hung I2C transfer, say — the device resets, and if that
/// happened to a freshly installed image the bootloader reverts it.
#[embassy_executor::task]
async fn watchdog_task(mut watchdog: Watchdog) -> ! {
    watchdog.start(WATCHDOG_TIMEOUT);
    let mut ticker = Ticker::every(WATCHDOG_FEED);
    let mut last = 0u32;
    loop {
        ticker.next().await;
        let beat = state::with(|a| a.ui_heartbeat);
        if beat != last {
            last = beat;
            watchdog.feed(WATCHDOG_TIMEOUT);
        } else {
            defmt::error!("watchdog: ui loop has not ticked, letting the dog bite");
        }
    }
}

/// Confirms the running image to the bootloader once it has proved itself.
///
/// Before this runs, a reset reverts to the previous firmware. That is the
/// whole safety net: an update that panics, hangs or wedges the UI never gets
/// here, so the watchdog resets the device and the old image comes back.
#[embassy_executor::task]
async fn confirm_task() -> ! {
    Timer::after(CONFIRM_AFTER).await;

    if let Some(store) = flash::STORE.lock().await.as_mut() {
        match store.ota_state() {
            Some(embassy_boot_rp::State::Swap) => {
                if store.ota_mark_booted() {
                    defmt::info!("ota: new firmware confirmed, rollback disarmed");
                } else {
                    defmt::error!("ota: could not confirm; next reset will revert");
                }
            }
            other => {
                defmt::debug!("ota: nothing to confirm (state {:?})", defmt::Debug2Format(&other));
                // Harmless when there is no pending swap, and covers the case
                // where a previous confirm failed to stick.
                store.ota_mark_booted();
            }
        }
    }

    core::future::pending().await
}

/// Keeps the clock synchronised.
#[embassy_executor::task]
async fn ntp_task(stack: Stack<'static>) -> ! {
    loop {
        let online = state::with(|a| a.net.mode == state::NetMode::Online);
        if !online {
            Timer::after(NTP_RETRY).await;
            continue;
        }

        match net::ntp::query(stack, NTP_SERVER, 123).await {
            Some(unix) => {
                let first = state::with(|a| {
                    let first = !a.clock.is_valid();
                    a.clock.set(unix);
                    if first {
                        a.system_mode = state::SystemMode::Operational;
                        if a.ui == state::UiState::Setup {
                            a.ui = state::UiState::Clock;
                        }
                    }
                    a.display_dirty = true;
                    first
                });
                if first {
                    defmt::info!("ntp: clock set, entering operational mode");
                }
                Timer::after(NTP_INTERVAL).await;
            }
            None => {
                defmt::warn!("ntp: sync failed");
                Timer::after(NTP_RETRY).await;
            }
        }
    }
}
