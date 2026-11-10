//! YouTube's own search filters, for the rules they can stand in for.
//!
//! A search can carry a filter (the `sp` parameter of a results page), and
//! YouTube then leaves out what doesn't match before anything is sent. It is
//! only an optimisation: every rule is applied again here, exactly, so a
//! filter is used only when what it lets through contains everything the
//! rule would keep. The parameter is undocumented; what is here was read off
//! the links YouTube's own filter menu makes.

use super::read::Reading;

const DAY: i64 = 86_400;
/// YouTube's "over 20 minutes".
const LONG: u64 = 20 * 60;

/// The `sp` value for a search under `reading`, or `None` when no filter of
/// YouTube's can stand in for any of its rules.
///
/// It is a small protobuf message, base64-encoded: field 1 is the order (2:
/// by upload date), and field 2 holds the filters, in which field 1 is the
/// upload date (3: this week, 4: this month, 5: this year), field 2 the kind
/// of result (1: videos) and field 3 the length (2: over 20 minutes).
///
/// The date filter used is always one size larger than the rule needs:
/// YouTube doesn't say whether "this week" is the last seven days or the
/// week since Monday, and the larger one contains the rule either way.
pub fn sp(reading: &Reading, now: i64) -> Option<String> {
    let date = reading.since.and_then(|since| {
        let age = now - since;
        match age {
            _ if age <= DAY => Some(3),
            _ if age <= 7 * DAY => Some(4),
            _ if age <= 30 * DAY => Some(5),
            _ => None,
        }
    });
    let long = reading.min_len.is_some_and(|min| min >= LONG);
    if date.is_none() && !long && !reading.newest_first {
        return None;
    }
    let mut filters = Vec::new();
    if let Some(date) = date {
        filters.extend([0x08, date]);
    }
    filters.extend([0x10, 0x01]);
    if long {
        filters.extend([0x18, 0x02]);
    }
    let mut message = Vec::new();
    if reading.newest_first {
        message.extend([0x08, 0x02]);
    }
    message.extend([0x12, filters.len() as u8]);
    message.extend(filters);
    Some(base64(&message))
}

/// Standard base64, with padding. (A dozen bytes at most: not worth a crate.)
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
        for k in 0..4 {
            if k <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * k) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lens::read::read;

    const NOW: i64 = 1_794_312_000;

    fn of(text: &str) -> Option<String> {
        sp(&read(text, NOW), NOW)
    }

    #[test]
    fn known_values() {
        // Videos, over 20 minutes.
        assert_eq!(of("talks over 30 min").as_deref(), Some("EgQQARgC"));
        assert_eq!(of("talks over 20 min").as_deref(), Some("EgQQARgC"));
        // This year, videos.
        assert_eq!(of("talks past 2 weeks").as_deref(), Some("EgQIBRAB"));
        // This month, videos: a week fits in a month with room to spare.
        assert_eq!(of("talks this week").as_deref(), Some("EgQIBBAB"));
        // This week, videos.
        assert_eq!(of("talks today").as_deref(), Some("EgQIAxAB"));
        // Both.
        assert_eq!(
            of("talks today over 1 hour").as_deref(),
            Some("EgYIAxABGAI=")
        );
    }

    #[test]
    fn a_rule_youtube_has_no_filter_for_is_left_to_us() {
        assert_eq!(of("talks"), None);
        assert_eq!(of("talks over 10 min"), None);
        assert_eq!(of("talks under 3 min"), None);
        assert_eq!(of("talks since 2019"), None);
        assert_eq!(of("talks, no music, with shorts"), None);
    }

    #[test]
    fn newest_first_sorts_by_date() {
        assert_eq!(of("talks, newest first").as_deref(), Some("CAISAhAB"));
    }
}
