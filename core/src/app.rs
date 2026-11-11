//! What qveil can do, shared by the command line and the local server that the
//! desktop app talks to: what a command runs on (`runtime`), the feed of the
//! channels you follow (`feed`), searches and lenses (`lens`), topic feeds
//! (`topics`), the subscriptions (`subs`) and the history (`history`).

mod feed;
mod history;
mod lens;
mod runtime;
mod subs;
#[cfg(test)]
mod testing;
mod topics;

pub use feed::{Refresh, Refreshed, channel, feed, feed_result, refresh};
#[cfg(test)]
pub use history::History;
pub use history::{Watched, clear_history, load_history, name_in_history, record_history};
pub use lens::{LensRefresh, LensResult, lens, search, search_each, smart_search};
pub use runtime::Runtime;
#[cfg(test)]
pub use runtime::SEARCH_PAUSE;
pub use subs::{add_sub, import_subs, load_subs, remove_sub};
#[cfg(test)]
pub(crate) use topics::TOPICS_SPENT;
pub use topics::{forget_topics, hide_topic, tidy_topics, topics};

pub fn now() -> i64 {
    jiff::Timestamp::now().as_second()
}
