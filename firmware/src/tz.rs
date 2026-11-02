//! POSIX `TZ` rule parsing and civil-time conversion.
//!
//! NTP serves UTC and nothing else — the protocol has no timezone field — so
//! the conversion to wall-clock time happens here.
//!
//! Rather than carry the IANA database (1.9 MB of mostly historical
//! transitions), this works from the POSIX rule string each zone compiles down
//! to: `Europe/Berlin` is `CET-1CEST,M3.5.0,M10.5.0/3`, 26 bytes. That encodes
//! only the rule currently in force, which is exactly what a wall clock needs.
//!
//! The rule is stored in settings and editable in the web UI, so a government
//! changing its DST policy is a text edit rather than a reflash.
//!
//! Grammar, per POSIX:
//!
//! ```text
//! std offset [dst [offset] [,start[/time],end[/time]]]
//! ```
//!
//! Note the sign convention: an offset is what you *add to local time to get
//! UTC*, so UTC+1 is written `-1`. This module negates on parse and works in
//! UTC offsets throughout, which is the less surprising direction.

/// Longest POSIX rule string accepted. The longest in tzdata is 44 bytes
/// (`Australia/Lord_Howe`); 48 leaves a little headroom.
pub const MAX_RULE_LEN: usize = 48;

/// Longest zone abbreviation, e.g. `CEST` or the bracketed `<+1030>`.
const MAX_ABBR: usize = 8;

/// A zone abbreviation, inline rather than heap-allocated.
#[derive(Copy, Clone, Default)]
pub struct Abbr {
    buf: [u8; MAX_ABBR],
    len: u8,
}

impl Abbr {
    /// Returns the abbreviation as a string.
    pub fn as_str(&self) -> &str {
        // Only ever filled from ASCII in `parse_abbr`.
        core::str::from_utf8(&self.buf[..self.len as usize]).unwrap_or("???")
    }
}

impl defmt::Format for Abbr {
    fn format(&self, f: defmt::Formatter) {
        defmt::write!(f, "{}", self.as_str())
    }
}

/// When a DST transition happens, in the year's local calendar.
#[derive(Copy, Clone)]
enum When {
    /// `Jn`: Julian day 1..=365, where 29 February is never counted.
    JulianNoLeap(u16),
    /// `n`: zero-based day 0..=365, where 29 February *is* counted.
    ZeroBased(u16),
    /// `Mm.w.d`: month 1..=12, week 1..=5 (5 meaning last), weekday 0..=6
    /// (0 being Sunday). Every zone in modern tzdata uses this form.
    MonthWeekDay { month: u8, week: u8, day: u8 },
}

/// One end of a DST period: which day, and at what local time.
#[derive(Copy, Clone)]
struct Transition {
    when: When,
    /// Seconds after local midnight. POSIX defaults this to 02:00:00.
    at: i32,
}

/// The daylight-saving half of a rule.
#[derive(Copy, Clone)]
struct Dst {
    abbr: Abbr,
    /// Seconds to add to UTC while DST is in force.
    offset: i32,
    start: Transition,
    end: Transition,
}

/// A parsed POSIX `TZ` rule.
#[derive(Copy, Clone)]
pub struct TzRule {
    std_abbr: Abbr,
    /// Seconds to add to UTC outside DST.
    std_offset: i32,
    dst: Option<Dst>,
}

impl TzRule {
    /// UTC, used until a real rule is loaded.
    pub const UTC: TzRule = TzRule {
        std_abbr: Abbr {
            buf: [b'U', b'T', b'C', 0, 0, 0, 0, 0],
            len: 3,
        },
        std_offset: 0,
        dst: None,
    };

    /// Parses a POSIX `TZ` rule, returning `None` if it is malformed.
    pub fn parse(s: &str) -> Option<TzRule> {
        let b = s.as_bytes();
        let mut i = 0;

        let std_abbr = parse_abbr(b, &mut i)?;
        // Negated: POSIX states offsets as local-to-UTC, we keep UTC-to-local.
        let std_offset = -parse_offset(b, &mut i)?;

        if i >= b.len() {
            return Some(TzRule {
                std_abbr,
                std_offset,
                dst: None,
            });
        }

        let abbr = parse_abbr(b, &mut i)?;
        // An omitted DST offset means one hour ahead of standard time.
        let offset = if i < b.len() && b[i] != b',' {
            -parse_offset(b, &mut i)?
        } else {
            std_offset + 3600
        };

        // A DST abbreviation with no transition dates is unusable: POSIX says
        // the rules are then implementation-defined, so treat it as no DST
        // rather than guess.
        if i >= b.len() || b[i] != b',' {
            return Some(TzRule {
                std_abbr,
                std_offset,
                dst: None,
            });
        }
        i += 1;
        let start = parse_transition(b, &mut i)?;
        if i >= b.len() || b[i] != b',' {
            return None;
        }
        i += 1;
        let end = parse_transition(b, &mut i)?;

        Some(TzRule {
            std_abbr,
            std_offset,
            dst: Some(Dst {
                abbr,
                offset,
                start,
                end,
            }),
        })
    }

    /// Returns the UTC offset and abbreviation in force at a UTC instant.
    pub fn offset_at(&self, utc: i64) -> (i32, Abbr) {
        let Some(dst) = self.dst else {
            return (self.std_offset, self.std_abbr);
        };

        // Year in local terms. Transitions never fall near a year boundary in
        // practice, so standard offset is close enough to pick the right year.
        let (year, _, _) = civil_from_days((utc + self.std_offset as i64).div_euclid(86400));

        let start = self.transition_utc(&dst.start, year, self.std_offset);
        let end = self.transition_utc(&dst.end, year, dst.offset);

        let in_dst = if start <= end {
            // Northern hemisphere: DST sits inside the year.
            utc >= start && utc < end
        } else {
            // Southern hemisphere: DST straddles New Year.
            utc >= start || utc < end
        };

        if in_dst {
            (dst.offset, dst.abbr)
        } else {
            (self.std_offset, self.std_abbr)
        }
    }

    /// Returns the next DST transition strictly after `utc`, if the zone has one.
    ///
    /// Surfaced on the status page: a rule that has silently gone stale shows up
    /// as a transition on a date that no longer matches reality.
    pub fn next_transition(&self, utc: i64) -> Option<i64> {
        let dst = self.dst?;
        let (year, _, _) = civil_from_days((utc + self.std_offset as i64).div_euclid(86400));

        // Check this year and the next, so December lookups still find March.
        let mut best: Option<i64> = None;
        for y in [year, year + 1] {
            for t in [
                self.transition_utc(&dst.start, y, self.std_offset),
                self.transition_utc(&dst.end, y, dst.offset),
            ] {
                if t > utc && best.is_none_or(|b| t < b) {
                    best = Some(t);
                }
            }
        }
        best
    }

    /// Converts a transition rule into the UTC instant it fires.
    ///
    /// `offset_before` is the UTC offset in force immediately *before* the
    /// transition, because POSIX states the transition time in local wall-clock
    /// terms: standard time when springing forward, DST when falling back.
    fn transition_utc(&self, t: &Transition, year: i32, offset_before: i32) -> i64 {
        let day = match t.when {
            When::JulianNoLeap(n) => {
                // 1..=365, skipping 29 February entirely.
                let n = n.clamp(1, 365) as i32;
                let mut doy = n - 1;
                if is_leap(year) && n >= 60 {
                    doy += 1;
                }
                days_from_civil(year, 1, 1) + doy as i64
            }
            When::ZeroBased(n) => {
                let len = if is_leap(year) { 366 } else { 365 };
                days_from_civil(year, 1, 1) + (n as i32).min(len - 1) as i64
            }
            When::MonthWeekDay { month, week, day } => {
                nth_weekday(year, month, week, day)
            }
        };
        day * 86400 + t.at as i64 - offset_before as i64
    }
}

/// Reads an abbreviation: either 3+ letters, or anything inside `<...>`.
fn parse_abbr(b: &[u8], i: &mut usize) -> Option<Abbr> {
    let mut out = Abbr::default();
    if *i < b.len() && b[*i] == b'<' {
        *i += 1;
        while *i < b.len() && b[*i] != b'>' {
            if (out.len as usize) < MAX_ABBR {
                out.buf[out.len as usize] = b[*i];
                out.len += 1;
            }
            *i += 1;
        }
        // Unterminated bracket.
        if *i >= b.len() {
            return None;
        }
        *i += 1;
    } else {
        while *i < b.len() && b[*i].is_ascii_alphabetic() {
            if (out.len as usize) < MAX_ABBR {
                out.buf[out.len as usize] = b[*i];
                out.len += 1;
            }
            *i += 1;
        }
    }
    (out.len >= 3).then_some(out)
}

/// Reads `[+|-]hh[:mm[:ss]]` as seconds, keeping POSIX's sign convention.
fn parse_offset(b: &[u8], i: &mut usize) -> Option<i32> {
    let neg = match b.get(*i) {
        Some(b'-') => {
            *i += 1;
            true
        }
        Some(b'+') => {
            *i += 1;
            false
        }
        _ => false,
    };
    let h = parse_num(b, i)?;
    let mut secs = h * 3600;
    if b.get(*i) == Some(&b':') {
        *i += 1;
        secs += parse_num(b, i)? * 60;
        if b.get(*i) == Some(&b':') {
            *i += 1;
            secs += parse_num(b, i)?;
        }
    }
    Some(if neg { -secs } else { secs })
}

/// Reads a transition date and its optional `/time` suffix.
fn parse_transition(b: &[u8], i: &mut usize) -> Option<Transition> {
    let when = match b.get(*i) {
        Some(b'M') => {
            *i += 1;
            let month = parse_num(b, i)? as u8;
            if b.get(*i) != Some(&b'.') {
                return None;
            }
            *i += 1;
            let week = parse_num(b, i)? as u8;
            if b.get(*i) != Some(&b'.') {
                return None;
            }
            *i += 1;
            let day = parse_num(b, i)? as u8;
            if !(1..=12).contains(&month) || !(1..=5).contains(&week) || day > 6 {
                return None;
            }
            When::MonthWeekDay { month, week, day }
        }
        Some(b'J') => {
            *i += 1;
            When::JulianNoLeap(parse_num(b, i)? as u16)
        }
        Some(c) if c.is_ascii_digit() => When::ZeroBased(parse_num(b, i)? as u16),
        _ => return None,
    };

    // POSIX defaults the transition to 02:00:00 local.
    let at = if b.get(*i) == Some(&b'/') {
        *i += 1;
        parse_offset(b, i)?
    } else {
        7200
    };

    Some(Transition { when, at })
}

/// Reads a run of ASCII digits.
fn parse_num(b: &[u8], i: &mut usize) -> Option<i32> {
    let start = *i;
    let mut v: i32 = 0;
    while let Some(c) = b.get(*i) {
        if !c.is_ascii_digit() {
            break;
        }
        v = v.checked_mul(10)?.checked_add((c - b'0') as i32)?;
        *i += 1;
    }
    (*i > start).then_some(v)
}

/// Is `y` a leap year in the proleptic Gregorian calendar?
pub fn is_leap(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

/// Days since 1970-01-01 for a Gregorian date.
///
/// Howard Hinnant's `days_from_civil`, which is exact for any year in range and
/// needs no lookup tables.
pub fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y } as i64;
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // 0..=399
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1; // 0..=365
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // 0..=146096
    era * 146097 + doe - 719468
}

/// Gregorian date from days since 1970-01-01. The inverse of [`days_from_civil`].
pub fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097; // 0..=146096
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // 0..=399
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // 0..=365
    let mp = (5 * doy + 2) / 153; // 0..=11
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // 1..=31
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // 1..=12
    ((if m <= 2 { y + 1 } else { y }) as i32, m, d)
}

/// Day of week for a day count, 0 = Monday.
///
/// Matches the weekday indexing the alarm's recurrence bitmask uses, which in
/// turn matches MicroPython's `RTC.datetime()`.
pub fn weekday_from_days(z: i64) -> u8 {
    // Day 0 (1970-01-01) was a Thursday, index 3 when Monday is 0.
    (z + 3).rem_euclid(7) as u8
}

/// Days since the epoch for the `week`-th `weekday` of a month.
///
/// `week` is 1..=5 with 5 meaning "last", and `weekday` is 0..=6 with 0 being
/// Sunday — POSIX's `Mm.w.d` convention, not this module's Monday-first one.
fn nth_weekday(year: i32, month: u8, week: u8, weekday: u8) -> i64 {
    let first = days_from_civil(year, month as u32, 1);
    // Convert Monday-first to Sunday-first to match the rule's convention.
    let first_dow = (weekday_from_days(first) + 1) % 7;
    let delta = (weekday + 7 - first_dow) % 7;
    let mut day = first + delta as i64;
    // Week 5 means the last such weekday, which may be the 4th.
    let advance = (week - 1).min(4) as i64;
    day += advance * 7;
    let len = days_in_month(year, month) as i64;
    while day >= first + len {
        day -= 7;
    }
    day
}

/// Length of a month in days.
fn days_in_month(year: i32, month: u8) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap(year) => 29,
        2 => 28,
        _ => 30,
    }
}

/// Broken-down local time.
#[derive(Copy, Clone, defmt::Format)]
pub struct DateTime {
    /// Gregorian year.
    pub year: i32,
    /// Month, 1..=12.
    pub month: u8,
    /// Day of month, 1..=31.
    pub day: u8,
    /// Hour, 0..=23.
    pub hour: u8,
    /// Minute, 0..=59.
    pub minute: u8,
    /// Second, 0..=59.
    pub second: u8,
    /// Day of week, 0 = Monday.
    pub weekday: u8,
}

/// Splits a UTC instant into local date and time under `rule`.
pub fn local(utc: i64, rule: &TzRule) -> (DateTime, Abbr, i32) {
    let (offset, abbr) = rule.offset_at(utc);
    let local = utc + offset as i64;
    let days = local.div_euclid(86400);
    let secs = local.rem_euclid(86400);
    let (year, month, day) = civil_from_days(days);
    (
        DateTime {
            year,
            month: month as u8,
            day: day as u8,
            hour: (secs / 3600) as u8,
            minute: ((secs / 60) % 60) as u8,
            second: (secs % 60) as u8,
            weekday: weekday_from_days(days),
        },
        abbr,
        offset,
    )
}
