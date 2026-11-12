//! Where a live segment starts on the stream's own clock: the media time of
//! its first sample, read from the first few kilobytes of it.
//!
//! A playlist says how long each segment is (`#EXTINF`), and FFmpeg finds a
//! segment by those lengths, but plays it by the times inside it. Over the
//! days a running stream can be rewound, the two drift apart (24.5 s over
//! five days, measured: the stream had breaks), so the timeline reads both
//! ends of the window here to make its playlist say the media's time.
//!
//! Two shapes, both as YouTube sends live segments: fragmented MP4 (audio,
//! H.264), where it is the fragment's `tfdt` over its track's `mdhd`
//! timescale; and WebM (VP9), the first Cluster's Timecode times the
//! segment's TimecodeScale. Anything else, or bytes that end too soon, is
//! no answer.

/// How much of a segment is read for this: its header and first fragment
/// or cluster head are well within it (1.1 KB and 0.5 KB, measured).
pub const PEEK: u64 = 8 << 10;

/// The media time, in seconds, at which the segment `bytes` begins with
/// (only its start is needed: see `PEEK`).
pub fn start_of(bytes: &[u8]) -> Option<f64> {
    if bytes.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        webm_start(bytes)
    } else {
        mp4_start(bytes)
    }
}

/// An MP4 box at `at`: its type, where its body starts, and where it ends
/// (which may be past the bytes there are).
fn mp4_box(b: &[u8], at: usize) -> Option<([u8; 4], usize, usize)> {
    let size = u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?) as u64;
    let kind: [u8; 4] = b.get(at + 4..at + 8)?.try_into().ok()?;
    let (size, body) = match size {
        1 => (
            u64::from_be_bytes(b.get(at + 8..at + 16)?.try_into().ok()?),
            at + 16,
        ),
        0 => (u64::MAX, at + 8),
        n => (n, at + 8),
    };
    if size < (body - at) as u64 {
        return None;
    }
    let end = usize::try_from(size)
        .ok()
        .and_then(|s| at.checked_add(s))
        .unwrap_or(usize::MAX);
    Some((kind, body, end))
}

fn mp4_start(b: &[u8]) -> Option<f64> {
    let (mut timescale, mut decode) = (None, None);
    mp4_walk(b, 0, b.len(), &mut timescale, &mut decode);
    match (timescale?, decode?) {
        (0, _) => None,
        (scale, t) => Some(t as f64 / scale as f64),
    }
}

/// Looks through the boxes in `b[from..to]` (and inside those that hold
/// others) for the first `mdhd`'s timescale and the first `tfdt`'s time.
fn mp4_walk(b: &[u8], from: usize, to: usize, scale: &mut Option<u32>, decode: &mut Option<u64>) {
    let mut at = from;
    while at + 8 <= to.min(b.len()) && (scale.is_none() || decode.is_none()) {
        let Some((kind, body, end)) = mp4_box(b, at) else {
            return;
        };
        match &kind {
            b"moov" | b"trak" | b"mdia" | b"moof" | b"traf" => {
                mp4_walk(b, body, end.min(to), scale, decode);
            }
            b"mdhd" if scale.is_none() => {
                // Version 1 has 64-bit creation and modification times.
                let Some(&v) = b.get(body) else {
                    return;
                };
                let at = body + 4 + if v == 1 { 16 } else { 8 };
                *scale = b
                    .get(at..at + 4)
                    .map(|s| u32::from_be_bytes(s.try_into().unwrap_or_default()));
            }
            b"tfdt" if decode.is_none() => {
                let Some(&v) = b.get(body) else {
                    return;
                };
                *decode = if v == 1 {
                    b.get(body + 4..body + 12)
                        .map(|s| u64::from_be_bytes(s.try_into().unwrap_or_default()))
                } else {
                    b.get(body + 4..body + 8)
                        .map(|s| u32::from_be_bytes(s.try_into().unwrap_or_default()) as u64)
                };
            }
            _ => {}
        }
        if end <= at {
            return;
        }
        at = end;
    }
}

/// An EBML variable-length integer at `at`: its value (with the length
/// marker kept, for an ID) or without it (for a size), and its length.
fn vint(b: &[u8], at: usize, keep_marker: bool) -> Option<(u64, usize)> {
    let first = *b.get(at)?;
    let len = first.leading_zeros() as usize + 1;
    if len > 8 {
        return None;
    }
    let mut v = if keep_marker {
        first as u64
    } else {
        (first as u16 & (0xFF >> len)) as u64
    };
    for i in 1..len {
        v = (v << 8) | *b.get(at + i)? as u64;
    }
    Some((v, len))
}

/// An unsigned EBML integer of `len` bytes at `at`.
fn uint(b: &[u8], at: usize, len: usize) -> Option<u64> {
    if len > 8 {
        return None;
    }
    let mut v = 0u64;
    for i in 0..len {
        v = (v << 8) | *b.get(at + i)? as u64;
    }
    Some(v)
}

const EBML_SEGMENT: u64 = 0x1853_8067;
const EBML_INFO: u64 = 0x1549_A966;
const EBML_TIMECODE_SCALE: u64 = 0x2A_D7B1;
const EBML_CLUSTER: u64 = 0x1F43_B675;
const EBML_TIMECODE: u64 = 0xE7;

fn webm_start(b: &[u8]) -> Option<f64> {
    // Nanoseconds per Timecode unit; a millisecond unless the segment says.
    let mut scale = 1_000_000u64;
    let mut at = 0;
    // The levels entered, by where they end (an unknown size: the end).
    while at < b.len() {
        let (id, id_len) = vint(b, at, true)?;
        let (size, size_len) = vint(b, at + id_len, false)?;
        let unknown = size == (1u64 << (7 * size_len)) - 1;
        let body = at + id_len + size_len;
        match id {
            // Entered: what is in them follows.
            EBML_SEGMENT | EBML_INFO | EBML_CLUSTER => at = body,
            EBML_TIMECODE_SCALE => {
                scale = uint(b, body, size as usize)?;
                at = body + size as usize;
            }
            EBML_TIMECODE => {
                let t = uint(b, body, size as usize)?;
                return Some(t as f64 * scale as f64 / 1e9);
            }
            // Anything else is stepped over, which an unknown size can't be.
            _ if unknown => return None,
            _ => at = body.checked_add(usize::try_from(size).ok()?)?,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mp4_box_of(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut b = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        b.extend_from_slice(kind);
        b.extend_from_slice(body);
        b
    }

    /// A fragmented MP4 segment's start, laid out as YouTube's live audio
    /// is: `ftyp`, `moov` (with the track's `mdhd`), an `emsg`, `moof`
    /// (with `tfdt`), then `mdat`, cut short.
    fn fmp4(timescale: u32, decode: u64, v1: bool) -> Vec<u8> {
        let mut mdhd = vec![if v1 { 1 } else { 0 }, 0, 0, 0];
        mdhd.extend_from_slice(&[0; 16][..if v1 { 16 } else { 8 }]);
        mdhd.extend_from_slice(&timescale.to_be_bytes());
        mdhd.extend_from_slice(&[0; 8]);
        let mdia = mp4_box_of(
            b"mdia",
            &[mp4_box_of(b"hdlr", &[0; 24]), mp4_box_of(b"mdhd", &mdhd)].concat(),
        );
        let trak = mp4_box_of(b"trak", &[mp4_box_of(b"tkhd", &[0; 84]), mdia].concat());
        let moov = mp4_box_of(b"moov", &[mp4_box_of(b"mvhd", &[0; 100]), trak].concat());
        let mut tfdt = vec![if v1 { 1 } else { 0 }, 0, 0, 0];
        if v1 {
            tfdt.extend_from_slice(&decode.to_be_bytes());
        } else {
            tfdt.extend_from_slice(&(decode as u32).to_be_bytes());
        }
        let traf = mp4_box_of(
            b"traf",
            &[mp4_box_of(b"tfhd", &[0; 20]), mp4_box_of(b"tfdt", &tfdt)].concat(),
        );
        let moof = mp4_box_of(b"moof", &[mp4_box_of(b"mfhd", &[0; 8]), traf].concat());
        let mut b = [
            mp4_box_of(b"ftyp", &[0; 16]),
            moov,
            mp4_box_of(b"emsg", &[0; 40]),
            moof,
        ]
        .concat();
        // The mdat's head, saying more than there is.
        b.extend_from_slice(&80_000u32.to_be_bytes());
        b.extend_from_slice(b"mdat");
        b.extend_from_slice(&[0; 100]);
        b
    }

    #[test]
    fn an_mp4_segment_starts_at_its_fragment_decode_time() {
        // 20846380.071474 s at 44.1 kHz, as a real segment said.
        let t = 919_325_361_152u64;
        let got = start_of(&fmp4(44_100, t, true)).unwrap();
        assert!((got - t as f64 / 44_100.0).abs() < 1e-9, "{got}");
        let got = start_of(&fmp4(90_000, 900_000, false)).unwrap();
        assert!((got - 10.0).abs() < 1e-9, "{got}");
    }

    /// The start of a WebM segment as YouTube's live video is: the EBML
    /// header, a Segment and a Cluster of unknown size, Info with its
    /// TimecodeScale, and the Cluster's Timecode in 8 bytes.
    fn webm(scale: Option<u32>, timecode: u64) -> Vec<u8> {
        let mut b = vec![0x1A, 0x45, 0xDF, 0xA3, 0x84, 0x42, 0x86, 0x81, 0x01];
        b.extend_from_slice(&[
            0x18, 0x53, 0x80, 0x67, 0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
        ]);
        // Info, with or without its scale.
        let mut info = Vec::new();
        if let Some(s) = scale {
            info.extend_from_slice(&[0x2A, 0xD7, 0xB1, 0x84]);
            info.extend_from_slice(&s.to_be_bytes());
        }
        info.extend_from_slice(&[0x44, 0x89, 0x84, 0, 0, 0, 0]);
        b.extend_from_slice(&[0x15, 0x49, 0xA9, 0x66, 0x80 | info.len() as u8]);
        b.extend_from_slice(&info);
        // Tracks, stepped over.
        b.extend_from_slice(&[0x16, 0x54, 0xAE, 0x6B, 0x85, 1, 2, 3, 4, 5]);
        b.extend_from_slice(&[
            0x1F, 0x43, 0xB6, 0x75, 0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
        ]);
        b.extend_from_slice(&[0xE7, 0x88]);
        b.extend_from_slice(&timecode.to_be_bytes());
        b.extend_from_slice(&[0xA3, 0x84, 0, 0, 0, 0]);
        b
    }

    #[test]
    fn a_webm_segment_starts_at_its_first_cluster() {
        // 0x4f4454928 ms, as a real segment said.
        let got = start_of(&webm(Some(1_000_000), 0x0004_F445_4928)).unwrap();
        assert!(
            (got - 0x0004_F445_4928u64 as f64 / 1000.0).abs() < 1e-6,
            "{got}"
        );
        // A millisecond when it doesn't say.
        assert_eq!(start_of(&webm(None, 2_500)), Some(2.5));
        assert_eq!(start_of(&webm(Some(500_000), 2_500)), Some(1.25));
    }

    #[test]
    fn bytes_that_end_too_soon_or_are_neither_say_nothing() {
        let whole = fmp4(44_100, 441_000, true);
        let tfdt = whole.windows(4).position(|w| w == b"tfdt").unwrap();
        assert_eq!(start_of(&whole[..tfdt + 6]), None);
        let w = webm(Some(1_000_000), 5_000);
        assert_eq!(start_of(&w[..w.len() - 10]), None);
        assert_eq!(start_of(b""), None);
        assert_eq!(start_of(b"not a segment at all"), None);
        assert_eq!(start_of(&[0x1A, 0x45, 0xDF, 0xA3, 0xFF]), None);
    }

    /// Real segments, when they are at hand (not in the repository): the
    /// directory in `QVEIL_SEGMENTS`, its `*.m4a` and `*.mkv`, each against
    /// what ffprobe says in `<name>.start`.
    #[test]
    fn real_segments_if_given() {
        let Some(dir) = std::env::var_os("QVEIL_SEGMENTS") else {
            return;
        };
        let mut seen = 0;
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if !p.extension().is_some_and(|x| x == "m4a" || x == "mkv") {
                continue;
            }
            let Ok(said) = std::fs::read_to_string(p.with_extension("start")) else {
                continue;
            };
            let bytes = std::fs::read(&p).unwrap();
            let head = &bytes[..bytes.len().min(PEEK as usize)];
            let got = start_of(head).unwrap_or_else(|| panic!("{p:?}"));
            let want: f64 = said.trim().parse().unwrap();
            assert!((got - want).abs() < 0.01, "{p:?}: {got} vs {want}");
            seen += 1;
        }
        assert!(seen > 0);
    }
}
