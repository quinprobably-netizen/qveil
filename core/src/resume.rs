//! Where unfinished videos were stopped, so they can be picked up again.
//!
//! This is a record of what was watched, which is why it is small and
//! short-lived: only a video that was left part-way has an entry, a video
//! that is finished or started over loses it at once, and every entry is
//! deleted `resume_days` after it was last written. With `resume_days = 0`
//! nothing is written and the file is removed.

use crate::store::{self, Paths};
use anyhow::Result;
use serde::{Deserialize, Serialize};

/// How many positions are kept; beyond it the oldest go.
pub const MAX: usize = 500;
/// The seconds at either end of a video that are not a position worth
/// keeping: the first few are "hasn't started", the last few "has finished".
pub const EDGE: u64 = 30;

const DAY: i64 = 86_400;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Position {
    pub id: String,
    /// Seconds into the video.
    pub pos: u64,
    /// The video's length in seconds.
    pub dur: u64,
    /// When this was written, in Unix seconds: what the expiry counts from.
    pub at: i64,
    /// So that the list can be shown without asking YouTube for anything.
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
}

#[derive(Default, Serialize, Deserialize)]
struct File {
    positions: Vec<Position>,
}

/// What became of a reported position.
#[derive(Debug, PartialEq, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Noted {
    Kept,
    /// The video is over: its entry is gone.
    Finished,
    /// The video has barely started: nothing is kept, and an older position
    /// is dropped, because the video was started over.
    TooEarly,
    /// Resume is turned off.
    Off,
    /// The player didn't know the length yet, so there is nothing to judge
    /// the position by.
    Unknown,
}

/// When `p` stops being offered and is deleted.
pub fn expires(p: &Position, days: u32) -> i64 {
    p.at + i64::from(days) * DAY
}

fn live(p: &Position, days: u32, now: i64) -> bool {
    expires(p, days) > now
}

/// The file, as far as it can be read. It is a convenience that a few
/// seconds of watching rebuild, so one that doesn't parse counts as empty
/// (and is replaced by the next write) instead of stopping a video.
fn load(paths: &Paths) -> Result<File> {
    store::load_cache(&paths.resume())
}

/// Writes what is still worth keeping: nothing expired, the newest `MAX`.
fn save(paths: &Paths, mut file: File, days: u32, now: i64) -> Result<()> {
    file.positions.retain(|p| live(p, days, now));
    file.positions.sort_by_key(|p| p.at);
    let excess = file.positions.len().saturating_sub(MAX);
    file.positions.drain(..excess);
    store::save_compact(&paths.resume(), &file)
}

/// Records where a video was stopped, or forgets it, as the position says.
pub fn note(paths: &Paths, days: u32, p: Position, now: i64) -> Result<Noted> {
    if days == 0 {
        return Ok(Noted::Off);
    }
    if p.dur == 0 || p.pos > p.dur {
        return Ok(Noted::Unknown);
    }
    let noted = if p.pos < EDGE {
        Noted::TooEarly
    } else if p.pos + EDGE >= p.dur || p.pos * 100 >= p.dur * 95 {
        Noted::Finished
    } else {
        Noted::Kept
    };
    let _lock = store::lock(paths)?;
    let mut file = load(paths)?;
    let had = file.positions.iter().any(|x| x.id == p.id);
    if noted != Noted::Kept && !had {
        // Nothing to add and nothing to take away.
        return Ok(noted);
    }
    file.positions.retain(|x| x.id != p.id);
    if noted == Noted::Kept {
        file.positions.push(p);
    }
    save(paths, file, days, now)?;
    Ok(noted)
}

/// The positions that are still offered, newest first.
pub fn list(paths: &Paths, days: u32, now: i64) -> Result<Vec<Position>> {
    let mut positions = load(paths)?.positions;
    positions.retain(|p| live(p, days, now));
    positions.sort_by_key(|p| std::cmp::Reverse(p.at));
    Ok(positions)
}

pub fn get(paths: &Paths, days: u32, id: &str, now: i64) -> Result<Option<Position>> {
    Ok(list(paths, days, now)?.into_iter().find(|p| p.id == id))
}

pub fn forget(paths: &Paths, id: &str) -> Result<()> {
    let _lock = store::lock(paths)?;
    let mut file = load(paths)?;
    let before = file.positions.len();
    file.positions.retain(|p| p.id != id);
    if file.positions.len() == before {
        return Ok(());
    }
    store::save_compact(&paths.resume(), &file)
}

/// Forgets every position: the file is removed.
pub fn clear(paths: &Paths) -> Result<()> {
    let _lock = store::lock(paths)?;
    match std::fs::remove_file(paths.resume()) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

/// Deletes what has expired, so that it doesn't sit on disk until the next
/// video is watched. With resume off, that is everything.
pub fn purge(paths: &Paths, days: u32, now: i64) -> Result<()> {
    if days == 0 {
        return clear(paths);
    }
    if !paths.resume().exists() {
        return Ok(());
    }
    let _lock = store::lock(paths)?;
    let file = load(paths)?;
    if file.positions.iter().all(|p| live(p, days, now)) {
        return Ok(());
    }
    save(paths, file, days, now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testing::TempDir;

    fn pos(id: &str, pos: u64, dur: u64, at: i64) -> Position {
        Position {
            id: id.into(),
            pos,
            dur,
            at,
            title: "T".into(),
            channel: None,
        }
    }

    #[test]
    fn a_position_is_kept_listed_and_expires() {
        let d = TempDir::new();
        let p = d.paths();
        assert_eq!(
            note(&p, 7, pos("aaaaaaaaaaa", 845, 2463, 1000), 1000).unwrap(),
            Noted::Kept
        );
        assert_eq!(get(&p, 7, "aaaaaaaaaaa", 1000).unwrap().unwrap().pos, 845);
        let last = 1000 + 7 * 86_400;
        assert_eq!(expires(&pos("aaaaaaaaaaa", 845, 2463, 1000), 7), last);
        assert!(get(&p, 7, "aaaaaaaaaaa", last - 1).unwrap().is_some());
        assert!(get(&p, 7, "aaaaaaaaaaa", last).unwrap().is_none());
        assert!(list(&p, 7, last).unwrap().is_empty());
    }

    #[test]
    fn the_ends_of_a_video_are_not_positions() {
        let d = TempDir::new();
        let p = d.paths();
        assert_eq!(
            note(&p, 7, pos("aaaaaaaaaaa", 29, 2463, 0), 0).unwrap(),
            Noted::TooEarly
        );
        assert_eq!(
            note(&p, 7, pos("aaaaaaaaaaa", 845, 2463, 0), 0).unwrap(),
            Noted::Kept
        );
        // The last 30 seconds.
        assert_eq!(
            note(&p, 7, pos("aaaaaaaaaaa", 2433, 2463, 0), 0).unwrap(),
            Noted::Finished
        );
        assert!(list(&p, 7, 0).unwrap().is_empty());
        // Past 95 %.
        assert_eq!(
            note(&p, 7, pos("bbbbbbbbbbb", 9500, 10_000, 0), 0).unwrap(),
            Noted::Finished
        );
        assert_eq!(
            note(&p, 7, pos("aaaaaaaaaaa", 845, 2463, 0), 0).unwrap(),
            Noted::Kept
        );
        assert_eq!(
            note(&p, 7, pos("aaaaaaaaaaa", 5, 2463, 1), 1).unwrap(),
            Noted::TooEarly
        );
        assert!(
            get(&p, 7, "aaaaaaaaaaa", 1).unwrap().is_none(),
            "starting over forgets the old position"
        );
    }

    #[test]
    fn an_unknown_duration_or_resume_off_saves_nothing() {
        let d = TempDir::new();
        let p = d.paths();
        assert_eq!(
            note(&p, 7, pos("aaaaaaaaaaa", 100, 0, 0), 0).unwrap(),
            Noted::Unknown
        );
        assert_eq!(
            note(&p, 7, pos("aaaaaaaaaaa", 500, 400, 0), 0).unwrap(),
            Noted::Unknown
        );
        assert_eq!(
            note(&p, 0, pos("aaaaaaaaaaa", 100, 400, 0), 0).unwrap(),
            Noted::Off
        );
        assert!(!p.resume().exists());
    }

    #[test]
    fn the_newest_five_hundred_are_kept_newest_first() {
        let d = TempDir::new();
        let p = d.paths();
        for i in 0..(MAX as i64 + 3) {
            note(&p, 365, pos(&format!("v{i:010}"), 100, 400, i), i).unwrap();
        }
        let all = list(&p, 365, 600).unwrap();
        assert_eq!(all.len(), MAX);
        assert_eq!(all[0].id, format!("v{:010}", MAX as i64 + 2));
        assert_eq!(all[MAX - 1].id, format!("v{:010}", 3));
    }

    #[test]
    fn a_later_position_replaces_the_earlier_one_for_the_same_video() {
        let d = TempDir::new();
        let p = d.paths();
        note(&p, 7, pos("aaaaaaaaaaa", 100, 400, 0), 0).unwrap();
        note(&p, 7, pos("aaaaaaaaaaa", 200, 400, 5), 5).unwrap();
        let all = list(&p, 7, 5).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!((all[0].pos, all[0].at), (200, 5));
    }

    #[test]
    fn a_damaged_file_is_replaced_and_the_file_is_private() {
        let d = TempDir::new();
        let p = d.paths();
        std::fs::create_dir_all(&p.data).unwrap();
        std::fs::write(p.resume(), b"{ not json").unwrap();
        assert!(list(&p, 7, 0).unwrap().is_empty());
        assert_eq!(
            note(&p, 7, pos("aaaaaaaaaaa", 100, 400, 0), 0).unwrap(),
            Noted::Kept
        );
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(p.resume()).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(list(&p, 7, 0).unwrap().len(), 1);
    }

    #[test]
    fn purging_drops_the_expired_and_zero_days_removes_the_file() {
        let d = TempDir::new();
        let p = d.paths();
        note(&p, 7, pos("aaaaaaaaaaa", 100, 400, 0), 0).unwrap();
        note(&p, 7, pos("bbbbbbbbbbb", 100, 400, 86_400 * 5), 86_400 * 5).unwrap();
        purge(&p, 7, 86_400 * 8).unwrap();
        let text = std::fs::read_to_string(p.resume()).unwrap();
        assert!(!text.contains("aaaaaaaaaaa") && text.contains("bbbbbbbbbbb"));
        assert_eq!(list(&p, 7, 86_400 * 8).unwrap().len(), 1);
        purge(&p, 0, 0).unwrap();
        assert!(!p.resume().exists());
        // Nothing to purge is not an error.
        purge(&p, 7, 0).unwrap();
        forget(&p, "aaaaaaaaaaa").unwrap();
        clear(&p).unwrap();
    }
}
