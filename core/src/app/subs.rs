//! Subscriptions: kept in a file of their own, changed under the store lock.

use super::runtime::Runtime;
use crate::feed;
use crate::ids;
use crate::store::{self, Paths};
use crate::subs::{self, Sub, Subs};
use anyhow::{Context, Result, bail};

pub fn load_subs(paths: &Paths) -> Result<Subs> {
    store::load(&paths.subscriptions())
}

/// Returns the subscription and whether it is new. `current` says whether
/// `rt`'s route is still the one in use: a lookup that came back from a route
/// that has been left isn't added.
pub fn add_sub(
    rt: &Runtime,
    paths: &Paths,
    input: &str,
    current: &dyn Fn() -> bool,
) -> Result<(Sub, bool)> {
    let r = ids::parse_channel(input)
        .with_context(|| format!("{input:?} isn't a channel (@handle, channel URL or UC… ID)"))?;
    let (id, name) = match r {
        // The ID is all a subscription needs, and the channel's own feed
        // says what it is called. That needs no yt-dlp, which YouTube may
        // refuse (a bot check on this address) when RSS still answers, and
        // spares the one request that is a lookup of this channel in
        // particular. If the feed doesn't answer either, the ID will do as a
        // name until the channel is renamed.
        ids::ChannelRef::Id(id) => match feed::channel_title(rt, &id, current) {
            Ok(name) => (id, name),
            Err(_) if !current() => bail!(crate::inflight::ROUTE_CHANGED),
            Err(e) => {
                note!("qveil: couldn't read the name of {id} from its feed ({e:#}); using the ID");
                let name = id.clone();
                (id, name)
            }
        },
        // A handle has to be looked up, which only yt-dlp does.
        ids::ChannelRef::Url(url) => rt.yt.channel_info(&url)?,
    };
    // Only the file update is locked, not the lookup above.
    let _lock = store::lock(paths)?;
    if !current() {
        bail!(crate::inflight::ROUTE_CHANGED);
    }
    let mut subs = load_subs(paths)?;
    let sub = Sub { id, name };
    let added = subs.add(sub.clone());
    store::save(&paths.subscriptions(), &subs)?;
    Ok((sub, added))
}

pub fn remove_sub(paths: &Paths, query: &str) -> Result<Sub> {
    let _lock = store::lock(paths)?;
    let mut subs = load_subs(paths)?;
    let removed = subs
        .remove(query)
        .with_context(|| format!("no subscription matches {query:?}"))?;
    store::save(&paths.subscriptions(), &subs)?;
    Ok(removed)
}

/// Returns (newly added, found in the file). Makes no network requests.
pub fn import_subs(paths: &Paths, text: &str) -> Result<(usize, usize)> {
    let found = subs::parse_import(text)?;
    let _lock = store::lock(paths)?;
    let mut subs = load_subs(paths)?;
    let total = found.len();
    let added = found.into_iter().filter(|s| subs.add(s.clone())).count();
    store::save(&paths.subscriptions(), &subs)?;
    Ok((added, total))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::history::{Watched, load_history, record_history};
    use crate::app::testing::{channel, with_feeds_at};
    use crate::store::testing::{TempDir, listing_runs, listing_ytdlp};
    use std::fs;
    use std::sync::atomic::Ordering;

    #[test]
    fn corrupt_subscriptions_are_an_error_and_are_left_alone() {
        let dir = TempDir::new();
        let paths = dir.paths();
        fs::create_dir_all(&paths.data).unwrap();
        fs::write(paths.subscriptions(), "{\"channels\":[{\"id\":").unwrap();
        assert!(load_subs(&paths).is_err());
        assert!(import_subs(&paths, &channel(1)).is_err());
        assert!(remove_sub(&paths, "Chan1").is_err());
        assert_eq!(
            fs::read_to_string(paths.subscriptions()).unwrap(),
            "{\"channels\":[{\"id\":"
        );
    }

    #[test]
    fn simultaneous_changes_are_all_kept() {
        let dir = TempDir::new();
        let paths = dir.paths();
        let threads: Vec<_> = (0..8)
            .map(|n| {
                let paths = paths.clone();
                std::thread::spawn(move || {
                    for k in 0..5 {
                        import_subs(&paths, &channel(n * 10 + k)).unwrap();
                        record_history(
                            &paths,
                            Watched {
                                id: format!("v{n:02}{k:08}"),
                                title: "t".into(),
                                channel: None,
                                at: 0,
                            },
                        )
                        .unwrap();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(load_subs(&paths).unwrap().channels.len(), 40);
        assert_eq!(load_history(&paths).unwrap().watched.len(), 40);
    }

    const SOMEONE: &str = "UCsBjURrPoezykLs9EqgamOA";

    #[test]
    fn subscribing_by_id_takes_the_name_from_the_feed_and_needs_no_yt_dlp() {
        let dir = TempDir::new();
        let paths = dir.paths();
        let (base, hits) = crate::store::testing::rss_upstream(0);
        // A yt-dlp that would be counted if it were run: the bot check has
        // closed it to this address, in the case that matters.
        let rt = with_feeds_at(&paths, &base, &listing_ytdlp(&dir.0, ""));
        let (sub, added) = add_sub(&rt, &paths, SOMEONE, &|| true).unwrap();
        assert_eq!(
            (sub.name.as_str(), added),
            (&*format!("Name of {SOMEONE}"), true)
        );
        // A channel URL is the same thing.
        let other = format!("UC{:022}", 7);
        let (sub, _) = add_sub(
            &rt,
            &paths,
            &format!("https://www.youtube.com/channel/{other}/videos?si=x"),
            &|| true,
        )
        .unwrap();
        assert_eq!(sub.id, other);
        assert_eq!(hits.load(Ordering::SeqCst), 2, "one request each");
        assert_eq!(listing_runs(&dir.0), 0, "yt-dlp was not run");
        assert_eq!(load_subs(&paths).unwrap().channels.len(), 2);
        // Again: nothing new.
        assert!(!add_sub(&rt, &paths, SOMEONE, &|| true).unwrap().1);
    }

    #[test]
    fn subscribing_by_id_falls_back_to_the_id_when_the_feed_is_down() {
        let dir = TempDir::new();
        let paths = dir.paths();
        let (base, hits) = crate::store::testing::rss_upstream(usize::MAX);
        let rt = with_feeds_at(&paths, &base, &listing_ytdlp(&dir.0, ""));
        let (sub, added) = add_sub(&rt, &paths, SOMEONE, &|| true).unwrap();
        assert_eq!((sub.name.as_str(), added), (SOMEONE, true));
        assert_eq!(hits.load(Ordering::SeqCst), 2, "tried twice, then gave up");
        assert_eq!(listing_runs(&dir.0), 0, "and did not ask yt-dlp either");
        // A first attempt that fails and a second that works is not a fallback.
        let (base, hits) = crate::store::testing::rss_upstream(1);
        let rt = with_feeds_at(&paths, &base, &listing_ytdlp(&dir.0, ""));
        let other = format!("UC{:022}", 3);
        let (sub, _) = add_sub(&rt, &paths, &other, &|| true).unwrap();
        assert_eq!(sub.name, format!("Name of {other}"));
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_handle_still_needs_yt_dlp_and_the_feed_is_not_asked() {
        let dir = TempDir::new();
        let paths = dir.paths();
        let (base, hits) = crate::store::testing::rss_upstream(0);
        let named = crate::store::testing::script(
            &dir.0.join("yt-dlp"),
            &format!(
                "echo x >> '{}'\necho '{{\"channel_id\":\"{SOMEONE}\",\"channel\":\"Someone\",\"entries\":[]}}'\n",
                dir.0.join("runs").display()
            ),
        );
        let rt = with_feeds_at(&paths, &base, &named);
        let (sub, added) = add_sub(&rt, &paths, "@someone", &|| true).unwrap();
        assert_eq!(
            (sub.id.as_str(), sub.name.as_str(), added),
            (SOMEONE, "Someone", true)
        );
        assert_eq!((listing_runs(&dir.0), hits.load(Ordering::SeqCst)), (1, 0));
    }

    #[test]
    fn a_name_that_came_back_from_a_route_that_was_left_is_not_added() {
        let dir = TempDir::new();
        let paths = dir.paths();
        let (base, hits) = crate::store::testing::rss_upstream(0);
        let rt = with_feeds_at(&paths, &base, &listing_ytdlp(&dir.0, ""));
        let err = add_sub(&rt, &paths, SOMEONE, &|| false).unwrap_err();
        assert!(err.to_string().contains("route changed"), "{err}");
        // Not even asked: nothing goes out on the route that was left.
        assert_eq!(hits.load(Ordering::SeqCst), 0);
        assert!(load_subs(&paths).unwrap().channels.is_empty());
        // The route changing while the feed was being read is the same: the
        // first look says all is well, and the feed fails, and the next says no.
        let (base, _) = crate::store::testing::rss_upstream(usize::MAX);
        let rt = with_feeds_at(&paths, &base, &listing_ytdlp(&dir.0, ""));
        let looks = std::sync::atomic::AtomicUsize::new(0);
        let err = add_sub(&rt, &paths, SOMEONE, &|| {
            looks.fetch_add(1, Ordering::SeqCst) == 0
        })
        .unwrap_err();
        assert!(err.to_string().contains("route changed"), "{err}");
    }
}
