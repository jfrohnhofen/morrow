//! Shared DNS wire-format helpers.
//!
//! Both the captive portal responder and the mDNS responder speak DNS, so the
//! name encoding lives here rather than in each.

/// Walks a name starting at `pos`, returning the offset just past it.
///
/// Compression pointers terminate the name, as they always point backwards and
/// nothing after them belongs to this name.
pub fn skip_name(buf: &[u8], mut pos: usize) -> Option<usize> {
    loop {
        let len = *buf.get(pos)? as usize;
        if len == 0 {
            return Some(pos + 1);
        }
        // Top two bits set marks a compression pointer, which is two bytes.
        if len & 0xC0 == 0xC0 {
            return Some(pos + 2);
        }
        pos += 1 + len;
    }
}

/// Compares a wire-format name against dot-free labels, ASCII case-insensitively.
///
/// DNS names are case-insensitive, and mDNS queriers do vary the case, so a
/// byte comparison would miss legitimate queries.
pub fn name_matches(buf: &[u8], mut pos: usize, labels: &[&str]) -> bool {
    for want in labels {
        let Some(&len) = buf.get(pos) else {
            return false;
        };
        let len = len as usize;
        if len == 0 || len & 0xC0 == 0xC0 || len != want.len() {
            return false;
        }
        let Some(got) = buf.get(pos + 1..pos + 1 + len) else {
            return false;
        };
        if !got.eq_ignore_ascii_case(want.as_bytes()) {
            return false;
        }
        pos += 1 + len;
    }
    buf.get(pos) == Some(&0)
}

/// Writes dot-free labels in wire format, returning bytes written.
pub fn write_name(out: &mut [u8], at: usize, labels: &[&str]) -> Option<usize> {
    let mut n = at;
    for label in labels {
        let len = label.len();
        if len > 63 {
            return None;
        }
        *out.get_mut(n)? = len as u8;
        out.get_mut(n + 1..n + 1 + len)?.copy_from_slice(label.as_bytes());
        n += 1 + len;
    }
    *out.get_mut(n)? = 0;
    Some(n + 1 - at)
}

/// DNS record type for an IPv4 address.
pub const TYPE_A: u16 = 1;
/// DNS record type for a service pointer.
pub const TYPE_PTR: u16 = 12;
/// DNS record type for a service location.
pub const TYPE_SRV: u16 = 33;
/// DNS record type for service metadata.
pub const TYPE_TXT: u16 = 16;
/// DNS record type matching anything.
pub const TYPE_ANY: u16 = 255;
/// DNS class for the internet.
pub const CLASS_IN: u16 = 1;
