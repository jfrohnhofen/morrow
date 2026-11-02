/* Flash map for morrow. Must agree with bootloader/memory.x; `morrow-test`
   checks that they do.

   0x10000000  BOOT2              256 B   second-stage loader (the bootloader's)
   0x10000100  bootloader          23 K
   0x10006000  BOOTLOADER_STATE     4 K   which slot is live, and why
   0x10007000  ACTIVE             256 K   <- this program links here
   0x10047000  DFU                260 K   an uploaded image, awaiting swap
   0x10088000  CYW43 blobs        240 K   WiFi firmware, flashed once
   0x100C4000  (free)            1256 K
   0x101FE000  settings             8 K   two 4 K slots, written alternately

   BOOT2 is declared because the offsets below are computed from its origin,
   which is the base of flash. Nothing is emitted into it: `boot2-none` means
   this image carries no second stage, and there is no SECTIONS block here to
   place one. The bootloader owns it — see `bootloader/memory.x` — and an image
   that also carried one would overwrite it.
*/
MEMORY
{
  BOOT2            : ORIGIN = 0x10000000, LENGTH = 0x100
  BOOTLOADER_STATE : ORIGIN = 0x10006000, LENGTH = 4K
  FLASH            : ORIGIN = 0x10007000, LENGTH = 256K
  DFU              : ORIGIN = 0x10047000, LENGTH = 260K

  RAM              : ORIGIN = 0x20000000, LENGTH = 256K
}

__bootloader_state_start = ORIGIN(BOOTLOADER_STATE) - ORIGIN(BOOT2);
__bootloader_state_end   = ORIGIN(BOOTLOADER_STATE) + LENGTH(BOOTLOADER_STATE) - ORIGIN(BOOT2);

__bootloader_dfu_start = ORIGIN(DFU) - ORIGIN(BOOT2);
__bootloader_dfu_end   = ORIGIN(DFU) + LENGTH(DFU) - ORIGIN(BOOT2);
