/* Flash map for morrow. Must agree with firmware/memory.x; `morrow-test`
   checks that they do.

   0x10000000  BOOT2              256 B   second-stage loader
   0x10000100  bootloader          23 K   this program
   0x10006000  BOOTLOADER_STATE     4 K   which slot is live, and why
   0x10007000  ACTIVE             256 K   the running firmware
   0x10047000  DFU                260 K   an uploaded image, awaiting swap
   0x10088000  CYW43 blobs        240 K   WiFi firmware, flashed once
   0x100C4000  (free)            1256 K
   0x101FE000  settings             8 K   two 4 K slots, written alternately
*/
/* The second stage has to be placed by hand. embassy-rp pulls the 256-byte
   blob in from `rp2040-boot2` as a `#[link_section = ".boot2"]` static, but it
   ships no linker script to position it, and cortex-m-rt's `link.x` has no rule
   for the section — so without the SECTIONS block below the blob is emitted as
   an orphan after `.rodata`, the image starts at the vector table instead, and
   `morrow-flash provision` writes the whole bootloader 256 bytes low. The boot
   ROM then finds no valid second stage at 0x10000000 and drops the board into
   USB mass storage. `morrow-test` checks the checksum so that cannot recur. */
MEMORY
{
  BOOT2            : ORIGIN = 0x10000000, LENGTH = 0x100
  FLASH            : ORIGIN = 0x10000100, LENGTH = 24K - 0x100
  BOOTLOADER_STATE : ORIGIN = 0x10006000, LENGTH = 4K
  ACTIVE           : ORIGIN = 0x10007000, LENGTH = 256K
  DFU              : ORIGIN = 0x10047000, LENGTH = 260K

  RAM              : ORIGIN = 0x20000000, LENGTH = 264K
}

__bootloader_state_start = ORIGIN(BOOTLOADER_STATE) - ORIGIN(BOOT2);
__bootloader_state_end   = ORIGIN(BOOTLOADER_STATE) + LENGTH(BOOTLOADER_STATE) - ORIGIN(BOOT2);

__bootloader_active_start = ORIGIN(ACTIVE) - ORIGIN(BOOT2);
__bootloader_active_end   = ORIGIN(ACTIVE) + LENGTH(ACTIVE) - ORIGIN(BOOT2);

__bootloader_dfu_start = ORIGIN(DFU) - ORIGIN(BOOT2);
__bootloader_dfu_end   = ORIGIN(DFU) + LENGTH(DFU) - ORIGIN(BOOT2);

/* INSERT BEFORE is resolved after the whole script is read, so this reaches
   into `link.x`'s own SECTIONS from here. The ROM reads these 256 bytes first
   and checks the last word against a CRC over the preceding 252. */
SECTIONS
{
  .boot2 ORIGIN(BOOT2) :
  {
    KEEP(*(.boot2));
  } > BOOT2
} INSERT BEFORE .text;
