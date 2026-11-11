//! What was watched, kept only while the history is on.

use crate::store::{self, Paths};
use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
pub struct History {
    pub watched: Vec<Watched>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Watched {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    pub at: i64,
}

const HISTORY_CAP: usize = 5000;

pub fn load_history(paths: &Paths) -> Result<History> {
    store::load(&paths.history())
}

/// Adds `w` to the history, in place of the same video's entry if it has
/// one. A video played without its channel's name (from a pasted link,
/// with the quick lookup on: see `ytdlp::LOOKUP`) keeps the name its entry
/// had.
pub fn record_history(paths: &Paths, mut w: Watched) -> Result<()> {
    let _lock = store::lock(paths)?;
    let mut h = load_history(paths)?;
    if w.channel.is_none() {
        w.channel = h
            .watched
            .iter()
            .find(|x| x.id == w.id)
            .and_then(|x| x.channel.clone());
    }
    h.watched.retain(|x| x.id != w.id);
    h.watched.push(w);
    let excess = h.watched.len().saturating_sub(HISTORY_CAP);
    h.watched.drain(..excess);
    store::save(&paths.history(), &h)
}

/// Names the channel of video `id` in its history entry, if it has one
/// without a name: one found after it was played.
pub fn name_in_history(paths: &Paths, id: &str, channel: &str) -> Result<()> {
    let _lock = store::lock(paths)?;
    if !paths.history().exists() {
        return Ok(());
    }
    let mut h = load_history(paths)?;
    let Some(w) = h
        .watched
        .iter_mut()
        .find(|x| x.id == id && x.channel.is_none())
    else {
        return Ok(());
    };
    w.channel = Some(channel.to_string());
    store::save(&paths.history(), &h)
}

pub fn clear_history(paths: &Paths) -> Result<()> {
    let _lock = store::lock(paths)?;
    match std::fs::remove_file(paths.history()) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
        _ => {}
    }
    // A write that a crash cut short left its temp file, with the history
    // it was writing: that is part of what was asked to be forgotten.
    store::sweep_temps(&paths.data, store::TEMP_ABANDONED);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testing::TempDir;
    use crate::store::{self, Paths};
    use std::fs;

    #[test]
    fn clearing_the_history_removes_what_a_write_cut_short_by_a_crash_left_too() {
        let d = TempDir::new();
        let paths = d.paths();
        store::save(
            &paths.history(),
            &History {
                watched: vec![Watched {
                    id: "dQw4w9WgXcQ".into(),
                    title: "T".into(),
                    channel: None,
                    at: 1,
                }],
            },
        )
        .unwrap();
        // What a process that died while writing the history left, and the
        // temp file of one that is still running, which isn't for us to take.
        let crashed = paths
            .data
            .join(".history.json.99999999-0123456789abcdef.tmp");
        fs::write(&crashed, br#"{"watched":[{"id":"dQw4w9WgXcQ""#).unwrap();
        let writing = paths.data.join(format!(
            ".history.json.{}-0123456789abcdef.tmp",
            std::process::id()
        ));
        fs::write(&writing, b"{").unwrap();
        clear_history(&paths).unwrap();
        assert!(!paths.history().exists());
        assert!(!crashed.exists(), "the crashed write's temp file");
        assert!(writing.exists());
    }

    #[test]
    fn a_channel_once_known_is_never_taken_out_of_the_history() {
        let dir = TempDir::new();
        let paths = dir.paths();
        let w = |channel: Option<&str>, at| Watched {
            id: "dQw4w9WgXcQ".into(),
            title: "t".into(),
            channel: channel.map(String::from),
            at,
        };
        let only = |paths: &Paths| {
            let h = load_history(paths).unwrap().watched;
            assert_eq!(h.len(), 1);
            (h[0].channel.clone(), h[0].at)
        };
        record_history(&paths, w(Some("Chan"), 1)).unwrap();
        // Watched again from a pasted link: it moves up, and keeps its channel.
        record_history(&paths, w(None, 2)).unwrap();
        assert_eq!(only(&paths), (Some("Chan".into()), 2));
        // A name that came later is taken.
        record_history(&paths, w(Some("Renamed"), 3)).unwrap();
        assert_eq!(only(&paths), (Some("Renamed".into()), 3));

        // A channel found afterwards fills an entry without one, and only that.
        let dir = TempDir::new();
        let paths = dir.paths();
        record_history(&paths, w(None, 1)).unwrap();
        name_in_history(&paths, "dQw4w9WgXcQ", "Found").unwrap();
        assert_eq!(only(&paths), (Some("Found".into()), 1));
        name_in_history(&paths, "dQw4w9WgXcQ", "Other").unwrap();
        assert_eq!(only(&paths), (Some("Found".into()), 1));
        // A video that isn't in the history isn't added to it.
        name_in_history(&paths, "aaaaaaaaaaa", "Other").unwrap();
        assert_eq!(only(&paths).0.as_deref(), Some("Found"));
        // Nor is a history made where there is none.
        let dir = TempDir::new();
        let paths = dir.paths();
        name_in_history(&paths, "dQw4w9WgXcQ", "Found").unwrap();
        assert!(!paths.history().exists());
    }
}
