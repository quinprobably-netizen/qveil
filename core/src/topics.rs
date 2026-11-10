//! Topic feeds: what the channels you follow are about, worked out here from
//! the titles they have uploaded lately, and grouped, so that a search can
//! find more of the same from channels you don't follow.
//!
//! Fixed rules, no model. A channel's terms are the words and word pairs
//! that keep coming back in its titles and that few of the other channels
//! use. Channels whose terms overlap are one group; a channel unlike the
//! rest is a group of its own; a channel with nothing that comes back has
//! none. Nothing in this file talks to anybody: see `app::topics` for the
//! searches, which only happen while the setting is on.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// How many topic feeds there can be.
pub const MAX_GROUPS: usize = 4;
/// How many searches a group has.
pub const SEARCHES: usize = 3;
/// How many results each search asks for.
pub const PER_SEARCH: usize = 20;
/// A group's results are fetched again once they are this old (a day).
pub const STALE: i64 = 24 * 3600;
/// How many of a channel's latest titles are read.
pub const TITLES: usize = 30;
/// How alike two channels' terms must be for them to share a group.
const ALIKE: f64 = 0.2;

/// The groups the user doesn't want to see, by id.
#[derive(Default, Serialize, Deserialize)]
pub struct Hidden {
    pub ids: Vec<String>,
}

/// What each group's searches last found, by the group's id.
#[derive(Default, Serialize, Deserialize)]
pub struct Results {
    pub groups: HashMap<String, crate::lens::Cached>,
}

/// A channel you follow and the titles it uploaded lately.
pub struct Channel<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub titles: Vec<&'a str>,
}

/// Channels that are about the same thing, and what that is.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Group {
    /// Stays the same while the same channels are in the group.
    pub id: String,
    /// The terms the group is known by, most telling first.
    pub label: String,
    /// The names of its channels.
    pub channels: Vec<String>,
    /// What a refresh asks YouTube for, word for word.
    pub searches: Vec<String>,
}

/// Words that say nothing about a topic.
const STOP: &[&str] = &[
    "the", "and", "for", "with", "from", "into", "onto", "over", "under", "about", "after",
    "before", "this", "that", "these", "those", "you", "your", "our", "his", "her", "its", "their",
    "they", "them", "she", "him", "who", "what", "why", "how", "when", "where", "which", "was",
    "were", "are", "been", "being", "have", "has", "had", "does", "did", "will", "would", "can",
    "could", "should", "not", "but", "all", "any", "one", "two", "out", "off", "too", "very",
    "just", "more", "most", "than", "then", "now", "new", "best", "ever", "never", "every", "here",
    "there", "get", "got", "make", "made", "making", "use", "using", "try", "tried", "video",
    "videos", "part", "episode", "live", "stream", "official", "full", "vs", "really", "actually",
    "almost", "still", "also", "some", "much", "many", "isn", "don", "didn", "doesn", "won",
    "can't", "let", "lets", "let's", "it's", "i'm", "i've",
];

fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(String::from)
        .collect()
}

/// Whether a word can be part of a topic.
fn telling(word: &str, name: &BTreeSet<String>) -> bool {
    word.chars().count() >= 3
        && !word.chars().all(|c| c.is_ascii_digit())
        && !STOP.contains(&word)
        && !name.contains(word)
}

/// A channel's terms and how telling each is, before the other channels are
/// taken into account: the share of its titles a term is in, for terms that
/// are in at least two.
fn recurring(channel: &Channel) -> BTreeMap<String, f64> {
    let name: BTreeSet<String> = words(channel.name).into_iter().collect();
    let titles: Vec<&&str> = channel.titles.iter().take(TITLES).collect();
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    for title in &titles {
        let w = words(title);
        let mut terms: BTreeSet<String> = BTreeSet::new();
        for (i, word) in w.iter().enumerate() {
            if !telling(word, &name) {
                continue;
            }
            terms.insert(word.clone());
            // A pair is two telling words that stand together in the title.
            if let Some(next) = w.get(i + 1).filter(|n| telling(n, &name)) {
                terms.insert(format!("{word} {next}"));
            }
        }
        for term in terms {
            *seen.entry(term).or_default() += 1;
        }
    }
    seen.into_iter()
        .filter(|(_, n)| *n >= 2)
        .map(|(term, n)| (term, n as f64 / titles.len().max(1) as f64))
        .collect()
}

fn alike(a: &BTreeMap<String, f64>, b: &BTreeMap<String, f64>) -> f64 {
    // One word in common is a habit of speech ("review"), not a subject.
    let words_in_common = a
        .keys()
        .filter(|term| !term.contains(' ') && b.contains_key(*term))
        .count();
    if words_in_common < 2 {
        return 0.0;
    }
    let dot: f64 = a
        .iter()
        .filter_map(|(term, x)| b.get(term).map(|y| x * y))
        .sum();
    let len = |m: &BTreeMap<String, f64>| m.values().map(|x| x * x).sum::<f64>().sqrt();
    let (la, lb) = (len(a), len(b));
    if la == 0.0 || lb == 0.0 {
        0.0
    } else {
        dot / (la * lb)
    }
}

/// FNV-1a, so that an id is the same on every machine and in every version.
fn stable_id(parts: &[&str]) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in parts.join("\n").bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{:012x}", hash & 0xffff_ffff_ffff)
}

/// The groups the channels fall into, largest first. A channel is in at most
/// one, and in none when nothing in its titles comes back.
pub fn groups(channels: &[Channel]) -> Vec<Group> {
    let mut with_terms: Vec<(&Channel, BTreeMap<String, f64>)> = channels
        .iter()
        .map(|c| (c, recurring(c)))
        .filter(|(_, terms)| !terms.is_empty())
        .collect();
    with_terms.sort_by(|a, b| a.0.id.cmp(b.0.id));
    // A term that most of the channels use tells little about any of them.
    let total = with_terms.len() as f64;
    let mut used: HashMap<String, f64> = HashMap::new();
    for (_, terms) in &with_terms {
        for term in terms.keys() {
            *used.entry(term.clone()).or_default() += 1.0;
        }
    }
    for (_, terms) in &mut with_terms {
        for (term, weight) in terms.iter_mut() {
            *weight *= (1.0 + total / used[term]).ln();
        }
    }
    // Channels that are alike are joined, and what is joined to one of a
    // group is in the group.
    let mut group_of: Vec<usize> = (0..with_terms.len()).collect();
    for i in 0..with_terms.len() {
        for j in i + 1..with_terms.len() {
            if alike(&with_terms[i].1, &with_terms[j].1) >= ALIKE {
                let (from, to) = (group_of[j], group_of[i]);
                for g in &mut group_of {
                    if *g == from {
                        *g = to;
                    }
                }
            }
        }
    }
    let mut found: Vec<(usize, f64, Group)> = Vec::new();
    let roots: BTreeSet<usize> = group_of.iter().copied().collect();
    for root in roots {
        let members: Vec<&(&Channel, BTreeMap<String, f64>)> = with_terms
            .iter()
            .zip(&group_of)
            .filter(|(_, g)| **g == root)
            .map(|(m, _)| m)
            .collect();
        let mut terms: BTreeMap<&str, f64> = BTreeMap::new();
        for (_, own) in &members {
            for (term, weight) in own {
                *terms.entry(term).or_default() += weight;
            }
        }
        let mut ranked: Vec<(&str, f64)> = terms.into_iter().collect();
        // Most telling first; a pair before a word of the same weight, then by spelling.
        ranked.sort_by(|a, b| {
            b.1.total_cmp(&a.1)
                .then(b.0.contains(' ').cmp(&a.0.contains(' ')))
                .then(a.0.cmp(b.0))
        });
        // What is searched: pairs first (they say more), then single words
        // that no chosen pair already has.
        let mut searches: Vec<String> = Vec::new();
        for (term, _) in ranked.iter().filter(|(t, _)| t.contains(' ')) {
            if searches.len() < SEARCHES - 1 {
                searches.push((*term).to_string());
            }
        }
        for (term, _) in ranked.iter().filter(|(t, _)| !t.contains(' ')) {
            if searches.len() < SEARCHES
                && !searches.iter().any(|s| s.split(' ').any(|w| w == *term))
            {
                searches.push((*term).to_string());
            }
        }
        let ids: Vec<&str> = members.iter().map(|(c, _)| c.id).collect();
        let weight: f64 = ranked.iter().take(SEARCHES).map(|(_, w)| w).sum();
        found.push((
            members.len(),
            weight,
            Group {
                id: stable_id(&ids),
                label: searches.join(", "),
                channels: members.iter().map(|(c, _)| c.name.to_string()).collect(),
                searches,
            },
        ));
    }
    found.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(b.1.total_cmp(&a.1))
            .then(a.2.id.cmp(&b.2.id))
    });
    found.into_iter().map(|(_, _, group)| group).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wood_a() -> Channel<'static> {
        Channel {
            id: "UCwood000000000000000000a",
            name: "Northlight Woodworks",
            titles: vec![
                "A dovetail box with four hand tools",
                "Sharpening a hand plane properly",
                "The cheapest workbench that works",
                "Dovetail joints without a jig",
                "Restoring an old hand plane",
                "A workbench for a small shop",
            ],
        }
    }
    fn wood_b() -> Channel<'static> {
        Channel {
            id: "UCwood000000000000000000b",
            name: "Quiet Bench",
            titles: vec![
                "Hand plane basics",
                "My first dovetail",
                "Tuning a hand plane",
                "Dovetail saw comparison",
                "Flattening a board by hand",
            ],
        }
    }
    fn systems() -> Channel<'static> {
        Channel {
            id: "UCsys0000000000000000000a",
            name: "Low Level Hours",
            titles: vec![
                "Why the borrow checker says no",
                "Allocators, from scratch",
                "What a linker actually does",
                "A bump allocator in 50 lines",
                "The borrow checker and lifetimes",
                "Linker scripts, explained",
            ],
        }
    }
    fn variety() -> Channel<'static> {
        Channel {
            id: "UCvar0000000000000000000a",
            name: "Everything Daily",
            titles: vec![
                "I bought a boat",
                "Trying the spiciest noodles",
                "We built a treehouse",
                "My dog learned chess",
            ],
        }
    }

    #[test]
    fn channels_about_the_same_thing_share_a_group_and_an_outlier_gets_its_own() {
        let found = groups(&[systems(), wood_a(), wood_b()]);
        assert_eq!(found.len(), 2, "{found:?}");
        // The larger group first.
        assert_eq!(found[0].channels, ["Northlight Woodworks", "Quiet Bench"]);
        assert_eq!(found[1].channels, ["Low Level Hours"]);
    }

    #[test]
    fn a_channel_with_nothing_that_comes_back_has_no_group() {
        assert_eq!(groups(&[variety()]), []);
        let found = groups(&[variety(), systems()]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].channels, ["Low Level Hours"]);
        assert_eq!(groups(&[]), []);
    }

    #[test]
    fn what_is_searched_is_what_keeps_coming_back_in_the_titles() {
        let found = groups(&[wood_a(), wood_b(), systems()]);
        let wood = &found[0];
        // Pairs first, then single words that the pairs don't already have.
        assert_eq!(wood.searches[0], "hand plane");
        assert!(wood.searches.contains(&"dovetail".to_string()), "{wood:?}");
        assert!(wood.searches.len() <= SEARCHES);
        assert_eq!(wood.label, wood.searches.join(", "));
        let sys = &found[1];
        assert!(
            sys.searches.contains(&"borrow checker".to_string()),
            "{sys:?}"
        );
        // Nothing of a channel's own name, no filler, and no word alone that a pair has.
        for group in &found {
            for search in &group.searches {
                for word in search.split(' ') {
                    assert!(!STOP.contains(&word), "{search}");
                    assert!(
                        ![
                            "northlight",
                            "woodworks",
                            "quiet",
                            "bench",
                            "low",
                            "level",
                            "hours"
                        ]
                        .contains(&word),
                        "{search}"
                    );
                }
            }
        }
        assert!(!wood.searches.contains(&"plane".to_string()), "{wood:?}");
    }

    #[test]
    fn a_groups_id_stays_while_its_channels_do() {
        let both = groups(&[wood_a(), wood_b()]);
        let again = groups(&[wood_b(), wood_a(), systems()]);
        assert_eq!(both[0].id, again[0].id);
        assert_eq!(both[0].id.len(), 12);
        // Another membership is another group.
        let alone = groups(&[wood_a()]);
        assert_ne!(alone[0].id, both[0].id);
    }

    #[test]
    fn a_word_most_channels_use_does_not_join_them() {
        // Both say "review" all the time, and nothing else in common.
        let phones = Channel {
            id: "UCa",
            name: "Phones",
            titles: vec![
                "Pixel camera review",
                "Pixel battery review",
                "Galaxy fold review",
            ],
        };
        let bikes = Channel {
            id: "UCb",
            name: "Bikes",
            titles: vec!["Gravel tyre review", "Gravel frame review", "Saddle review"],
        };
        let found = groups(&[phones, bikes]);
        assert_eq!(found.len(), 2, "{found:?}");
    }
}
