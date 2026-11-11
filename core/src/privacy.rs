//! What qveil says about privacy, in one place.
//!
//! The window shows the sentences it gets from the engine (`GET api/config`),
//! `qveil config` prints them, and the README's privacy section is printed by
//! `qveil-core privacy --markdown` and checked against the file by a test. So
//! no claim is worded twice, and a claim that stops being true is fixed here
//! and nowhere else.
//!
//! Every sentence has to be literally true of the state it describes. The
//! words that have been wrong before, and why they are chosen as they are:
//!
//! - Cookies are "never stored", not "never sent": yt-dlp sends a consent
//!   cookie, and sends back what YouTube sets during one call.
//! - Isolation is "fresh SOCKS credentials per request (a separate circuit if
//!   this is Tor)". It is never said that YouTube *can't* link requests: the
//!   fingerprint and the timing can.
//! - A proxy URL with credentials of its own is "not isolated", and so is an
//!   HTTP proxy.
//! - A stream that is looked up again after a 403, or after its circuit
//!   stalls, links two circuits (and only two: the old URLs are never tried
//!   on a new identity, see `Server::recover`).
//! - With resume on, qveil does remember unfinished videos, so history being
//!   off is "keeps no list of what you watch", not "doesn't remember".

use crate::config::Config;
use crate::net::{Net, TOR_PROXY};
use serde::Serialize;

/// What the route does, as far as what is said about it differs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Direct,
    /// The preset: the SOCKS port on 127.0.0.1:9050.
    Tor,
    /// Any other SOCKS proxy, which gets fresh credentials per request.
    Socks,
    /// A SOCKS proxy whose URL has credentials of its own.
    SocksShared,
    /// An HTTP or HTTPS proxy.
    Http,
}

impl Kind {
    pub fn of(net: &Net) -> Kind {
        match net.proxy() {
            None => Kind::Direct,
            Some(TOR_PROXY) if net.isolates() => Kind::Tor,
            Some(_) if net.isolates() => Kind::Socks,
            Some(_) if net.is_socks() => Kind::SocksShared,
            Some(_) => Kind::Http,
        }
    }

    /// One sentence for the route chip's tooltip.
    fn summary(self) -> &'static str {
        match self {
            Kind::Direct => "Direct: YouTube sees your IP address.",
            Kind::Tor => {
                "Through Tor: YouTube sees an exit node's address, not your IP, but can still link requests by their fingerprint and timing."
            }
            Kind::Socks => {
                "Through your SOCKS proxy: YouTube sees its address, not your IP, but can still link requests by their fingerprint and timing."
            }
            Kind::SocksShared => {
                "Through your SOCKS proxy: YouTube sees its address, not your IP, and the credentials in the proxy URL make every request one identity (not isolated)."
            }
            Kind::Http => {
                "Through your HTTP proxy: YouTube sees its address, not your IP, and requests are not isolated."
            }
        }
    }

    /// What the route does to the address and to the requests, which is the
    /// part of the story that depends on it.
    fn route_points(self) -> Vec<&'static str> {
        let relink = "A video that outlives its circuit, or whose circuit stalls (a minute without a byte), is looked up again on a fresh one, which links the two circuits: it is the same video, resumed at the same byte.";
        let isolated = "Requests go out with fresh SOCKS credentials per request (a separate circuit if this is Tor), except that a video's lookup and its bytes share one set, as do the thumbnails of one page, and a video played again within nine minutes is played from the same lookup on the same set.";
        match self {
            Kind::Direct => {
                vec!["YouTube sees your IP address with every request, which links them all."]
            }
            Kind::Tor => vec![
                "Requests go through the SOCKS proxy on 127.0.0.1:9050 (Tor), so YouTube sees an exit node's address instead of yours.",
                isolated,
                relink,
            ],
            Kind::Socks => vec![
                "Requests go through your SOCKS proxy, so YouTube sees its address instead of yours; if it is Tor, that is an exit node's.",
                isolated,
                relink,
            ],
            Kind::SocksShared => vec![
                "Requests go through your SOCKS proxy, so YouTube sees its address instead of yours.",
                "The proxy URL has credentials of its own, which every request sends, so qveil can't isolate requests: they all share one identity (not isolated).",
            ],
            Kind::Http => vec![
                "Requests go through your HTTP proxy, so YouTube sees its address instead of yours.",
                "qveil can't isolate requests on an HTTP proxy: they all leave from its address (not isolated).",
            ],
        }
    }
}

/// What holds on every route, and is short enough for the window.
const EVERYWHERE: &[&str] = &[
    "qveil's requests carry a browser User-Agent on a TLS handshake that no browser makes, which is probably rare, so a careful observer can link them by that fingerprint, even across circuits.",
    "Refreshing your subscriptions asks for each channel on its own, in random order, skipping channels fetched in the last 15 minutes: that is still a burst of requests for exactly your channels, so timing and the set of IDs can link them.",
    "You are never signed in, and cookies are never stored or read from a browser: yt-dlp sends the one consent cookie it sets for itself, and sends back what YouTube sets during a single call, kept in memory until that call ends.",
    "Comments are fetched only when you open them, as one more lookup of that video; commenters' pictures are never fetched.",
    "Playing a video asks YouTube for its streams and not for its watch page (unless they can't be played without it, as with a video made for kids: then they are asked for again, page and all, with the same identity). What only the page has is fetched as one more lookup of that video, and only when it is wanted: chapters that its description doesn't name when you open the Chapters tab, and the channel's name and the video's age when it was played without them (from a pasted link, say).",
    "A lens is refreshed as one search per topic (a second one for a topic whose filtered search found nothing): on one address their timing links them, and even over Tor a set of related searches can be linked by content.",
    "Subscriptions, lenses, history, resume positions and the caches stay on this computer, in files only you can read, and qveil has no telemetry.",
];

/// What the README says besides the above: the details behind them.
const DETAILS: &[(&str, &str)] = &[
    (
        "One way out",
        "The window and its video player only ever talk to qveil's engine on `127.0.0.1`, behind a random token. Every request that leaves your machine (search, feeds, thumbnails and the video bytes themselves) is made by the engine, or by the yt-dlp it runs, through the route you pick.",
    ),
    (
        "Route",
        "*Direct*, *Tor*, or your own *Proxy*, always shown at the foot of the window, in a colour of its own once YouTube can't see your IP. *Direct* means direct: qveil ignores `HTTP_PROXY`, `HTTPS_PROXY` and `ALL_PROXY` from the environment. qveil can't tell Tor from another SOCKS proxy by its address (it may be `localhost`, Tor Browser's port 9150 or a tunnel), so every SOCKS proxy gets fresh credentials per request, and `isolate_circuits = false` is refused. Seeking still works over any route, because the engine relays Range requests. `socks5://` is refused (it leaks DNS); use `socks5h://`. Thumbnails are fetched a page at a time: all the thumbnails of one page of the window share one identity, and the next page gets another.",
    ),
    (
        "Switching routes",
        "Switching routes ends the streams playing on the old route, kills the searches, lookups and refreshes still running on it (they answer \"The route changed; try again\"), drops thumbnails and feeds that were still on their way, and forgets the lookups kept for playing a video again (they are kept in memory, until their stream URLs are about to expire, and over a SOCKS proxy for nine minutes at most; clearing the history forgets them too).",
    ),
    (
        "Account and cookies",
        "yt-dlp runs with `--ignore-config --no-cookies --no-cookies-from-browser --no-plugin-dirs` and its own cache, so neither your config nor a plugin can add cookies or a route of its own. The engine's own requests (feeds, thumbnails and video bytes) send no cookies, and the cookies in yt-dlp's resolved headers are not passed on to the relay. Neither the embedded player nor `qveil play` loads your mpv config or scripts.",
    ),
    (
        "Command lines",
        "Anything on a command line can be read by every local user through `/proc`. So the app's own calls give yt-dlp its proxy (and password), your searches and the video you open through a private file (mode `0600`, in a fresh `0700` directory that is removed afterwards, also when the engine is stopped: the window closing, Ctrl+C or `kill` end any yt-dlp first, and a run that was killed hard has its directory swept up by the next one), and `qveil play` hands mpv its loopback URLs the same way. **qveil's own command line is not covered:** `qveil search …`, `qveil play <url>` and `qveil sub add …` carry what you type in their arguments, which other local users can read through `/proc` while the command runs, unlike a search typed into the window.",
    ),
    (
        "Process tree",
        "yt-dlp and mpv are started in process groups of their own, and are killed together with whatever they started (a deno for yt-dlp's JavaScript challenge, say) when the engine stops, and also when it is killed outright: a small guard process, which is qveil itself, outlives it for a moment and does that.",
    ),
    (
        "Tracking parameters",
        "Links are reduced to the video ID; `si=`, `pp=`, `feature=` and similar are dropped. A start time (`t=`) is kept, for the player only.",
    ),
    (
        "Player state",
        "mpv keeps no resume files or history. qveil itself remembers where you stopped in an unfinished video for a set number of days (7 unless you change it; 0 turns it off) in a file only you can read; a finished video is forgotten at once. A position that has expired is no longer offered, and is deleted the next time qveil starts or saves a position.",
    ),
    (
        "Local data",
        "Subscriptions, lenses, resume positions, the history, the topic feeds you hid and when recent topic searches were made live in `~/.local/share/qveil`, the feed cache, what each lens and topic feed found and (while keeping them is on) the thumbnails you were shown in `~/.cache/qveil`, settings in `~/.config/qveil`. The directories are mode `0700` and the files `0600`, and qveil tightens them at startup, because the window creates them first with looser defaults (leftover `veil` directories too). Watch history is **off** by default. `qveil wipe` deletes the data and caches, and anything left over from veil's directories (`qveil config` lists those).",
    ),
];

/// What YouTube can still see, on any route.
const STILL_SEES: &str = "**What YouTube can still see:** the IP you connect from (on Direct, yours; otherwise the route's), each search and video you play, and the timing of your requests. Even over Tor, a long session of related videos can be linked by content, and the fingerprint above can link requests that come from separate circuits. qveil reduces linkability; it doesn't eliminate it.";

/// What qveil says about the route in use. This is the `privacy` object of
/// `GET api/config`.
#[derive(Debug, Serialize)]
pub struct Privacy {
    /// One sentence, for the route chip's tooltip.
    pub summary: String,
    /// Sentences for the route popup, in order.
    pub points: Vec<String>,
    /// One line under each route option.
    pub routes: Routes,
    /// Lines that go with the pages.
    pub pages: Pages,
}

#[derive(Debug, Serialize)]
pub struct Routes {
    pub direct: &'static str,
    pub tor: &'static str,
    pub proxy: &'static str,
}

#[derive(Debug, Serialize)]
pub struct Pages {
    pub search: &'static str,
    pub subscriptions: &'static str,
    pub history_off: &'static str,
    pub history_on: &'static str,
    /// For the number of days that is set, so it is not a fixed text.
    pub resume: String,
    pub comments: &'static str,
    /// One sentence about comments, true on its own, for beside the order
    /// they are in, where `comments` is too long to show.
    pub comments_short: &'static str,
    pub lenses: &'static str,
    /// One sentence about lenses, true on its own, for where `lenses` is
    /// too long to show.
    pub lenses_short: &'static str,
    /// For whether topic feeds are on.
    pub topics: &'static str,
    /// Both, so that what turning them on does can be said before it is done.
    pub topics_on: &'static str,
    pub topics_off: &'static str,
    /// Getting the next video ready, on and off.
    pub prepare_on: &'static str,
    pub prepare_off: &'static str,
    /// Keeping thumbnails on disk, on and off.
    pub keep_thumbs_on: &'static str,
    pub keep_thumbs_off: &'static str,
}

const ROUTES: Routes = Routes {
    direct: "YouTube sees your IP address.",
    tor: "Fresh SOCKS credentials per request, which Tor puts on separate circuits (a video, and a page of thumbnails, each share one); requests can still be linked by their fingerprint and timing.",
    proxy: "A proxy you trust, such as a VPN's: YouTube sees its address, not yours. A SOCKS proxy gets fresh credentials per request (a separate circuit if it is Tor); a proxy URL with credentials of its own is not isolated.",
};

const SEARCH: &str = "Straight from YouTube's search, without an account; qveil stores no cookies and doesn't keep your searches. A search with a filter in it (\"over 30 min\", \"-word\") is sent as one search per topic, like a lens, and filtered and ordered on this computer.";
const SUBSCRIPTIONS: &str = "Kept only on this computer. Each channel is fetched on its own, in random order, but a refresh is still a burst of requests for exactly your channels.";
const HISTORY_OFF: &str = "Off: qveil keeps no history. Where you stopped in unfinished videos is remembered separately, while resume is on.";
const HISTORY_ON: &str =
    "On: kept only on this computer, in a file only you can read, until you clear it.";
const COMMENTS: &str = "Fetched only when you open them, as one more lookup of that video. Commenters' pictures are never fetched.";
const COMMENTS_SHORT: &str = "Loaded on demand, one extra request.";
const TOPICS_OFF: &str = "Off: qveil doesn't work out what the channels you follow are about, and searches for nothing by itself.";
const TOPICS_ON: &str = "On: qveil groups the channels you follow by the words that keep coming back in their titles, on this computer, and searches YouTube by itself for up to three of those phrases for each of up to four groups: at most twelve searches in a day, each counted before it is sent, and none for a group whose results are less than a day old, both by this computer's clock (changing the clock, or deleting qveil's data, can start the day over). YouTube sees those searches. They say what the channels you follow are about, and can point to a channel itself when what comes back in its titles is a series or a name. Videos from channels you follow are left out of what is shown. Turned off, what was found is deleted, but when the day's searches were made is kept until each is a day old, so that turning it off and on again doesn't start the count over; each is deleted by the next time qveil starts after that.";
const TOPICS_POINT: &str = "Topic feeds are on: without being asked each time, qveil searches YouTube for phrases taken from the titles of the channels you follow, which tells YouTube what those channels are about.";
const PREPARE_OFF: &str = "Off: a video is looked up when you play it, and not before.";
const PREPARE_ON: &str = "On: in the last 30 seconds of a video you play from a list, qveil looks up the next one in Up next, so that it starts at once when you go on to it. YouTube (over Tor, the exit node too) sees that lookup even if you never watch the video. Over Tor it goes on a circuit of its own, which the video then plays on if you start it within nine minutes. Nothing is looked up because you pointed at it.";
const PREPARE_POINT: &str = "Getting the next video ready is on: near the end of a video, the next one in its list is looked up before you play it, so YouTube sees a lookup of a video you may not watch.";
const KEEP_THUMBS_OFF: &str =
    "Off: thumbnails are kept in memory while qveil runs, and not on disk.";
const KEEP_THUMBS_ON: &str = "On: the thumbnails you were shown are kept on disk, in ~/.cache/qveil/thumbs, in files only you can read (at most 64 MiB, the least recently shown going first), so that they aren't fetched again. They are a record, on this computer, of the pictures you were shown. Turning this off, clearing the history or qveil wipe deletes them.";
const KEEP_THUMBS_POINT: &str = "Thumbnails are kept on disk: ~/.cache/qveil/thumbs is a record, on this computer, of the pictures you were shown, until you turn that off or clear the history.";
const LENSES: &str = "A refresh sends one search per topic, each a request of its own. YouTube sees those searches, and a length, date or order rule that it has a filter for is sent with the search as YouTube's own filter; a topic whose filtered search finds nothing is searched a second time without it. Your wording, the other rules and the ranking stay on this computer.";
const LENSES_SHORT: &str =
    "A refresh sends each topic to YouTube as a search of its own, sometimes twice.";

/// What is said about resume when positions are kept for `days` (0: not at all).
fn resume(days: u32) -> String {
    match days {
        0 => "Off: qveil doesn't remember where you stopped.".into(),
        _ => format!(
            "On: qveil remembers where you stopped in an unfinished video for {}, in a file only you can read. After that it is no longer offered, and it is deleted the next time qveil starts or saves a position. A finished video is forgotten at once.",
            if days == 1 {
                "1 day".to_string()
            } else {
                format!("{days} days")
            }
        ),
    }
}

/// The text for the route `net` is, and for the settings in `cfg` that
/// change what is true.
pub fn describe(net: &Net, cfg: &Config) -> Privacy {
    let kind = Kind::of(net);
    let points = kind
        .route_points()
        .into_iter()
        .chain(EVERYWHERE.iter().copied())
        // What is only true while a setting is on.
        .chain(cfg.topic_feeds.then_some(TOPICS_POINT))
        .chain(cfg.prepare_next.then_some(PREPARE_POINT))
        .chain(cfg.keep_thumbnails.then_some(KEEP_THUMBS_POINT))
        .map(String::from)
        .collect();
    Privacy {
        summary: kind.summary().into(),
        points,
        routes: ROUTES,
        pages: Pages {
            search: SEARCH,
            subscriptions: SUBSCRIPTIONS,
            history_off: HISTORY_OFF,
            history_on: HISTORY_ON,
            resume: resume(cfg.resume_days),
            comments: COMMENTS,
            comments_short: COMMENTS_SHORT,
            lenses: LENSES,
            lenses_short: LENSES_SHORT,
            topics: if cfg.topic_feeds {
                TOPICS_ON
            } else {
                TOPICS_OFF
            },
            topics_on: TOPICS_ON,
            topics_off: TOPICS_OFF,
            prepare_on: PREPARE_ON,
            prepare_off: PREPARE_OFF,
            keep_thumbs_on: KEEP_THUMBS_ON,
            keep_thumbs_off: KEEP_THUMBS_OFF,
        },
    }
}

/// The README's privacy section: everything above, for every route, as
/// Markdown. `qveil-core privacy --markdown` prints it, and a test fails
/// when the README says anything else.
pub fn markdown() -> String {
    let mut md = String::new();
    md.push_str("qveil can't make you invisible to YouTube: it still has to ask YouTube for videos. The goal is to give YouTube as little as possible to link together, and to keep everything else on your machine.\n\n");

    md.push_str("**The routes**\n\n| Route | |\n|---|---|\n");
    for (name, line) in [
        ("Direct", ROUTES.direct),
        ("Tor", ROUTES.tor),
        ("Proxy", ROUTES.proxy),
    ] {
        md.push_str(&format!("| **{name}** | {line} |\n"));
    }

    md.push_str("\n**What a route does**\n\n");
    for (kind, name) in [
        (Kind::Direct, "Direct"),
        (Kind::Tor, "Tor (127.0.0.1:9050)"),
        (Kind::Socks, "Another SOCKS proxy"),
        (
            Kind::SocksShared,
            "A SOCKS proxy with credentials in its URL",
        ),
        (Kind::Http, "An HTTP or HTTPS proxy"),
    ] {
        md.push_str(&format!(
            "- **{name}.** {}\n",
            kind.route_points().join(" ")
        ));
    }

    md.push_str("\n**On every route**\n\n");
    for point in EVERYWHERE {
        md.push_str(&format!("- {point}\n"));
    }

    md.push_str("\n**The details**\n\n| | |\n|---|---|\n");
    for (title, text) in DETAILS {
        md.push_str(&format!("| **{title}** | {text} |\n"));
    }
    md.push_str("| **Telemetry** | None. |\n");

    md.push_str(&format!("\n{STILL_SEES}\n"));

    md.push_str("\n**On the pages**\n\n");
    let (resume_on, resume_off) = (resume(Config::default().resume_days), resume(0));
    for (name, line) in [
        ("Search", SEARCH),
        ("Subscriptions", SUBSCRIPTIONS),
        ("Lenses", LENSES),
        ("Lenses, in short", LENSES_SHORT),
        ("Comments", COMMENTS),
        ("Comments, in short", COMMENTS_SHORT),
        ("History, off", HISTORY_OFF),
        ("History, on", HISTORY_ON),
        ("Resume, on", resume_on.as_str()),
        ("Resume, off", resume_off.as_str()),
        ("Topic feeds, off", TOPICS_OFF),
        ("Topic feeds, on", TOPICS_ON),
        ("Getting the next video ready, off", PREPARE_OFF),
        ("Getting the next video ready, on", PREPARE_ON),
        ("Keeping thumbnails on disk, off", KEEP_THUMBS_OFF),
        ("Keeping thumbnails on disk, on", KEEP_THUMBS_ON),
    ] {
        md.push_str(&format!("- **{name}.** {line}\n"));
    }
    md.trim_end().to_string()
}

/// The text for the route `net` is, for the command line.
pub fn text(net: &Net, cfg: &Config) -> String {
    let p = describe(net, cfg);
    let mut out = format!("{}\n", p.summary);
    for point in &p.points {
        out.push_str(&format!("\n- {point}"));
    }
    out.push_str(&format!("\n- Resume. {}", p.pages.resume));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use serde_json::{Value, json};

    const BEGIN: &str = "<!-- privacy:begin -->";
    const END: &str = "<!-- privacy:end -->";

    fn net(proxy: Option<&str>) -> Net {
        Net::new(proxy.map(String::from), true).unwrap()
    }

    /// One route of each kind.
    fn routes() -> Vec<(Kind, Net)> {
        vec![
            (Kind::Direct, net(None)),
            (Kind::Tor, net(Some(TOR_PROXY))),
            (Kind::Socks, net(Some("socks5h://10.0.0.2:1080"))),
            (Kind::Socks, net(Some("socks5h://127.0.0.1:9150"))),
            (
                Kind::SocksShared,
                net(Some("socks5h://me:pw@10.0.0.2:1080")),
            ),
            (
                Kind::SocksShared,
                net(Some("socks5h://me:pw@127.0.0.1:9050")),
            ),
            (Kind::Http, net(Some("http://10.0.0.1:3128"))),
            (Kind::Http, net(Some("http://me:pw@10.0.0.1:3128"))),
        ]
    }

    /// Every sentence there is, with the route it is for.
    fn everything() -> Vec<String> {
        let mut all = vec![markdown()];
        for (_, n) in routes() {
            let p = describe(&n, &Config::default());
            all.push(p.summary);
            all.extend(p.points);
        }
        all
    }

    #[test]
    fn the_kind_follows_what_the_route_really_does() {
        for (kind, n) in routes() {
            assert_eq!(Kind::of(&n), kind, "{}", n.describe());
        }
    }

    #[test]
    fn the_readme_has_exactly_what_the_engine_prints() {
        let readme = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../README.md"))
            .expect("the README is beside core/");
        let (_, rest) = readme
            .split_once(BEGIN)
            .unwrap_or_else(|| panic!("README.md has no {BEGIN}"));
        let (section, _) = rest
            .split_once(END)
            .unwrap_or_else(|| panic!("README.md has no {END}"));
        assert!(
            section.trim() == markdown(),
            "README.md's privacy section is out of date: put the output of \
             `qveil-core privacy --markdown` between {BEGIN} and {END}"
        );
    }

    #[test]
    fn the_words_that_were_wrong_are_not_used_and_the_right_ones_are() {
        for text in everything() {
            for wrong in [
                "never sent",
                "Never sent",
                "can't link",
                "cannot link",
                "can't be linked",
                "unlinkable",
                // With resume on, qveil does remember some of it.
                "doesn't remember what you watch",
                "keeps no list of what you watch",
                // Some rules go to YouTube as its own filters.
                "the filters and the ranking stay",
                // Nothing deletes a position at the moment it expires.
                "then deletes it",
            ] {
                assert!(!text.contains(wrong), "{wrong:?} in: {text}");
            }
        }
        // The README says all of these somewhere.
        let md = markdown();
        for right in [
            "never stored",
            "fresh SOCKS credentials per request (a separate circuit if this is Tor)",
            "not isolated",
            "links the two circuits",
            "fingerprint",
            "one more lookup of that video",
            "sent with the search as YouTube's own filter",
            "searched a second time",
            "the next time qveil starts or saves a position",
            "keeps no history",
            "one search per topic",
            "a finished video is forgotten at once",
        ] {
            assert!(
                md.contains(right),
                "{right:?} is missing from the README text"
            );
        }
    }

    #[test]
    fn each_route_says_what_is_true_of_it() {
        let says = |n: &Net, words: &str| {
            let p = describe(n, &Config::default());
            p.summary.contains(words) || p.points.iter().any(|s| s.contains(words))
        };
        let direct = net(None);
        assert!(says(&direct, "your IP address"));
        // Nothing about circuits or credentials where there are none.
        let p = describe(&direct, &Config::default());
        assert!(
            !p.points
                .iter()
                .any(|s| s.contains("circuit") && s.contains("credentials"))
        );

        for isolated in [net(Some(TOR_PROXY)), net(Some("socks5h://10.0.0.2:1080"))] {
            assert!(says(
                &isolated,
                "fresh SOCKS credentials per request (a separate circuit if this is Tor)"
            ));
            assert!(says(&isolated, "links the two circuits"));
            assert!(!says(&isolated, "not isolated"));
        }
        // A proxy that can't isolate says so, and doesn't claim a circuit each.
        for shared in [
            net(Some("socks5h://me:pw@127.0.0.1:9050")),
            net(Some("http://10.0.0.1:3128")),
        ] {
            assert!(says(&shared, "not isolated"));
            assert!(!says(&shared, "fresh SOCKS credentials"));
            assert!(!says(&shared, "links the two circuits"));
        }
        // Only the preset is Tor.
        assert!(says(&net(Some(TOR_PROXY)), "Through Tor"));
        assert!(!says(&net(Some("socks5h://127.0.0.1:9150")), "Through Tor"));
    }

    #[test]
    fn credentials_never_appear_in_what_is_said() {
        for (_, n) in routes() {
            for text in [
                describe(&n, &Config::default()).summary,
                describe(&n, &Config::default()).points.join("\n"),
            ] {
                assert!(!text.contains("pw") && !text.contains("me:"), "{text}");
            }
        }
    }

    /// What the window is coded against.
    #[test]
    fn the_json_has_the_shape_the_window_expects() {
        let v = serde_json::to_value(describe(&net(Some(TOR_PROXY)), &Config::default())).unwrap();
        let keys = |v: &Value| -> Vec<String> {
            let mut k: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
            k.sort();
            k
        };
        assert_eq!(keys(&v), ["pages", "points", "routes", "summary"]);
        assert_eq!(keys(&v["routes"]), ["direct", "proxy", "tor"]);
        assert_eq!(
            keys(&v["pages"]),
            [
                "comments",
                "comments_short",
                "history_off",
                "history_on",
                "keep_thumbs_off",
                "keep_thumbs_on",
                "lenses",
                "lenses_short",
                "prepare_off",
                "prepare_on",
                "resume",
                "search",
                "subscriptions",
                "topics",
                "topics_off",
                "topics_on",
            ]
        );
        assert!(v["summary"].is_string());
        let points = v["points"].as_array().unwrap();
        assert!(points.len() >= 4 && points.iter().all(|p| p.is_string()));
        // One sentence for a tooltip, and one for under a lens.
        let summary = v["summary"].as_str().unwrap();
        assert_eq!(summary.matches(". ").count(), 0, "{summary}");
        for key in ["lenses_short", "comments_short"] {
            let short = v["pages"][key].as_str().unwrap();
            assert_eq!(short.matches(". ").count(), 0, "{short}");
            assert!(short.ends_with('.') && short.len() < 100, "{short}");
        }
        assert_eq!(
            json!(describe(&net(None), &Config::default()).routes)["direct"],
            "YouTube sees your IP address."
        );
    }

    #[test]
    fn resume_says_the_live_number_of_days() {
        let says = |days: u32| {
            let cfg = Config {
                resume_days: days,
                ..Config::default()
            };
            describe(&net(None), &cfg).pages.resume
        };
        assert!(
            says(7).starts_with("On: ") && says(7).contains("for 7 days,"),
            "{}",
            says(7)
        );
        assert!(says(1).contains("for 1 day,"), "{}", says(1));
        assert!(says(0).starts_with("Off: "), "{}", says(0));
        assert!(!says(0).contains("days"));
        // The command line says it too.
        assert!(text(&net(None), &Config::default()).contains("for 7 days,"));
        // The README describes both states.
        let md = markdown();
        assert!(md.contains("**Resume, on.**") && md.contains("**Resume, off.**"));
        assert!(md.contains("**Comments.**") && md.contains("**Lenses.**"));
    }

    #[test]
    fn the_short_line_about_lenses_is_true_on_its_own() {
        let pages = describe(&net(None), &Config::default()).pages;
        let short = pages.lenses_short;
        // Each topic is its own search, and a topic can be sent twice (see
        // `app::search_topics`), which the first sentence of the long text
        // leaves out.
        assert!(short.contains("each topic"), "{short}");
        assert!(short.contains("a search of its own"), "{short}");
        assert!(short.contains("twice"), "{short}");
        assert!(pages.lenses.contains("searched a second time"));
        assert!(markdown().contains(&format!("- **Lenses, in short.** {short}\n")));
    }

    #[test]
    fn the_short_line_about_comments_is_true_on_its_own() {
        let pages = describe(&net(None), &Config::default()).pages;
        let short = pages.comments_short;
        // They cost a request of their own, and only when asked for.
        assert!(short.contains("on demand"), "{short}");
        assert!(short.contains("one extra request"), "{short}");
        assert!(pages.comments.contains("only when you open them"));
        assert!(markdown().contains(&format!("- **Comments, in short.** {short}\n")));
    }

    #[test]
    fn topic_feeds_say_how_the_day_is_counted() {
        let on = describe(&net(None), &Config::default()).pages.topics_on;
        for right in [
            "at most twelve searches in a day",
            "each counted before it is sent",
            "none for a group whose results are less than a day old",
            "Turned off, what was found is deleted",
            "when the day's searches were made is kept until each is a day old",
            "turning it off and on again doesn't start the count over",
            "each is deleted by the next time qveil starts after that",
            // `qveil wipe` keeps the setting and deletes the count, and the
            // count goes by the clock.
            "both by this computer's clock (changing the clock, or deleting qveil's data, can start the day over)",
        ] {
            assert!(on.contains(right), "{right:?} is missing from: {on}");
        }
        // Where that is kept is said with the rest of what is kept.
        assert!(markdown().contains("when recent topic searches were made live in"));
    }

    #[test]
    fn getting_the_next_video_ready_is_said_while_it_is_on() {
        let net = Net::new(None, true).unwrap();
        let off = describe(&net, &Config::default());
        assert!(!off.points.iter().any(|p| p == PREPARE_POINT));
        assert!(off.pages.prepare_off.starts_with("Off"));
        let on = describe(
            &net,
            &Config {
                prepare_next: true,
                ..Config::default()
            },
        );
        assert!(on.points.iter().any(|p| p == PREPARE_POINT));
        // What it costs: a lookup YouTube sees, of a video that may not be watched.
        assert!(
            on.pages.prepare_on.contains("YouTube") && on.pages.prepare_on.contains("never watch")
        );
        assert!(on.pages.prepare_on.contains("pointed at it"));
    }

    #[test]
    fn keeping_thumbnails_on_disk_is_said_while_it_is_on() {
        let net = Net::new(None, true).unwrap();
        let off = describe(&net, &Config::default());
        assert!(!off.points.iter().any(|p| p == KEEP_THUMBS_POINT));
        assert!(off.pages.keep_thumbs_off.starts_with("Off"));
        let on = describe(
            &net,
            &Config {
                keep_thumbnails: true,
                ..Config::default()
            },
        );
        assert!(on.points.iter().any(|p| p == KEEP_THUMBS_POINT));
        assert!(
            on.pages.keep_thumbs_on.contains("record")
                && on.pages.keep_thumbs_on.contains("deletes them")
        );
        assert!(markdown().contains("the thumbnails you were shown in `~/.cache/qveil`"));
    }

    /// The window's tests run against a stand-in engine, which has to say
    /// what this one says where the window shows it word for word.
    #[test]
    fn the_stand_in_engine_says_the_same() {
        let standin = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../tests/shots/engine.py"
        ))
        .expect("the stand-in engine is in tests/shots");
        for (name, text) in [
            ("TOPICS_ON", TOPICS_ON),
            ("TOPICS_OFF", TOPICS_OFF),
            ("LENSES_SHORT", LENSES_SHORT),
            ("COMMENTS_SHORT", COMMENTS_SHORT),
            ("PREPARE_OFF", PREPARE_OFF),
            ("PREPARE_ON", PREPARE_ON),
            ("KEEP_THUMBS_OFF", KEEP_THUMBS_OFF),
            ("KEEP_THUMBS_ON", KEEP_THUMBS_ON),
        ] {
            assert!(
                standin.contains(&format!("{name} = \"{text}\"")),
                "tests/shots/engine.py's {name} is not the engine's: {text}"
            );
        }
    }

    #[test]
    fn the_markdown_has_no_trailing_space_and_ends_without_a_blank_line() {
        let md = markdown();
        assert!(!md.ends_with('\n'));
        for line in md.lines() {
            assert_eq!(line, line.trim_end(), "{line:?}");
        }
    }
}
