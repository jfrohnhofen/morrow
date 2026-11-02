//! Parsing of `application/x-www-form-urlencoded` bodies.
//!
//! The setup page is served to iOS's Captive Network Assistant, which is a
//! cut-down WebKit view. Plain form posts work there; a JSON API driven by
//! `fetch` is a gamble. So the device parses form bodies, and this does it
//! without allocating.

/// Decodes one percent-encoded form value into `out`.
///
/// Returns the decoded length, or `None` if the value does not fit. `+` becomes
/// a space and `%XX` becomes a byte; a malformed escape is passed through
/// literally rather than rejected, since a half-typed password should not 500.
pub fn decode(src: &str, out: &mut [u8]) -> Option<usize> {
    let b = src.as_bytes();
    let mut i = 0;
    let mut n = 0;
    while i < b.len() {
        if n >= out.len() {
            return None;
        }
        match b[i] {
            b'+' => {
                out[n] = b' ';
                i += 1;
            }
            b'%' if i + 2 < b.len() => match (hex(b[i + 1]), hex(b[i + 2])) {
                (Some(h), Some(l)) => {
                    out[n] = h << 4 | l;
                    i += 3;
                }
                _ => {
                    out[n] = b[i];
                    i += 1;
                }
            },
            c => {
                out[n] = c;
                i += 1;
            }
        }
        n += 1;
    }
    Some(n)
}

/// Value of a hex digit.
fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// Iterates the raw `key=value` pairs of a form body.
///
/// Keys and values are still percent-encoded; run [`decode`] over whichever
/// values you actually need.
pub struct Fields<'a> {
    rest: &'a str,
}

impl<'a> Fields<'a> {
    /// Starts iterating `body`.
    pub fn new(body: &'a str) -> Fields<'a> {
        Fields { rest: body }
    }
}

impl<'a> Iterator for Fields<'a> {
    type Item = (&'a str, &'a str);

    fn next(&mut self) -> Option<(&'a str, &'a str)> {
        while !self.rest.is_empty() {
            let (pair, rest) = match self.rest.find('&') {
                Some(i) => (&self.rest[..i], &self.rest[i + 1..]),
                None => (self.rest, ""),
            };
            self.rest = rest;
            if pair.is_empty() {
                continue;
            }
            return Some(match pair.find('=') {
                Some(i) => (&pair[..i], &pair[i + 1..]),
                None => (pair, ""),
            });
        }
        None
    }
}

/// Returns the raw value of `key`, or `None` if absent.
pub fn field<'a>(body: &'a str, key: &str) -> Option<&'a str> {
    Fields::new(body).find(|(k, _)| *k == key).map(|(_, v)| v)
}

/// Decodes `key` into `out`, returning it as a string.
///
/// `None` if the field is absent, too long, or not valid UTF-8 once decoded.
pub fn text<'a>(body: &str, key: &str, out: &'a mut [u8]) -> Option<&'a str> {
    let raw = field(body, key)?;
    let n = decode(raw, out)?;
    core::str::from_utf8(&out[..n]).ok()
}

/// Parses `key` as an unsigned integer, clamped to `max`.
///
/// Clamping rather than rejecting keeps a fat-fingered value from wedging the
/// form: the settings that come back are always usable.
pub fn number(body: &str, key: &str, max: u32) -> Option<u32> {
    let raw = field(body, key)?;
    let mut v: u32 = 0;
    let mut any = false;
    for c in raw.bytes() {
        if !c.is_ascii_digit() {
            // Tolerate a trailing fragment rather than discarding the number.
            break;
        }
        any = true;
        v = v.saturating_mul(10).saturating_add((c - b'0') as u32);
    }
    any.then_some(v.min(max))
}

/// Is `key` present and not obviously falsey? Checkboxes post `on`.
pub fn flag(body: &str, key: &str) -> bool {
    matches!(field(body, key), Some(v) if v != "0" && v != "off" && v != "false")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decodes into a fixed buffer, as the firmware does.
    fn dec(s: &str) -> String {
        let mut buf = [0u8; 128];
        let n = decode(s, &mut buf).expect("fits");
        core::str::from_utf8(&buf[..n]).expect("utf-8").to_string()
    }

    #[test]
    fn plus_is_space_and_percent_is_a_byte() {
        assert_eq!(dec("hello+world"), "hello world");
        assert_eq!(dec("a%20b"), "a b");
        assert_eq!(dec("%41%42%43"), "ABC");
    }

    #[test]
    fn decodes_utf8_ssids() {
        // A real SSID: "Münchner Freiheit" as a browser would post it.
        assert_eq!(dec("M%C3%BCnchner+Freiheit"), "Münchner Freiheit");
    }

    #[test]
    fn malformed_escapes_pass_through_rather_than_failing() {
        // A half-typed passphrase must not take the whole form down.
        assert_eq!(dec("50%"), "50%");
        assert_eq!(dec("a%zzb"), "a%zzb");
    }

    #[test]
    fn overlong_values_are_rejected_not_truncated() {
        let mut small = [0u8; 4];
        assert_eq!(decode("abcdefgh", &mut small), None);
    }

    #[test]
    fn splits_fields() {
        let body = "hour=7&minute=30&mode=1";
        assert_eq!(field(body, "hour"), Some("7"));
        assert_eq!(field(body, "minute"), Some("30"));
        assert_eq!(field(body, "mode"), Some("1"));
        assert_eq!(field(body, "absent"), None);
    }

    #[test]
    fn tolerates_empty_and_valueless_fields() {
        let body = "&a=&&b&c=3";
        assert_eq!(field(body, "a"), Some(""));
        assert_eq!(field(body, "b"), Some(""));
        assert_eq!(field(body, "c"), Some("3"));
        assert_eq!(Fields::new(body).count(), 3);
    }

    #[test]
    fn numbers_clamp_instead_of_rejecting() {
        assert_eq!(number("h=7", "h", 23), Some(7));
        assert_eq!(number("h=99", "h", 23), Some(23));
        assert_eq!(number("h=", "h", 23), None);
        assert_eq!(number("h=abc", "h", 23), None);
        // Saturates rather than wrapping on absurd input.
        assert_eq!(number("h=99999999999999999999", "h", 240), Some(240));
    }

    #[test]
    fn empty_passphrase_is_distinguishable_from_absent() {
        // The status page relies on this: an empty field means "keep the
        // existing passphrase", an absent one means the form had no such input.
        let mut buf = [0u8; 64];
        assert_eq!(text("ssid=home&psk=", "psk", &mut buf), Some(""));
        assert_eq!(text("ssid=home", "psk", &mut buf), None);
    }

    #[test]
    fn flags_read_checkboxes() {
        assert!(flag("x=on", "x"));
        assert!(!flag("x=off", "x"));
        assert!(!flag("x=0", "x"));
        assert!(!flag("y=on", "x"));
    }
}
