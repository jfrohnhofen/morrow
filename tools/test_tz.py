#!/usr/bin/env python3
"""Check firmware/src/tz.rs against glibc.

The timezone code is the piece most likely to be subtly wrong in a way nobody
notices until an alarm fires an hour late, so it is checked against a reference
implementation rather than hand-written expectations: every curated zone's POSIX
rule, plus southern-hemisphere, half-hour-DST, Julian-day, zero-based and
negative-DST forms, evaluated at thousands of instants dense around every
plausible transition.

Usage:  python3 tools/test_tz.py
"""
import calendar
import os
import re
import subprocess
import sys
import tempfile
import time

# Rules that exercise the parser beyond what the curated list covers.
EXTRA = {
    "lord howe (half-hour dst, bracketed abbrevs)": "<+1030>-10:30<+11>-11,M10.1.0,M4.1.0",
    "berlin, permanent winter time": "CET-1",
    "berlin, permanent summer time": "CEST-2",
    "julian-day form": "XXX3EDT,J100/2,J300/2",
    "zero-based day form": "YYY-5ZZZ,60/2,300/2",
    "dublin-style negative dst": "IST-1GMT0,M10.5.0,M3.5.0/1",
}


def build(workdir):
    """Compiles tz.rs standalone, with the defmt coupling stripped."""
    src = open("firmware/src/tz.rs").read()
    src = re.sub(r"^impl defmt::Format for Abbr \{.*?\n\}\n", "", src, flags=re.S | re.M)
    src = src.replace("#[derive(Copy, Clone, defmt::Format)]", "#[derive(Copy, Clone)]")
    src = re.sub(r"^//!", "//", src, flags=re.M)
    open(f"{workdir}/tz.rs", "w").write(src)
    open(f"{workdir}/main.rs", "w").write('''
include!("tz.rs");
use std::io::{self, BufRead};
fn main() {
    for line in io::stdin().lock().lines() {
        let line = line.unwrap();
        let mut it = line.splitn(2, '\\t');
        let rule = it.next().unwrap();
        let utc: i64 = it.next().unwrap().trim().parse().unwrap();
        match TzRule::parse(rule) {
            None => println!("PARSE_FAIL"),
            Some(r) => {
                let (dt, abbr, off) = local(utc, &r);
                println!("{:04}-{:02}-{:02} {:02}:{:02}:{:02} {} {} {}",
                    dt.year, dt.month, dt.day, dt.hour, dt.minute, dt.second,
                    abbr.as_str(), off, dt.weekday);
            }
        }
    }
}
''')
    subprocess.run(["rustc", "-O", "--edition", "2021", "-o", f"{workdir}/tztest",
                    f"{workdir}/main.rs"], check=True)
    return f"{workdir}/tztest"


def rules():
    """The curated zones from build.rs, plus the awkward extras."""
    tzdir = os.environ.get("TZDIR") or "/etc/zoneinfo"
    names = re.findall(r'^\s*"([A-Za-z_]+(?:/[A-Za-z_+\-]+)*)",\s*$',
                       open("firmware/build.rs").read(), re.M)
    out = {}
    for n in names:
        p = os.path.join(tzdir, n)
        if not os.path.exists(p):
            continue
        b = open(p, "rb").read()
        if not b.startswith(b"TZif"):
            continue
        end = max(i for i, v in enumerate(b) if v != 0)
        start = b.rfind(b"\n", 0, end) + 1
        out[n] = b[start:end + 1].decode().strip()
    out.update(EXTRA)
    return out


def instants():
    pts = set()
    for y in range(2024, 2031):
        for d in range(0, 366, 5):
            pts.add(calendar.timegm((y, 1, 1, 0, 0, 0, 0, 1, 0)) + d * 86400 + 11 * 3600)
        # Dense around every month a transition plausibly falls in.
        for mon in (3, 4, 9, 10, 11):
            base = calendar.timegm((y, mon, 1, 0, 0, 0, 0, 1, 0))
            pts.update(base + h * 3600 for h in range(31 * 24))
    return sorted(pts)


def main():
    with tempfile.TemporaryDirectory() as w:
        binary = build(w)
        rs = rules()
        pts = instants()
        cases = [(r, t) for r in rs.values() for t in pts]
        print(f"{len(rs)} rules x {len(pts):,} instants = {len(cases):,} comparisons")

        got = subprocess.run([binary],
                             input="\n".join(f"{r}\t{t}" for r, t in cases) + "\n",
                             capture_output=True, text=True).stdout.splitlines()
        assert len(got) == len(cases), f"{len(got)} outputs for {len(cases)} cases"

        bad, i = [], 0
        for name, rule in rs.items():
            os.environ["TZ"] = rule
            time.tzset()
            for t in pts:
                tm = time.localtime(t)
                zo = time.strftime("%z", tm)
                off = int(zo[:3]) * 3600 + int(zo[0] + zo[3:5]) * 60
                exp = (f"{tm.tm_year:04}-{tm.tm_mon:02}-{tm.tm_mday:02} "
                       f"{tm.tm_hour:02}:{tm.tm_min:02}:{tm.tm_sec:02} "
                       f"{time.strftime('%Z', tm)} {off} {tm.tm_wday}")
                if got[i] != exp:
                    bad.append((name, rule, t, exp, got[i]))
                i += 1

        print(f"checked {len(cases):,}  mismatches {len(bad):,}")
        if bad:
            for name, rule, t, e, g in bad[:10]:
                stamp = time.strftime("%Y-%m-%d %H:%M", time.gmtime(t))
                print(f"  {name}\n    rule={rule}\n    utc={t} ({stamp}Z)"
                      f"\n    glibc={e}\n    rust ={g}")
            sys.exit(1)
        print("EXACT MATCH against glibc across every rule and instant.")


if __name__ == "__main__":
    main()
