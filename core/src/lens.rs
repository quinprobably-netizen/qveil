//! Lenses: feeds described in words.
//!
//! A lens is a sentence like "slow woodworking with hand tools, no music,
//! longer than 15 minutes". Fixed rules read it (`read`): there is no
//! language model here. What is left over is searched for on YouTube, one
//! search per topic, and everything else (the filtering, the ordering, the
//! reasons shown with each result) happens on this machine.

pub mod rank;
pub mod read;
pub mod sp;

use crate::store::{self, Paths};
use anyhow::{Context, Result, bail};
use rank::Ranked;
use serde::{Deserialize, Serialize};

/// How many lenses there can be.
pub const MAX: usize = 12;
/// How many results each topic's search is asked for.
pub const PER_TOPIC: usize = 30;
/// How old a lens's results may be before opening it fetches them again.
pub const STALE: i64 = 6 * 3600;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Lens {
    /// Eight hex digits: what its results' file is named after.
    pub id: String,
    /// What the user wrote, trimmed.
    pub text: String,
    pub created: i64,
}

#[derive(Default, Serialize, Deserialize)]
struct File {
    lenses: Vec<Lens>,
}

/// What a lens's searches last found, ranked.
#[derive(Default, Serialize, Deserialize)]
pub struct Cached {
    pub fetched_at: i64,
    pub videos: Vec<Ranked>,
}

/// How far in the future results can be dated and still count as found now:
/// a clock put right by a few seconds or minutes is not a new day.
pub const SLACK: i64 = 600;

/// Whether results `found` are to be fetched again at `now`, when they are
/// good for `max_age` seconds: nothing was found, or it is that old. Found
/// more than `SLACK` "later" than now means the clock was set back since, and
/// their age is not known: they are due.
pub fn due(found: Option<&Cached>, now: i64, max_age: i64) -> bool {
    found.is_none_or(|c| c.fetched_at > now + SLACK || now - c.fetched_at >= max_age)
}

/// The lenses, in the order they were added. They are the user's writing, so
/// a file that can't be read is an error, and is left as it is.
pub fn load(paths: &Paths) -> Result<Vec<Lens>> {
    Ok(store::load::<File>(&paths.lenses())?.lenses)
}

fn save(paths: &Paths, lenses: Vec<Lens>) -> Result<()> {
    store::save(&paths.lenses(), &File { lenses })
}

/// `text` as it is stored, if it can be a lens.
fn checked(text: &str, now: i64) -> Result<String> {
    let text = text.trim();
    if text.chars().count() > read::TEXT_MAX {
        bail!("A lens can be up to {} characters.", read::TEXT_MAX);
    }
    read::read(text, now).check()?;
    Ok(text.to_string())
}

pub fn add(paths: &Paths, text: &str, now: i64) -> Result<Lens> {
    let text = checked(text, now)?;
    let _lock = store::lock(paths)?;
    let mut lenses = load(paths)?;
    if lenses.len() >= MAX {
        bail!("{MAX} lenses is the most; remove one first.");
    }
    let id = loop {
        let id = format!("{:08x}", rand::random::<u32>());
        if lenses.iter().all(|l| l.id != id) {
            break id;
        }
    };
    let lens = Lens {
        id,
        text,
        created: now,
    };
    lenses.push(lens.clone());
    save(paths, lenses)?;
    Ok(lens)
}

/// Changes what a lens says. Its results were for the old words, so they go.
pub fn edit(paths: &Paths, id: &str, text: &str, now: i64) -> Result<Lens> {
    let text = checked(text, now)?;
    let _lock = store::lock(paths)?;
    let mut lenses = load(paths)?;
    let lens = lenses.iter_mut().find(|l| l.id == id).context(GONE)?;
    lens.text = text;
    let lens = lens.clone();
    save(paths, lenses)?;
    forget_cache(paths, id)?;
    Ok(lens)
}

pub fn remove(paths: &Paths, id: &str) -> Result<()> {
    let _lock = store::lock(paths)?;
    let mut lenses = load(paths)?;
    let before = lenses.len();
    lenses.retain(|l| l.id != id);
    if lenses.len() == before {
        bail!(GONE);
    }
    save(paths, lenses)?;
    forget_cache(paths, id)
}

const GONE: &str = "That lens no longer exists.";

pub fn get(paths: &Paths, id: &str) -> Result<Lens> {
    load(paths)?.into_iter().find(|l| l.id == id).context(GONE)
}

/// The lens the command line means: its number in `qveil lens ls`, or its ID.
pub fn find(paths: &Paths, which: &str) -> Result<Lens> {
    let lenses = load(paths)?;
    let by_number = which
        .parse::<usize>()
        .ok()
        .and_then(|n| n.checked_sub(1))
        .and_then(|i| lenses.get(i));
    by_number
        .or_else(|| lenses.iter().find(|l| l.id == which))
        .cloned()
        .with_context(|| format!("no lens {which:?}; `qveil lens ls` lists them"))
}

/// What the lens's searches last found, if they have run. The file's name
/// comes from an ID in the stored list, never from a request.
pub fn cached(paths: &Paths, id: &str) -> Result<Option<Cached>> {
    let path = paths.lens_cache(id);
    if !path.exists() {
        return Ok(None);
    }
    store::load_cache(&path).map(Some)
}

/// Keeps what `lens`'s searches found, if it is still the lens they ran
/// for. They take a while, and meanwhile its words may have been changed or
/// the lens removed: results for other words would be shown as its own for
/// hours, and results for no lens would stay behind in the cache. Returns
/// whether they were kept.
pub fn save_results(paths: &Paths, lens: &Lens, results: &Cached) -> Result<bool> {
    let _lock = store::lock(paths)?;
    let unchanged = load(paths)?
        .iter()
        .any(|l| l.id == lens.id && l.text == lens.text);
    if unchanged {
        store::save_compact(&paths.lens_cache(&lens.id), results)?;
    }
    Ok(unchanged)
}

fn forget_cache(paths: &Paths, id: &str) -> Result<()> {
    match std::fs::remove_file(paths.lens_cache(id)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testing::TempDir;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn lenses_are_added_edited_removed_and_private() {
        let d = TempDir::new();
        let p = d.paths();
        assert!(load(&p).unwrap().is_empty());
        let lens = add(&p, "  bread, sourdough  ", 5).unwrap();
        assert_eq!((lens.text.as_str(), lens.created), ("bread, sourdough", 5));
        assert!(lens.id.len() == 8 && lens.id.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(load(&p).unwrap(), std::slice::from_ref(&lens));
        assert_eq!(
            std::fs::metadata(p.lenses()).unwrap().permissions().mode() & 0o777,
            0o600
        );
        // Its results go when its words change, and when it does.
        let results = Cached {
            fetched_at: 9,
            videos: vec![],
        };
        assert!(save_results(&p, &lens, &results).unwrap());
        assert_eq!(cached(&p, &lens.id).unwrap().unwrap().fetched_at, 9);
        let edited = edit(&p, &lens.id, "bread", 6).unwrap();
        assert_eq!(
            (edited.id.as_str(), edited.text.as_str()),
            (lens.id.as_str(), "bread")
        );
        assert!(cached(&p, &lens.id).unwrap().is_none());
        assert!(save_results(&p, &edited, &results).unwrap());
        remove(&p, &lens.id).unwrap();
        assert!(load(&p).unwrap().is_empty());
        assert!(!p.lens_cache(&lens.id).exists());
        assert!(edit(&p, &lens.id, "bread", 7).is_err());
        assert!(get(&p, &lens.id).is_err());
    }

    #[test]
    fn a_lens_needs_a_topic_a_sane_length_and_there_are_at_most_twelve() {
        let d = TempDir::new();
        let p = d.paths();
        let e = add(&p, "no music, over 10 min", 0).unwrap_err().to_string();
        assert!(e.starts_with("Say what it should be about"), "{e}");
        let e = add(&p, &"x".repeat(501), 0).unwrap_err().to_string();
        assert_eq!(e, "A lens can be up to 500 characters.");
        assert!(!p.lenses().exists(), "nothing was saved");
        for i in 0..MAX {
            add(&p, &format!("topic {i}"), 0).unwrap();
        }
        let e = add(&p, "one more", 0).unwrap_err().to_string();
        assert_eq!(e, "12 lenses is the most; remove one first.");
        // An edit is held to the same rules.
        let first = load(&p).unwrap()[0].clone();
        assert!(edit(&p, &first.id, "no music", 0).is_err());
        assert_eq!(load(&p).unwrap()[0], first);
    }

    #[test]
    fn a_damaged_lenses_file_is_an_error_and_is_left_alone() {
        let d = TempDir::new();
        let p = d.paths();
        std::fs::create_dir_all(&p.data).unwrap();
        std::fs::write(p.lenses(), b"{ nope").unwrap();
        let e = format!("{:#}", load(&p).unwrap_err());
        assert!(e.contains("lenses.json"), "{e}");
        assert!(add(&p, "bread", 0).is_err());
        assert_eq!(std::fs::read(p.lenses()).unwrap(), b"{ nope");
    }

    #[test]
    fn results_are_only_saved_for_the_words_they_were_fetched_for() {
        let d = TempDir::new();
        let p = d.paths();
        let lens = add(&p, "bread", 0).unwrap();
        let results = Cached {
            fetched_at: 9,
            videos: vec![],
        };
        assert!(save_results(&p, &lens, &results).unwrap());
        assert!(p.lens_cache(&lens.id).exists());
        // Its words changed while its searches ran: what they found is for
        // the old words, and is not kept as the new ones' results.
        let edited = edit(&p, &lens.id, "jazz", 1).unwrap();
        assert!(!save_results(&p, &lens, &results).unwrap());
        assert!(!p.lens_cache(&lens.id).exists());
        // It was removed meanwhile: nothing is left behind for it.
        remove(&p, &lens.id).unwrap();
        assert!(!save_results(&p, &edited, &results).unwrap());
        assert!(!p.lens_cache(&lens.id).exists());
    }

    #[test]
    fn a_lens_is_found_by_its_number_or_its_id() {
        let d = TempDir::new();
        let p = d.paths();
        let a = add(&p, "bread", 0).unwrap();
        let b = add(&p, "jazz", 0).unwrap();
        assert_eq!(find(&p, "2").unwrap(), b);
        assert_eq!(find(&p, &a.id).unwrap(), a);
        assert!(find(&p, "3").is_err() && find(&p, "0").is_err() && find(&p, "x").is_err());
    }
}
