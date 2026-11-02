//! A/B bootloader for morrow.
//!
//! Runs from the bottom of flash, decides which of the two firmware slots to
//! boot, and jumps to it. Nothing here talks to the lamp, the panel or the
//! network — the less this program does, the less there is to go wrong in the
//! one component that has no rollback behind it.
//!
//! # How a rollback happens
//!
//! [`WatchdogFlash`] arms the hardware watchdog before handing control over. The
//! new firmware has [`WATCHDOG_TIMEOUT`] to start feeding it and to call
//! `mark_booted`. If it panics, hangs, or never convinces itself it is healthy,
//! the watchdog resets the chip, this program sees a swap that was never
//! confirmed, and swaps back.
//!
//! So a bad update costs a reboot, not a trip to the workbench — provided this
//! program itself is sound, which is why it stays this small.

#![no_std]
#![no_main]

use core::cell::RefCell;

use cortex_m_rt::{entry, exception};
use defmt_rtt as _;
use embassy_boot_rp::*;
use embassy_sync::blocking_mutex::Mutex;
use embassy_time::Duration;

/// Flash fitted to the Pico W.
const FLASH_SIZE: usize = 2 * 1024 * 1024;

/// How long the firmware has to prove itself before the watchdog reverts it.
///
/// Generous on purpose: the firmware's own startup does a flash read for
/// settings and brings up the display before it can report anything, and a
/// spurious rollback is worse than a slow one.
const WATCHDOG_TIMEOUT: Duration = Duration::from_secs(8);

#[entry]
fn main() -> ! {
    let p = embassy_rp::init(Default::default());

    // Arming the watchdog here is what makes rollback automatic: from this
    // point until the firmware feeds it, a hang resets the chip.
    let flash = WatchdogFlash::<FLASH_SIZE>::start(p.FLASH, p.WATCHDOG, WATCHDOG_TIMEOUT);
    let flash = Mutex::new(RefCell::new(flash));

    let config = BootLoaderConfig::from_linkerfile_blocking(&flash, &flash, &flash);
    let active_offset = config.active.offset();
    let bl: BootLoader = BootLoader::prepare(config);

    // SAFETY: `prepare` has just resolved which slot is bootable and performed
    // any pending swap, so `active_offset` names a complete image.
    unsafe { bl.load(embassy_rp::flash::FLASH_BASE as u32 + active_offset) }
}

/// A fault this early means the slot is unusable; reset so the watchdog path
/// gets a chance to revert rather than sitting in a fault loop.
#[unsafe(no_mangle)]
#[cfg_attr(target_os = "none", unsafe(link_section = ".HardFault.user"))]
unsafe extern "C" fn HardFault() {
    cortex_m::peripheral::SCB::sys_reset();
}

#[exception]
unsafe fn DefaultHandler(_: i16) -> ! {
    cortex_m::peripheral::SCB::sys_reset();
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    // Deliberately not resetting: a panic here would otherwise loop silently.
    // Letting the watchdog fire makes the failure visible and still recovers.
    cortex_m::asm::udf();
}
