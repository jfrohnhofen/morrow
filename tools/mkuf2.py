#!/usr/bin/env python3
"""Pack raw binaries at fixed addresses into one UF2 file.

The Pico's BOOTSEL drive accepts UF2, and a UF2 can carry several disjoint
regions. That lets a first-time install be a single drag-and-drop containing the
bootloader, the CYW43 blobs and the firmware, instead of three separate flashing
steps with three different tools.

Usage:
    tools/mkuf2.py OUT.uf2 ADDR=FILE [ADDR=FILE ...]

Example:
    tools/mkuf2.py morrow.uf2 0x10000000=boot.bin 0x10088000=43439A0.bin
"""
import struct
import sys

MAGIC0 = 0x0A324655  # "UF2\n"
MAGIC1 = 0x9E5D5157
MAGIC_END = 0x0AB16F30
FLAG_FAMILY_ID = 0x00002000
RP2040_FAMILY = 0xE48BFF56
PAYLOAD = 256
BLOCK = 512


def blocks(regions):
    """Yields (addr, chunk) for every payload-sized piece of every region."""
    for addr, data in regions:
        for off in range(0, len(data), PAYLOAD):
            chunk = data[off : off + PAYLOAD]
            # The bootrom writes whole pages; pad the tail so the last block of
            # a region is not short.
            yield addr + off, chunk.ljust(PAYLOAD, b"\x00")


def pack(regions):
    pieces = list(blocks(regions))
    total = len(pieces)
    out = bytearray()
    for i, (addr, chunk) in enumerate(pieces):
        out += struct.pack(
            "<IIIIIIII",
            MAGIC0, MAGIC1, FLAG_FAMILY_ID, addr,
            PAYLOAD, i, total, RP2040_FAMILY,
        )
        out += chunk.ljust(476, b"\x00")
        out += struct.pack("<I", MAGIC_END)
    return bytes(out)


def main():
    if len(sys.argv) < 3:
        print(__doc__.strip(), file=sys.stderr)
        sys.exit(2)

    out_path, specs = sys.argv[1], sys.argv[2:]
    regions = []
    for spec in specs:
        if "=" not in spec:
            sys.exit(f"expected ADDR=FILE, got {spec!r}")
        addr_s, path = spec.split("=", 1)
        addr = int(addr_s, 0)
        if addr % PAYLOAD:
            sys.exit(f"{addr_s}: address must be a multiple of {PAYLOAD}")
        with open(path, "rb") as f:
            regions.append((addr, f.read()))

    # Overlapping regions would silently corrupt each other.
    spans = sorted((a, a + len(d), d) for a, d in regions)
    for (a1, e1, _), (a2, _, _) in zip(spans, spans[1:]):
        if e1 > a2:
            sys.exit(f"regions overlap: {a1:#x}..{e1:#x} and {a2:#x}..")

    data = pack(regions)
    with open(out_path, "wb") as f:
        f.write(data)

    print(f"wrote {out_path}: {len(data) // BLOCK} blocks, {len(data):,} bytes")
    for addr, payload in regions:
        print(f"  {addr:#010x}  {len(payload):>8,} bytes")


if __name__ == "__main__":
    main()
