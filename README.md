# morrow

A sunrise alarm clock firmware for the Raspberry Pi Pico W, written in Rust.

Features a WS2812 LED strip with smooth 16-bit spatial dithering, an SH1106 OLED display, rotary encoder controls, NTP network time synchronization, a local responsive web interface, and over-the-air (OTA) updates.

```bash
nix develop          # development environment (toolchain, probe-rs, picotool, tzdata)
```

Inside the shell:

```bash
cargo build --release   # compile the firmware
morrow-test             # run every off-device check and test suite
morrow-devserver        # run the web UI development server at http://127.0.0.1:8080
morrow-flash provision  # package bootloader + Wi-Fi blobs + firmware into one UF2
morrow-flash ota        # upload updates over the air to morrow.local
```

---

## Bill of Materials

| Part | Notes |
| :-- | :-- |
| Raspberry Pi Pico W (RP2040 + CYW43439) | Main controller, Wi-Fi |
| WS2812 / WS2812B addressable LED strip, 5V | Up to 1000 pixels; length to taste |
| 5V power supply | Size for the strip: up to ~60 mA per pixel at full white |
| 5V relay module, active-high input | Switches the strip's 5V supply; must accept a 3.3V logic input |
| SH1106 OLED display, 128×64, 1.3", I2C | 4-pin I2C variant (VCC, GND, SCL, SDA) |
| Rotary encoder with push button (e.g. EC11) | CLK/DT/SW; internal pull-ups are used, no external resistors needed |

---

## Hardware Pinout

The firmware runs on the Raspberry Pi Pico W (RP2040). GPIO assignments:

| Component | Signal | Pin | Notes |
| :-- | :-- | :-- | :-- |
| WS2812 strip | Data | `GP2` | Up to 1000 pixels, driven via DMA from **PIO1** |
| Relay | Control | `GP3` | Active high, switches the strip's 5V power supply |
| Rotary encoder | Button | `GP21` | Active low, internal pull-up |
| Rotary encoder | CLK | `GP20` | Quadrature A, internal pull-up |
| Rotary encoder | DT | `GP15` | Quadrature B, internal pull-up |
| SH1106 OLED | SDA | `GP16` | I2C0 at 400 kHz |
| SH1106 OLED | SCL | `GP17` | I2C0 at 400 kHz |

---

## First Run & Wi-Fi Setup

When unconfigured or if unable to connect to a saved network after several attempts, the device enters setup mode and broadcasts an open access point named `morrow-setup`:

1. **Scan the QR Code**: The OLED displays a Wi-Fi connection QR code (`WIFI:T:nopass;S:morrow-setup;;`). Most smartphones connect directly when scanned with the camera.
2. **Captive Portal**: Upon connecting, the captive portal automatically opens the setup interface.
3. **Select Network**: Choose your Wi-Fi network from the live scan results, enter the passphrase, and save.
4. **Online**: The access point shuts down and the clock joins your local network, advertising itself via mDNS at **`http://morrow.local/`**.

If the Wi-Fi credentials change or the network is lost, setup mode can be re-entered at any time from the web UI or automatically after repeated failed join attempts.

---

## Web Interface

Navigating to `http://morrow.local/` serves an embedded web management UI styled to match the OLED's high-contrast monochrome aesthetic:

* **Status**: Live time, sync status, active NTP server, network SSID, IP address, and timezone.
* **Alarm Configuration**: Alarm enable/disable (off, weekdays, weekends, every day), alarm time with steppers, configurable sunrise ramp duration (1–240 min), optional post-sunrise hold duration (0–240 min), and active LED count (1–1000).
* **Timezone**: Selection across global timezones or custom POSIX timezone rule configuration.
* **System**: Firmware update (.bin upload), as well as a one-click button to enter Wi-Fi setup mode.

---

## Timezones & NTP

Time is synchronized over NTP via `pool.ntp.org`. Because NTP serves UTC timestamps without local timezone information, conversion to local wall-clock time occurs on-device.

Rather than storing the full 1.9 MB IANA timezone database, `build.rs` compiles a curated set of 39 IANA timezones into compact POSIX timezone strings (e.g. `CET-1CEST,M3.5.0,M10.5.0/3` for `Europe/Berlin`, occupying only 26 bytes).

The POSIX rule is evaluated on-device and serves as the source of truth. Users can customize the rule at runtime via the web UI without needing to reflash the firmware.

---

## Over-The-Air (OTA) Updates

Firmware updates can be uploaded over Wi-Fi without physical access to the device:

```bash
morrow-flash ota              # upload to morrow.local
morrow-flash ota 192.168.0.x  # or specify an explicit target IP
```

Updates can also be uploaded directly through the browser on the `System` section of the web interface.

### Fail-Safe A/B Partitioning & Watchdog Rollback

The 2 MB onboard NOR flash is split into dedicated, non-overlapping regions:

```
0x10000000  BOOT2              256 B   Second-stage bootloader
0x10000100  bootloader          23 KB  Embassy-boot-rp A/B swap manager
0x10006000  BOOTLOADER_STATE     4 KB  Slot state & rollback tracking
0x10007000  ACTIVE             256 KB  Currently running firmware
0x10047000  DFU                260 KB  Staged OTA firmware slot
0x10088000  CYW43 blobs        240 KB  Wi-Fi / Bluetooth firmware blobs
0x100C4000  (free)            1256 KB  Unallocated flash
0x101FE000  settings             8 KB  Dual alternating wear-leveled slots
```

1. **CRC-32 Verification**: Incoming images are written to flash one sector at a time and validated against an uploaded CRC-32 checksum before the bootloader swap is armed.
2. **Watchdog Protection**: When booting into an updated image, the bootloader arms the RP2040 hardware watchdog with an 8-second timeout.
3. **Automatic Confirmation**: The new firmware must run healthily for 30 seconds before confirming itself. If the new image panics, deadlocks, or fails to feed the watchdog, the board resets and the bootloader automatically reverts to the previous working slot.
4. **Interlock**: Flash writes are blocked while an alarm ramp is active to prevent flash erase pauses from interrupting LED timing.

---

## Development & Architecture

The workspace is organized into modular crates:

| Crate | Target | Role |
| :-- | :-- | :-- |
| `firmware/` | `thumbv6m-none-eabi` | Core embedded application using Embassy async tasks (lamp, display, encoder, alarm, network, HTTP). |
| `bootloader/` | `thumbv6m-none-eabi` | Minimal second-stage bootloader managing slot swaps and rollback. |
| `ui/` | Host & Target | Zero-allocation HTML/CSS page renderer, SVG sprite generator, and form parser. |
| `devserver/` | Host (`x86_64` / `aarch64`) | Local web server running against mock hardware state for fast UI development. |

### Development Web Server

Run the UI locally in a desktop browser without hardware:

```bash
morrow-devserver             # open http://127.0.0.1:8080
morrow-devserver --setup     # view the captive portal setup page
```

### Verification & Test Suite

Run all off-device checks:

```bash
morrow-test
# or from outside the shell:
nix run .#test
```

The test runner validates:
* **Timezone Rules**: Exact match against glibc across 1.18 million instants covering daylight saving transitions, half-hour offsets, and leap years.
* **Settings Codec**: Flash serialization round-trip, wear-leveling recovery, and bit-flip detection across all record fields.
* **Flash Partition Sanity**: Agreement between bootloader and firmware linker scripts, boot2 checksum verification, and partition boundaries.
* **UF2 Integrity**: Packing validation and alignment assertions.
* **Compiler & Clippy**: Clean zero-warning compilation for both target and host.

---

## Flashing

### Initial Provisioning

Hold the `BOOTSEL` button on the Pico W while connecting via USB, then run:

```bash
morrow-flash provision
```

This builds a combined `.uf2` containing the bootloader, CYW43 Wi-Fi blobs, and application firmware, copying it to the mounted mass storage device.

### Debugging with SWD

To flash and stream live `defmt` logging over a Raspberry Pi Debug Probe or CMSIS-DAP probe:

```bash
morrow-flash probe
```

---

## License

This project is licensed under the [MIT License](LICENSE). Spleen font (`firmware/src/font.rs`) is licensed under the BSD-2-Clause license (© Frederic Cambus). The CYW43 Wi-Fi firmware binary blobs are downloaded at build time under Infineon's permissive binary redistribution license.
