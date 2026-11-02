#!/usr/bin/env python3
"""Check tools/mkuf2.py produces well-formed UF2.

A malformed UF2 is accepted by the BOOTSEL drive and then silently does the
wrong thing, so the block headers, ordering and padding are verified here rather
than discovered on a device.

Usage:  python3 tools/test_uf2.py
"""
import os
import struct
import subprocess
import sys
import tempfile

BLOCK = 512
PAYLOAD = 256
MAGIC0, MAGIC1, MAGIC_END = 0x0A324655, 0x9E5D5157, 0x0AB16F30
RP2040_FAMILY = 0xE48BFF56


def main():
    passed = failed = 0

    def check(label, ok):
        nonlocal passed, failed
        print(f"  {'ok  ' if ok else 'FAIL'}  {label}")
        if ok:
            passed += 1
        else:
            failed += 1

    with tempfile.TemporaryDirectory() as d:
        a, b, out = f"{d}/a.bin", f"{d}/b.bin", f"{d}/o.uf2"
        # Three whole pages, and one partial page.
        open(a, "wb").write(bytes(range(256)) * 3)
        open(b, "wb").write(b"\xAB" * 100)
        subprocess.run(
            [sys.executable, "tools/mkuf2.py", out, f"0x10000000={a}", f"0x10088000={b}"],
            check=True, stdout=subprocess.DEVNULL,
        )
        raw = open(out, "rb").read()

        check("file is a whole number of blocks", len(raw) % BLOCK == 0)
        n = len(raw) // BLOCK
        check("block count matches the payload", n == 4)

        addrs, ok_hdr, ok_end, ok_seq = [], True, True, True
        for i in range(n):
            blk = raw[i * BLOCK : (i + 1) * BLOCK]
            m0, m1, flags, addr, size, no, total, fam = struct.unpack("<IIIIIIII", blk[:32])
            ok_hdr &= (m0, m1, fam, size) == (MAGIC0, MAGIC1, RP2040_FAMILY, PAYLOAD)
            ok_hdr &= bool(flags & 0x2000)
            ok_end &= struct.unpack("<I", blk[508:512])[0] == MAGIC_END
            ok_seq &= (no, total) == (i, n)
            addrs.append((addr, blk[32 : 32 + size]))

        check("every block carries the UF2 magic and family id", ok_hdr)
        check("every block ends with the trailing magic", ok_end)
        check("blocks are numbered in order", ok_seq)
        check("addresses advance a page at a time, per region",
              [x for x, _ in addrs] == [0x10000000, 0x10000100, 0x10000200, 0x10088000])
        check("partial final page is zero padded",
              addrs[3][1][:100] == b"\xAB" * 100 and addrs[3][1][100:] == b"\x00" * 156)
        check("payload bytes survive intact",
              b"".join(p for _, p in addrs[:3]) == open(a, "rb").read())

        # Overlapping regions must be refused, not silently merged.
        r = subprocess.run(
            [sys.executable, "tools/mkuf2.py", f"{d}/x.uf2", f"0x10000000={a}", f"0x10000100={b}"],
            capture_output=True, text=True,
        )
        check("overlapping regions are rejected", r.returncode != 0)

        # Non-page-aligned addresses cannot be expressed in UF2.
        r = subprocess.run(
            [sys.executable, "tools/mkuf2.py", f"{d}/y.uf2", f"0x10000004={a}"],
            capture_output=True, text=True,
        )
        check("unaligned addresses are rejected", r.returncode != 0)

    print(f"\n{passed} passed, {failed} failed")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
