//! Which videos the engine last saw said to be live or announced, in the
//! lists it served (searches, a channel's tabs, the feed with its live
//! check), so that a play of one goes straight to the live lookup (see
//! `Server::open_until`). In memory only, and only as fresh as the list
//! it came from: the lookup has the last word, and a mark that turns out
//! wrong costs one lookup more, never a video that doesn't play.

use crate::util::lock;
use crate::video::{LiveState, Video};
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

/// The most marks kept: more than a few pages of results.
const KEPT: usize = 1000;

#[derive(Default)]
pub(super) struct Marks {
    kept: Mutex<Kept>,
}

#[derive(Default)]
struct Kept {
    state: HashMap<String, LiveState>,
    /// The order they were noted in, oldest first, to let the oldest go.
    order: VecDeque<String>,
}

impl Marks {
    /// Takes what a list says of each of its videos: a live or announced one
    /// is marked as such, any other is no longer marked.
    pub(super) fn note<'a>(&self, videos: impl IntoIterator<Item = &'a Video>) {
        let mut kept = lock(&self.kept);
        for v in videos {
            kept.set(&v.id, v.live);
        }
    }

    /// What a lookup found `id` to be.
    pub(super) fn found(&self, id: &str, state: Option<LiveState>) {
        lock(&self.kept).set(id, state);
    }

    pub(super) fn get(&self, id: &str) -> Option<LiveState> {
        lock(&self.kept).state.get(id).copied()
    }

    pub(super) fn clear(&self) {
        *lock(&self.kept) = Kept::default();
    }
}

impl Kept {
    fn set(&mut self, id: &str, state: Option<LiveState>) {
        match state {
            Some(state) => {
                if self.state.insert(id.to_string(), state).is_none() {
                    self.order.push_back(id.to_string());
                }
                while self.order.len() > KEPT {
                    if let Some(old) = self.order.pop_front() {
                        self.state.remove(&old);
                    }
                }
            }
            None => {
                if self.state.remove(id).is_some() {
                    self.order.retain(|o| o != id);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video(id: &str, live: Option<LiveState>) -> Video {
        Video {
            id: id.into(),
            title: id.into(),
            channel: None,
            channel_id: None,
            published: None,
            duration: None,
            views: None,
            short: false,
            live,
            starts: None,
            watching: None,
            streamed: false,
        }
    }

    #[test]
    fn a_list_marks_its_live_and_announced_videos_and_unmarks_the_rest() {
        let m = Marks::default();
        m.note(&[
            video("a", Some(LiveState::Now)),
            video("b", Some(LiveState::Upcoming)),
            video("c", None),
        ]);
        assert_eq!(m.get("a"), Some(LiveState::Now));
        assert_eq!(m.get("b"), Some(LiveState::Upcoming));
        assert_eq!(m.get("c"), None);
        // A later list, or a lookup, has the last word.
        m.note(&[video("a", None)]);
        m.found("b", Some(LiveState::Now));
        assert_eq!(m.get("a"), None);
        assert_eq!(m.get("b"), Some(LiveState::Now));
        m.clear();
        assert_eq!(m.get("b"), None);
    }

    #[test]
    fn only_the_newest_marks_are_kept() {
        let m = Marks::default();
        for i in 0..KEPT + 10 {
            m.found(&format!("v{i}"), Some(LiveState::Now));
        }
        assert_eq!(m.get("v0"), None);
        assert_eq!(m.get("v9"), None);
        assert_eq!(m.get("v10"), Some(LiveState::Now));
        assert_eq!(m.get(&format!("v{}", KEPT + 9)), Some(LiveState::Now));
        assert_eq!(lock(&m.kept).order.len(), KEPT);
    }
}
