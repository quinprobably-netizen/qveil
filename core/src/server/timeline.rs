//! A live stream's playlists: what the player is told exists.
//!
//! A running stream's segments are numbered, and the playlist lists the ones
//! the engine knows exist (`oldest..=head`). It is an event playlist, so the
//! player rewinds over the whole of it and keeps asking for the rest until
//! `#EXT-X-ENDLIST` says it is over. A stream that is only a list of
//! fragments plays as a finished video.
//!
//! This is the pure part; it writes text and never fetches anything.

// Nothing calls these until the live play path serves playlists.
#![cfg_attr(
    not(test),
    expect(dead_code, reason = "the live play path serves these playlists next")
)]

use std::fmt::Write;

/// How far back a running stream can be rewound: five days.
pub(super) const WINDOW_SECS: f64 = 432_000.0;

/// Segments kept back from the window's edge, so the oldest one listed has
/// not just been dropped upstream by the time the player asks for it.
pub(super) const BACK_MARGIN: u64 = 60;

/// Which segments of a running stream exist.
pub(super) struct Span {
    /// The oldest segment listed.
    pub oldest: u64,
    /// The newest segment that exists, inclusive.
    pub head: u64,
    /// The stream is over: the playlist ends.
    pub ended: bool,
}

/// The oldest segment a playlist lists: the rewind window back from `head`,
/// plus the margin, and never past `head`.
pub(super) fn oldest_for(head: u64, target: f64) -> u64 {
    let window = (WINDOW_SECS / target) as u64;
    head.saturating_sub(window)
        .saturating_add(BACK_MARGIN)
        .min(head)
}

/// The playlist of a running (or just ended) stream's track: segment `n` is
/// `{prefix}{n}.{ext}`, each `target` seconds long.
pub(super) fn playlist(span: &Span, target: f64, prefix: &str, ext: &str) -> String {
    let mut out = header("EVENT", target_duration(target), span.oldest);
    for n in span.oldest..=span.head {
        entry(&mut out, target, &format!("{prefix}{n}.{ext}"));
    }
    if span.ended {
        out.push_str("#EXT-X-ENDLIST\n");
    }
    out
}

/// The playlist of a track that is a list of fragments: a finished video,
/// whose segment `i` (counting from 0) is `durations[i]` seconds long.
pub(super) fn listed_playlist(durations: &[f64], prefix: &str, ext: &str) -> String {
    let longest = durations.iter().copied().fold(0.0, f64::max);
    let mut out = header("VOD", target_duration(longest), 0);
    for (i, duration) in durations.iter().enumerate() {
        entry(&mut out, *duration, &format!("{prefix}{i}.{ext}"));
    }
    out.push_str("#EXT-X-ENDLIST\n");
    out
}

/// A playlist's target duration: whole seconds, rounded up, at least 1.
fn target_duration(secs: f64) -> u64 {
    secs.ceil().max(1.0) as u64
}

fn header(kind: &str, target: u64, first: u64) -> String {
    format!(
        "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:{target}\n\
         #EXT-X-PLAYLIST-TYPE:{kind}\n#EXT-X-MEDIA-SEQUENCE:{first}\n"
    )
}

fn entry(out: &mut String, duration: f64, uri: &str) {
    // Writing to a String cannot fail.
    let _ = write!(out, "#EXTINF:{duration:.3},\n{uri}\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUNNING: &str = "\
#EXTM3U
#EXT-X-VERSION:7
#EXT-X-TARGETDURATION:5
#EXT-X-PLAYLIST-TYPE:EVENT
#EXT-X-MEDIA-SEQUENCE:10
#EXTINF:5.000,
v/10.mkv
#EXTINF:5.000,
v/11.mkv
#EXTINF:5.000,
v/12.mkv
";

    fn span(ended: bool) -> Span {
        Span {
            oldest: 10,
            head: 12,
            ended,
        }
    }

    #[test]
    fn a_running_playlist_is_an_event_list_of_known_segments() {
        assert_eq!(playlist(&span(false), 5.0, "v/", "mkv"), RUNNING);
    }

    #[test]
    fn an_ended_playlist_ends_with_endlist() {
        assert_eq!(
            playlist(&span(true), 5.0, "v/", "mkv"),
            format!("{RUNNING}#EXT-X-ENDLIST\n")
        );
    }

    #[test]
    fn the_oldest_segment_is_five_days_back_plus_a_margin() {
        assert_eq!(oldest_for(476_821, 5.0), 476_821 - 86_400 + 60);
        assert_eq!(oldest_for(100, 5.0), 60);
        // A stream younger than the margin lists only what exists.
        assert_eq!(oldest_for(10, 5.0), 10);
    }

    #[test]
    fn a_fragment_list_plays_as_a_finished_video() {
        assert_eq!(
            listed_playlist(&[5.0, 5.0, 2.5], "a/", "m4a"),
            "\
#EXTM3U
#EXT-X-VERSION:7
#EXT-X-TARGETDURATION:5
#EXT-X-PLAYLIST-TYPE:VOD
#EXT-X-MEDIA-SEQUENCE:0
#EXTINF:5.000,
a/0.m4a
#EXTINF:5.000,
a/1.m4a
#EXTINF:2.500,
a/2.m4a
#EXT-X-ENDLIST
"
        );
    }

    #[test]
    fn the_target_duration_is_rounded_up() {
        assert!(playlist(&span(false), 5.5, "v/", "mkv").contains("#EXT-X-TARGETDURATION:6\n"));
        assert!(listed_playlist(&[2.0, 6.2], "v/", "mp4").contains("#EXT-X-TARGETDURATION:7\n"));
    }
}
