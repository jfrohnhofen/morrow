//! The one owner of the flash peripheral.
//!
//! Settings and firmware updates both write to flash, and the RP2040 cannot
//! have two of those in flight at once — a program operation runs with
//! execute-in-place suspended, so nothing else runs at all while it does.
//! Funnelling both through a single [`Store`] behind one mutex makes that
//! impossible to get wrong.
//!
//! # Why the updater is rebuilt per call
//!
//! [`BlockingFirmwareUpdater`] wants `&Mutex<NoopRawMutex, RefCell<F>>`, and a
//! `NoopRawMutex` is not `Sync`, so one cannot be held in a `static`. It is
//! built as a local instead, which is sound because `NorFlash` is implemented
//! for `&mut T`.
//!
//! The updater normally remembers which sector it last erased so it does not
//! erase twice. Rebuilding it drops that memory, so [`Store::ota_write`]
//! requires sector-aligned, sector-sized writes: then "erase the sector I am
//! about to fill" is exactly right, and each sector is erased once.

use core::cell::RefCell;

use embassy_boot_rp::{AlignedBuffer, BlockingFirmwareUpdater, FirmwareUpdaterConfig, State};
use embassy_rp::Peri;
use embassy_rp::flash::{Blocking, ERASE_SIZE, Flash};
use embassy_rp::peripherals::FLASH;
use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embassy_sync::blocking_mutex::raw::{CriticalSectionRawMutex, NoopRawMutex};
use embassy_sync::mutex::Mutex;

use crate::settings::{self, Settings};

/// Flash fitted to the Pico W.
pub const FLASH_SIZE: usize = 2 * 1024 * 1024;

/// Writes must arrive one erase sector at a time. See the module note.
pub const OTA_CHUNK: usize = ERASE_SIZE;

/// Scratch buffer length for the bootloader state block.
///
/// `BlockingFirmwareState::new` *asserts* this equals the state partition's
/// `WRITE_SIZE` rather than returning an error, so a wrong value panics the
/// moment anything touches the updater — `confirm_task` at 30 seconds into
/// every boot, and the status page, which reads the OTA state to render it.
/// Derived from the driver rather than written out, so it tracks the HAL.
const STATE_ALIGN: usize = <Driver as embedded_storage::nor_flash::NorFlash>::WRITE_SIZE;

/// The blocking flash driver.
type Driver = Flash<'static, FLASH, Blocking, FLASH_SIZE>;

/// A flash operation failed.
///
/// The richer refusals an upload can meet — wrong checksum, image too large,
/// an alarm in progress — are decided and reported by the HTTP layer, which can
/// say so in the response. By the time a request reaches here, the only thing
/// left to go wrong is the flash itself.
#[derive(Copy, Clone, defmt::Format)]
pub struct OtaError;

/// Owns the flash peripheral.
pub struct Store {
    driver: Driver,
    /// Sequence number of the current settings record.
    seq: u32,
}

impl Store {
    /// Claims the peripheral and reads back the stored settings.
    pub fn init(peri: Peri<'static, FLASH>) -> (Store, Settings) {
        let mut driver = Flash::new_blocking(peri);
        let (seq, loaded) = settings::load(&mut driver);
        (Store { driver, seq }, loaded)
    }

    /// Writes settings to whichever slot is currently stale.
    pub fn save(&mut self, s: &Settings) {
        if let Some(seq) = settings::save(&mut self.driver, self.seq, s) {
            self.seq = seq;
        }
    }

    /// What the bootloader thinks the current image's status is.
    pub fn ota_state(&mut self) -> Option<State> {
        let shared = BlockingMutex::<NoopRawMutex, _>::new(RefCell::new(&mut self.driver));
        let config = FirmwareUpdaterConfig::from_linkerfile_blocking(&shared, &shared);
        let mut aligned = AlignedBuffer([0u8; STATE_ALIGN]);
        BlockingFirmwareUpdater::new(config, &mut aligned.0)
            .get_state()
            .ok()
    }

    /// Confirms the running image, so the bootloader stops planning to revert.
    ///
    /// Until this is called, any reset lands back on the previous firmware.
    pub fn ota_mark_booted(&mut self) -> bool {
        let shared = BlockingMutex::<NoopRawMutex, _>::new(RefCell::new(&mut self.driver));
        let config = FirmwareUpdaterConfig::from_linkerfile_blocking(&shared, &shared);
        let mut aligned = AlignedBuffer([0u8; STATE_ALIGN]);
        BlockingFirmwareUpdater::new(config, &mut aligned.0)
            .mark_booted()
            .is_ok()
    }

    /// Writes one sector of an incoming image into the spare slot.
    ///
    /// `offset` must be a multiple of [`OTA_CHUNK`] and `data` no longer than
    /// one sector, so the sector erased is exactly the one being filled.
    pub fn ota_write(&mut self, offset: usize, data: &[u8]) -> Result<(), OtaError> {
        debug_assert!(offset.is_multiple_of(OTA_CHUNK), "ota writes must be sector-aligned");
        debug_assert!(data.len() <= OTA_CHUNK, "ota writes must be one sector");

        let shared = BlockingMutex::<NoopRawMutex, _>::new(RefCell::new(&mut self.driver));
        let config = FirmwareUpdaterConfig::from_linkerfile_blocking(&shared, &shared);
        let mut aligned = AlignedBuffer([0u8; STATE_ALIGN]);
        BlockingFirmwareUpdater::new(config, &mut aligned.0)
            .write_firmware(offset, data)
            .map_err(|e| {
                defmt::error!("ota: write failed: {:?}", defmt::Debug2Format(&e));
                OtaError
            })
    }

    /// Marks the uploaded image for swap on the next boot.
    pub fn ota_mark_updated(&mut self) -> Result<(), OtaError> {
        let shared = BlockingMutex::<NoopRawMutex, _>::new(RefCell::new(&mut self.driver));
        let config = FirmwareUpdaterConfig::from_linkerfile_blocking(&shared, &shared);
        let mut aligned = AlignedBuffer([0u8; STATE_ALIGN]);
        BlockingFirmwareUpdater::new(config, &mut aligned.0)
            .mark_updated()
            .map_err(|e| {
                defmt::error!("ota: mark failed: {:?}", defmt::Debug2Format(&e));
                OtaError
            })
    }
}

/// The single flash owner, shared by the settings task and the OTA endpoint.
///
/// `None` until `main` fills it in.
pub static STORE: Mutex<CriticalSectionRawMutex, Option<Store>> = Mutex::new(None);
