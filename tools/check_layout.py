#!/usr/bin/env python3
"""Check that the two linker scripts describe the same flash map.

The bootloader and the firmware each carry a `memory.x`, and they have to agree
about where the state block and the two firmware slots live. If they drift, the
bootloader swaps one range while the firmware writes another — which would be
discovered on a device, as a brick, rather than here.

Also checks the built artefacts still fit the regions they are linked for, and
that the bootloader image really does begin with a valid RP2040 second stage.

That last one is not paranoia. The second stage has to be placed by a SECTIONS
block in `bootloader/memory.x`; without it the blob is emitted as an orphan
section at the end of the image, `objcopy` then starts the binary at the vector
table, and `morrow-flash provision` writes the whole bootloader 256 bytes low.
Everything still builds, every other check here still passes, and the board
comes up in USB mass storage because the boot ROM finds no second stage. The
only way to see it is to checksum the bytes, so that is what this does.

Usage:  python3 tools/check_layout.py
"""
import os
import re
import sys

K = 1024
BOOT = "bootloader/memory.x"
APP = "firmware/memory.x"

# Must agree exactly; the firmware's FLASH is the bootloader's ACTIVE.
SHARED = [("BOOTLOADER_STATE", "BOOTLOADER_STATE"), ("ACTIVE", "FLASH"), ("DFU", "DFU")]


def regions(path):
    """Parses the MEMORY block into {name: (origin, length)}."""
    text = open(path).read()
    out = {}
    for name, origin, length in re.findall(
        r"^\s*(\w+)\s*:\s*ORIGIN\s*=\s*(0x[0-9A-Fa-f]+)\s*,\s*LENGTH\s*=\s*([^\n]+)$",
        text, re.M,
    ):
        out[name] = (int(origin, 16), size(length))
    return out


def size(expr):
    """Evaluates the small arithmetic the linker allows in LENGTH."""
    expr = expr.strip().rstrip(",")
    expr = re.sub(r"(\d+)K\b", lambda m: str(int(m.group(1)) * K), expr)
    return eval(expr, {"__builtins__": {}}, {})  # noqa: S307 - our own files


def boot2_checksum(data):
    """The checksum the RP2040 boot ROM expects over the first 252 bytes.

    CRC-32 with polynomial 0x04C11DB7, seeded all-ones, MSB first, no final
    inversion -- the same value pico-sdk's `pad_checksum` appends.
    """
    crc = 0xFFFFFFFF
    for byte in data:
        crc ^= byte << 24
        for _ in range(8):
            crc = ((crc << 1) ^ 0x04C11DB7) & 0xFFFFFFFF if crc & 0x80000000 \
                else (crc << 1) & 0xFFFFFFFF
    return crc


def main():
    boot, app = regions(BOOT), regions(APP)
    failures = []

    def check(label, ok, detail=""):
        print(f"  {'ok  ' if ok else 'FAIL'}  {label}{detail}")
        if not ok:
            failures.append(label)

    print("flash map agreement:")
    for b_name, a_name in SHARED:
        b, a = boot.get(b_name), app.get(a_name)
        ok = b is not None and a is not None and b == a
        detail = "" if ok else f"  (bootloader {b}, firmware {a})"
        check(f"{b_name} matches firmware {a_name}", ok, detail)

    print("\nregion sanity:")
    active, dfu = boot.get("ACTIVE"), boot.get("DFU")
    if active and dfu:
        # The swap algorithm needs the spare slot to be at least one sector
        # larger than the one it is swapping with.
        check("DFU is at least one sector larger than ACTIVE",
              dfu[1] >= active[1] + 4096,
              f"  (ACTIVE {active[1] // K}K, DFU {dfu[1] // K}K)")
        check("ACTIVE and DFU do not overlap", active[0] + active[1] <= dfu[0])

    blob_base, blob_len = 0x1008_8000, 240 * K
    if dfu:
        check("blob region starts after DFU", dfu[0] + dfu[1] <= blob_base)
    settings = 0x1000_0000 + 2 * 1024 * K - 8 * K
    check("blob region ends before the settings slots",
          blob_base + blob_len <= settings)

    print("\nsecond stage:")
    boot_bin = "target/morrow-boot.bin"
    boot2_ok = False
    if not os.path.exists(boot_bin):
        print(f"  skip  bootloader image not built ({boot_bin})")
    else:
        blob = open(boot_bin, "rb").read()[:256]
        if len(blob) < 256:
            check("bootloader image is at least one boot2 block", False,
                  f"  ({len(blob)} bytes)")
        else:
            # A vector table starts with an initial stack pointer in RAM. Seeing
            # one here means the image begins at 0x10000100, not 0x10000000.
            first = int.from_bytes(blob[:4], "little")
            check("image does not start with a vector table",
                  not 0x2000_0000 <= first < 0x2100_0000,
                  f"  (first word {first:#010x})")
            want = int.from_bytes(blob[252:256], "little")
            got = boot2_checksum(blob[:252])
            boot2_ok = want == got
            check("boot2 checksum is valid", boot2_ok,
                  f"  (stored {want:#010x}, computed {got:#010x})")

    print("\nbuilt artefacts:")
    for label, path, region in (
        ("bootloader", "target/thumbv6m-none-eabi/release/morrow-bootloader", boot.get("FLASH")),
        ("firmware", "target/thumbv6m-none-eabi/release/morrow", app.get("FLASH")),
    ):
        binpath = {"bootloader": "target/morrow-boot.bin", "firmware": "target/morrow-app.bin"}[label]
        if not os.path.exists(binpath):
            print(f"  skip  {label} image not built ({binpath})")
            continue
        used = os.path.getsize(binpath)
        # The bootloader image starts at BOOT2, one block below its own region,
        # so discount that block -- but only once the block above proved it is
        # really there, or a misplaced boot2 would be reported as headroom.
        if label == "bootloader":
            if not boot2_ok:
                print("  skip  bootloader region usage (no valid boot2 to discount)")
                continue
            used -= 0x100
        cap = region[1]
        check(f"{label} fits its region", used <= cap,
              f"  ({used:,} of {cap:,} bytes, {used * 100 // cap}%)")

    print()
    if failures:
        print(f"{len(failures)} check(s) failed")
        sys.exit(1)
    print("flash layout is consistent")


if __name__ == "__main__":
    main()
