//! The lens endpoints, and how a search query is read.
//!
//! `GET api/read` says how a text would be read, without asking anyone.
//! `api/lenses` lists, adds, changes and removes lenses. `GET api/lens`
//! answers with a lens's ranked results, fetching them when they are due.

use super::flights::Flights;
use super::http::{NDJSON_HEAD, Request, Watch, respond};
use super::{MAX_SEARCH, Server};
use crate::app::{self, LensRefresh, Runtime};
use crate::errors;
use crate::lens::read::{Reading, read, read_search};
use crate::lens::{self, Lens};
use crate::store::Paths;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::cell::{Cell, RefCell};
use std::io::Write;
use std::net::TcpStream;
use std::sync::{Arc, LazyLock};

/// The body of a request that adds or changes a lens.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Written {
    text: String,
}

/// Whether a search query is to be handled here: it has a filter to apply,
/// and something to search for. Anything else goes to YouTube exactly as it
/// was typed, whether it could be split into topics ("rick and morty") or
/// only looks like a rule ("no country for old men").
pub(super) fn has_rules(reading: &Reading) -> bool {
    reading.has_filters() && !reading.topics.is_empty()
}

/// A lens as the list shows it: how it reads, and what is stored for it.
fn entry(paths: &Paths, lens: &Lens, now: i64) -> Result<Value> {
    let reading = read(&lens.text, now);
    let stored = lens::cached(paths, &lens.id)?;
    let mut v = json!({
        "id": lens.id,
        "text": lens.text,
        "created": lens.created,
        "tags": reading.tags(),
        "searches": reading.topics,
        "count": stored.as_ref().map_or(0, |c| c.videos.len()),
    });
    if let Some(c) = stored {
        v["fetchedAt"] = json!(c.fetched_at);
    }
    Ok(v)
}

/// The lens refreshes running in this process, by lens.
static LENS_REFRESHES: LazyLock<Flights<String>> = LazyLock::new(Flights::default);

fn write_line(out: &mut TcpStream, line: &Value) -> std::io::Result<()> {
    out.write_all(format!("{line}\n").as_bytes())
}

/// Answers with what `run` comes to, which it is given a way to report its
/// progress to. With `stream` the reply is lines: `{"progress":{"done":N,
/// "total":M}}` as it goes, and last `{"result": …}` or `{"error": …,
/// "code": …}`. Without it, the result is the whole reply and progress isn't
/// sent. A client that has hung up (`watch`) is sent nothing more.
fn answer_as_it_goes<T: Serialize>(
    out: &mut TcpStream,
    watch: &Watch,
    stream: bool,
    run: impl FnOnce(&dyn Fn(usize, usize)) -> Result<T>,
) -> Result<()> {
    let out = RefCell::new(out);
    let started = Cell::new(false);
    let say = |line: Value| {
        let mut out = out.borrow_mut();
        if !started.replace(true) && out.write_all(NDJSON_HEAD.as_bytes()).is_err() {
            return;
        }
        // A client that is gone is noticed by `watch`.
        let _ = write_line(&mut out, &line);
    };
    let progress = |done: usize, total: usize| {
        if stream {
            say(json!({ "progress": { "done": done, "total": total } }));
        }
    };
    let result = run(&progress);
    if watch.gone() {
        return Ok(());
    }
    match (stream, result) {
        (true, Ok(found)) => say(json!({ "result": found })),
        (true, Err(e)) => say(errors::body(&format!("{e:#}"))),
        (false, Ok(found)) => {
            let body = json!(found).to_string();
            return respond(
                &mut out.borrow_mut(),
                200,
                "application/json",
                body.as_bytes(),
            );
        }
        (false, Err(e)) => {
            let body = errors::body(&format!("{e:#}")).to_string();
            return respond(
                &mut out.borrow_mut(),
                500,
                "application/json",
                body.as_bytes(),
            );
        }
    }
    Ok(())
}

impl Server {
    /// `GET api/read?q=…`: how a text would be read in the search box, or
    /// with `&for=lens` as a lens. Nothing is asked of anyone, so the window
    /// can call it as the user types. `rules` says whether there is anything
    /// to show for it: for a search, a filter that will be applied here.
    pub(super) fn read_json(&self, req: &Request) -> Value {
        let text = req.query.get("q").map(String::as_str).unwrap_or_default();
        let lens = req.query.get("for").is_some_and(|f| f == "lens");
        let reading = if lens {
            read(text, app::now())
        } else {
            read_search(text, app::now())
        };
        let rules = if lens {
            reading.has_rules()
        } else {
            has_rules(&reading)
        };
        let mut v = json!({
            "tags": reading.tags(),
            "searches": reading.topics,
            "rules": rules,
        });
        // Only a lens has to be about something: a search is sent as typed.
        if lens && let Err(e) = reading.check() {
            v["error"] = json!(e.to_string());
        }
        v
    }

    /// `GET api/lenses`.
    pub(super) fn lenses_json(&self) -> Result<Value> {
        let now = app::now();
        let list: Result<Vec<Value>> = lens::load(&self.paths)?
            .iter()
            .map(|l| entry(&self.paths, l, now))
            .collect();
        Ok(json!(list?))
    }

    /// `POST api/lenses` (a new one) and `POST api/lenses/<id>` (new words
    /// for an old one).
    pub(super) fn write_lens(&self, id: Option<&str>, body: &[u8]) -> Result<Value> {
        let written: Written = serde_json::from_slice(body).context("bad lens")?;
        let now = app::now();
        let lens = match id {
            None => lens::add(&self.paths, &written.text, now)?,
            Some(id) => lens::edit(&self.paths, id, &written.text, now)?,
        };
        entry(&self.paths, &lens, now)
    }

    /// `GET api/topics?refresh=1`: the topic feeds, after searching again
    /// for those whose results are missing or a day old. Like a lens's
    /// refresh it stops when the client hangs up or the route is left, and
    /// what was found on another route is not kept. With `&stream=1` the
    /// reply is lines, as for a lens: progress, then the result or an error.
    pub(super) fn refresh_topics(
        self: &Arc<Self>,
        out: &mut TcpStream,
        req: &Request,
    ) -> Result<()> {
        let rt = self.runtime();
        let watch = Watch::new(out);
        // Also while the setting is still on: turned off, it stops at once.
        let wanted = || !watch.gone() && self.still_on(&rt) && self.runtime().cfg.topic_feeds;
        answer_as_it_goes(out, &watch, req.flag("stream"), |progress| {
            self.guarded(&rt, app::topics(&rt, &self.paths, true, progress, &wanted))
        })
    }

    /// `GET api/lens?id=…&refresh=0|1&force=0|1`: the lens and its results.
    /// With `&stream=1` the reply is lines, in the feed's format:
    /// `{"progress":{"done":N,"total":M}}` as its searches finish, and last
    /// `{"result": …}` or `{"error": …, "code": …}`.
    pub(super) fn lens(self: &Arc<Self>, out: &mut TcpStream, req: &Request) -> Result<()> {
        let rt = self.runtime();
        let watch = Watch::new(out);
        let refresh = if req.flag("force") {
            LensRefresh::Force
        } else if req.flag("refresh") {
            LensRefresh::Stale
        } else {
            LensRefresh::Off
        };
        let wanted = || !watch.gone() && self.still_on(&rt);
        answer_as_it_goes(out, &watch, req.flag("stream"), |progress| {
            let result = self.refresh_lens(req, refresh, progress, &wanted);
            self.guarded(&rt, result)
        })
    }

    /// `app::lens`, one refresh of a lens at a time: a second request waits
    /// for the first, and then finds fresh what it found (or, if it found
    /// nothing, as when its client hung up, searches for itself). A forced
    /// one that waited for a refresh that has just found something gets that.
    fn refresh_lens(
        &self,
        req: &Request,
        mut refresh: LensRefresh,
        progress: &dyn Fn(usize, usize),
        wanted: &dyn Fn() -> bool,
    ) -> Result<app::LensResult> {
        let id = req.param("id")?;
        let rt = self.runtime();
        // Only what is stored is read: there is nothing to wait for.
        if refresh == LensRefresh::Off {
            return app::lens(&rt, &self.paths, id, refresh, progress, wanted);
        }
        let lens = lens::get(&self.paths, id)?;
        let fetched = || {
            lens::cached(&self.paths, &lens.id)
                .ok()
                .flatten()
                .map(|c| c.fetched_at)
        };
        let before = fetched();
        let Some(turn) = LENS_REFRESHES.take(&lens.id, &|| !wanted()) else {
            bail!(crate::ytdlp::CANCELLED);
        };
        if turn.waited && refresh == LensRefresh::Force && fetched() != before {
            refresh = LensRefresh::Stale;
        }
        app::lens(&rt, &self.paths, id, refresh, progress, wanted)
    }

    /// A search whose query has rules: `api/search`, with or without
    /// `&stream=1`. The results can only be ordered once every topic is in,
    /// so the streamed form sends them together at the end.
    pub(super) fn smart_search(
        self: &Arc<Self>,
        out: &mut TcpStream,
        req: &Request,
        rt: &Runtime,
        reading: &Reading,
    ) -> Result<()> {
        let watch = Watch::new(out);
        let n = req.num("n", 40).clamp(1, MAX_SEARCH);
        let wanted = || !watch.gone() && self.still_on(rt);
        let found = app::smart_search(rt, &self.paths, reading, n, &wanted);
        let found = self.guarded(rt, found);
        if watch.gone() {
            return Ok(());
        }
        match found {
            Err(e) => {
                let body = errors::body(&format!("{e:#}")).to_string();
                respond(out, 500, "application/json", body.as_bytes())
            }
            Ok(found) if !req.flag("stream") => respond(
                out,
                200,
                "application/json",
                json!(found).to_string().as_bytes(),
            ),
            Ok(found) if found.is_empty() => respond(out, 200, "application/x-ndjson", b""),
            Ok(found) => {
                out.write_all(NDJSON_HEAD.as_bytes())?;
                for video in &found {
                    write_line(out, &json!(video))?;
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::app::{self, Runtime};
    use crate::config::Config;
    use crate::server::testkit::*;
    use crate::store::testing::{TempDir, script};
    use serde_json::Value;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpStream;
    use std::time::Duration;

    /// A yt-dlp that answers every search with two videos, as lines (`-j`)
    /// or as one object (`-J`), and keeps a list of what it was asked. The
    /// files beside it steer it: `dur` is the videos' length in seconds
    /// (1900 without it), `delay` a number of seconds to wait first.
    fn searching_ytdlp(dir: &std::path::Path) -> String {
        script(
            &dir.join("yt-dlp"),
            r#"d=$(dirname "$0")
whole=
while [ $# -gt 0 ]; do [ "$1" = --config-locations ] && conf="$2"; shift; done
grep -qx -- -J "$conf" && whole=1
tail -n 1 "$conf" >> "$d/asked"
[ -f "$d/delay" ] && sleep "$(cat "$d/delay")"
# `failfor`: a search for what it says fails.
if [ -f "$d/failfor" ] && tail -n 1 "$d/asked" | grep -qF "$(cat "$d/failfor")"; then echo "ERROR: no" >&2; exit 1; fi
# `nofilter`: a search through YouTube's own filter finds nothing.
if [ -f "$d/nofilter" ] && tail -n 1 "$d/asked" | grep -q "results?"; then exit 0; fi
n=$(wc -l < "$d/asked")
dur=$(cat "$d/dur" 2>/dev/null || echo 1900)
now=$(date +%s)
one="{\"id\":\"aaaaaaaaa${n}1\",\"title\":\"first\",\"channel\":\"One\",\"channel_id\":\"UCaaaaaaaaaaaaaaaaaaaaa${n}\",\"duration\":$dur.0,\"timestamp\":$now}"
two="{\"id\":\"aaaaaaaaa${n}2\",\"title\":\"second\",\"channel\":\"Two\",\"channel_id\":\"UCbbbbbbbbbbbbbbbbbbbbb${n}\",\"duration\":$dur.0,\"timestamp\":$now}"
if [ -n "$whole" ]; then echo "{\"entries\":[$one,$two]}"; else echo "$one"; echo "$two"; fi
"#,
        )
    }

    fn rig() -> (TempDir, String, TempDir) {
        let bin = TempDir::new();
        let (_s, base, d) = server_with(Config {
            ytdlp: searching_ytdlp(&bin.0),
            ..Config::default()
        });
        (bin, base, d)
    }

    fn asked(bin: &TempDir) -> Vec<String> {
        std::fs::read_to_string(bin.0.join("asked"))
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect()
    }

    fn json(base: &str, method: &str, rest: &str, body: &str) -> (u16, Value) {
        let (status, text) = http(base, method, &path(base, rest), body);
        let v = serde_json::from_str(&text).unwrap_or_else(|_| panic!("not JSON: {text}"));
        (status, v)
    }

    fn add(base: &str, text: &str) -> Value {
        let body = serde_json::json!({ "text": text }).to_string();
        let (status, v) = json(base, "POST", "api/lenses", &body);
        assert_eq!(status, 200, "{v}");
        v
    }

    #[test]
    fn a_lens_sends_one_search_per_topic_then_answers_from_its_cache() {
        let (bin, base, _d) = rig();
        let lens = add(&base, "bread, sourdough over 10 min");
        let id = lens["id"].as_str().unwrap().to_string();
        assert_eq!(lens["searches"], serde_json::json!(["bread", "sourdough"]));
        assert_eq!(lens["tags"][2]["label"], "longer than");
        assert_eq!(lens["count"], 0);
        assert!(lens.get("fetchedAt").is_none());
        // Adding one asks nobody.
        assert!(asked(&bin).is_empty());

        // Nothing fetched yet, and not asked to: nothing to show.
        let (_, v) = json(&base, "GET", &format!("api/lens?id={id}"), "");
        assert_eq!(v["videos"], serde_json::json!([]));
        assert!(asked(&bin).is_empty());

        let (status, v) = json(&base, "GET", &format!("api/lens?id={id}&refresh=1"), "");
        assert_eq!(status, 200, "{v}");
        // In either order: it is a new one each time.
        let mut sent = asked(&bin);
        sent.sort();
        assert_eq!(sent, ["'ytsearch30:bread'", "'ytsearch30:sourdough'"]);
        let videos = v["videos"].as_array().unwrap();
        assert_eq!(videos.len(), 4);
        for video in videos {
            let why: Vec<&str> = video["why"]
                .as_array()
                .unwrap()
                .iter()
                .map(|w| w.as_str().unwrap())
                .collect();
            assert!(why[0] == "bread" || why[0] == "sourdough", "{why:?}");
            assert_eq!(why[1], "over 10:00");
            assert!(video["id"].is_string() && video["title"].is_string());
        }
        // The topics take turns.
        assert_eq!(videos[0]["why"][0], "bread");
        assert_eq!(videos[1]["why"][0], "sourdough");
        let fetched = v["fetchedAt"].as_i64().unwrap();
        assert_eq!(v["lens"]["id"], id.as_str());

        // Fresh results are not fetched again.
        let (_, again) = json(&base, "GET", &format!("api/lens?id={id}&refresh=1"), "");
        assert_eq!(asked(&bin).len(), 2);
        assert_eq!(again["fetchedAt"], fetched);
        assert_eq!(again["videos"], v["videos"]);
        // Unless that is what was asked for.
        json(&base, "GET", &format!("api/lens?id={id}&force=1"), "");
        assert_eq!(asked(&bin).len(), 4);

        let (_, list) = json(&base, "GET", "api/lenses", "");
        assert_eq!(list[0]["count"], 4);
        assert!(list[0]["fetchedAt"].is_i64());
    }

    #[test]
    fn a_lens_whose_results_are_dated_in_the_future_is_due() {
        let (bin, base, d) = rig();
        let id = add(&base, "bread")["id"].as_str().unwrap().to_string();
        json(&base, "GET", &format!("api/lens?id={id}&refresh=1"), "");
        assert_eq!(asked(&bin).len(), 1);
        // The clock was set back since: what was found seems to be from next week.
        let path = d.paths().lens_cache(&id);
        let mut kept: crate::lens::Cached = crate::store::load(&path).unwrap();
        kept.fetched_at = app::now() + 7 * 24 * 3600;
        crate::store::save_compact(&path, &kept).unwrap();
        json(&base, "GET", &format!("api/lens?id={id}&refresh=1"), "");
        assert_eq!(asked(&bin).len(), 2);
        // A clock put right by a few minutes is not a reason to search again.
        let mut kept: crate::lens::Cached = crate::store::load(&path).unwrap();
        kept.fetched_at = app::now() + 60;
        crate::store::save_compact(&path, &kept).unwrap();
        json(&base, "GET", &format!("api/lens?id={id}&refresh=1"), "");
        assert_eq!(asked(&bin).len(), 2);
    }

    #[test]
    fn a_lens_whose_ranking_cannot_be_done_sends_no_search() {
        let (bin, base, d) = rig();
        let id = add(&base, "bread, sourdough")["id"]
            .as_str()
            .unwrap()
            .to_string();
        // What is followed can't be read, so nothing found could be ranked:
        // the searches would be sent for nothing.
        let subs = d.paths().subscriptions();
        std::fs::create_dir_all(subs.parent().unwrap()).unwrap();
        std::fs::write(&subs, "{ not json").unwrap();
        let (status, v) = json(&base, "GET", &format!("api/lens?id={id}&refresh=1"), "");
        assert_eq!(status, 500, "{v}");
        assert!(asked(&bin).is_empty(), "{:?}", asked(&bin));
    }

    /// Two refreshes of the lens `id` at once, as `first` and then `second`
    /// asks for them; the second once the first's searches are under way.
    fn twice_at_once(bin: &TempDir, base: &str, id: &str, first: &str, second: &str) -> [Value; 2] {
        std::fs::write(bin.0.join("delay"), "0.5").unwrap();
        let ask = |how: &str| {
            let (base, rest) = (base.to_string(), format!("api/lens?id={id}&{how}"));
            std::thread::spawn(move || json(&base, "GET", &rest, ""))
        };
        let before = asked(bin).len();
        let one = ask(first);
        wait_for("the searches to start", Duration::from_secs(10), || {
            asked(bin).len() > before
        });
        let two = ask(second);
        let ((s1, v1), (s2, v2)) = (one.join().unwrap(), two.join().unwrap());
        assert_eq!((s1, s2), (200, 200), "{v1} {v2}");
        [v1, v2]
    }

    #[test]
    fn a_lens_refreshed_twice_at_once_is_searched_for_once() {
        let (bin, base, d) = rig();
        let id = add(&base, "bread, sourdough")["id"]
            .as_str()
            .unwrap()
            .to_string();
        for (first, second) in [
            ("refresh=1", "refresh=1"),
            ("force=1", "force=1"),
            ("force=1", "refresh=1"),
        ] {
            let before = asked(&bin).len();
            if first == "refresh=1" {
                // Due again.
                std::fs::remove_file(d.paths().lens_cache(&id)).unwrap_or(());
            }
            let [one, two] = twice_at_once(&bin, &base, &id, first, second);
            assert_eq!(asked(&bin).len() - before, 2, "{first} then {second}");
            assert_eq!(one["fetchedAt"], two["fetchedAt"]);
            assert_eq!(one["videos"], two["videos"]);
        }
    }

    #[test]
    fn lenses_are_changed_and_removed_and_a_bad_one_is_refused() {
        let (_bin, base, _d) = rig();
        let id = add(&base, "bread")["id"].as_str().unwrap().to_string();
        let (status, v) = json(
            &base,
            "POST",
            &format!("api/lenses/{id}"),
            r#"{"text":"rye bread"}"#,
        );
        assert_eq!(status, 200, "{v}");
        assert_eq!(v["text"], "rye bread");
        let (status, v) = json(&base, "POST", "api/lenses", r#"{"text":"no music"}"#);
        assert_eq!(status, 500);
        assert!(
            v["error"]
                .as_str()
                .unwrap()
                .starts_with("Say what it should be about"),
            "{v}"
        );
        let (status, _) = json(&base, "POST", &format!("api/lenses/{id}/rm"), "");
        assert_eq!(status, 200);
        let (_, list) = json(&base, "GET", "api/lenses", "");
        assert_eq!(list, serde_json::json!([]));
        let (status, v) = json(&base, "GET", &format!("api/lens?id={id}&refresh=1"), "");
        assert_eq!(status, 500);
        assert!(
            v["error"].as_str().unwrap().contains("no longer exists"),
            "{v}"
        );
    }

    #[test]
    fn a_streamed_lens_reports_each_topic_then_the_result() {
        let (_bin, base, _d) = rig();
        let id = add(&base, "bread, sourdough")["id"]
            .as_str()
            .unwrap()
            .to_string();
        let mut c = TcpStream::connect(addr(&base)).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let target = path(&base, &format!("api/lens?id={id}&refresh=1&stream=1"));
        write!(c, "GET {target} HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let lines: Vec<String> = BufReader::new(c).lines().map_while(Result::ok).collect();
        let blank = lines.iter().position(|l| l.is_empty()).unwrap();
        assert!(lines[0].starts_with("HTTP/1.1 200"), "{lines:?}");
        let body: Vec<Value> = lines[blank + 1..]
            .iter()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let progress: Vec<(u64, u64)> = body
            .iter()
            .filter_map(|l| {
                let p = l.get("progress")?;
                Some((p["done"].as_u64()?, p["total"].as_u64()?))
            })
            .collect();
        assert_eq!(progress, [(0, 2), (1, 2), (2, 2)]);
        let last = body.last().unwrap();
        assert_eq!(last["result"]["videos"].as_array().unwrap().len(), 4);
    }

    #[test]
    fn reading_is_offline_and_says_when_a_topic_is_missing() {
        let (bin, base, _d) = rig();
        let (status, v) = json(&base, "GET", "api/read?q=talks%20over%2030%20min", "");
        assert_eq!(status, 200);
        assert_eq!(v["rules"], true);
        assert_eq!(v["searches"], serde_json::json!(["talks"]));
        assert_eq!(
            v["tags"],
            serde_json::json!([
                { "label": "about", "value": "talks" },
                { "label": "longer than", "value": "30:00" }
            ])
        );
        assert!(v.get("error").is_none());
        let (_, v) = json(&base, "GET", "api/read?q=rust", "");
        assert_eq!(v["rules"], false);
        // The search box and a lens read "no …" differently, and say so.
        let (_, v) = json(&base, "GET", "api/read?q=no%20man%27s%20sky%2C%20bases", "");
        assert_eq!(v["rules"], false);
        let (_, v) = json(
            &base,
            "GET",
            "api/read?q=bases%2C%20no%20music&for=lens",
            "",
        );
        assert_eq!(
            v["tags"][1],
            serde_json::json!({ "label": "not", "value": "music" })
        );
        // Two topics and no filter: nothing for the search box to apply.
        let (_, v) = json(&base, "GET", "api/read?q=rick%20and%20morty", "");
        assert_eq!(v["rules"], false);
        let (status, v) = json(&base, "GET", "api/read?q=no%20music&for=lens", "");
        assert_eq!(status, 200);
        assert!(
            v["error"]
                .as_str()
                .unwrap()
                .starts_with("Say what it should be about"),
            "{v}"
        );
        assert!(asked(&bin).is_empty(), "reading asks nobody");
    }

    #[test]
    fn a_plain_search_reaches_youtube_unchanged_and_one_with_rules_is_filtered_here() {
        let (bin, base, _d) = rig();
        let search = |q: &str| json(&base, "GET", &format!("api/search?q={q}"), "").1;
        let v = search("rust%20in%20100%20seconds");
        assert_eq!(
            asked(&bin).last().unwrap(),
            "'ytsearch40:rust in 100 seconds'"
        );
        assert_eq!(v.as_array().unwrap().len(), 2);
        assert!(v[0].get("why").is_none());
        // Quotes are searched as typed, quotes and all.
        search("%22over%209000%22");
        assert_eq!(asked(&bin).last().unwrap(), r#"'ytsearch40:"over 9000"'"#);
        // More than one thing to search for, and nothing else: one search, as typed.
        for q in [
            "rick and morty",
            "Paris, Texas",
            "no man's sky, base building",
        ] {
            let encoded: String = url::form_urlencoded::byte_serialize(q.as_bytes()).collect();
            let v = search(&encoded);
            let sent = format!("'ytsearch40:{}'", q.replace('\'', r"'\''"));
            assert_eq!(asked(&bin).last().unwrap(), &sent);
            assert!(v[0].get("why").is_none(), "{q}");
        }
        search("rust%20%22borrow%20checker%22%20explained");
        assert_eq!(
            asked(&bin).last().unwrap(),
            r#"'ytsearch40:rust "borrow checker" explained'"#
        );
        // What only looks like rules, with nothing left to search for, is a search too.
        let v = search("no%20country%20for%20old%20men");
        assert_eq!(
            asked(&bin).last().unwrap(),
            "'ytsearch40:no country for old men'"
        );
        assert!(v[0].get("why").is_none());
        // A rule and a topic: the topic is searched, the rule is applied here.
        let v = search("talks%20over%2010%20min");
        assert_eq!(asked(&bin).last().unwrap(), "'ytsearch40:talks'");
        assert_eq!(v.as_array().unwrap().len(), 2);
        assert_eq!(v[0]["why"], serde_json::json!(["talks", "over 10:00"]));
        // Videos that are too short for it are not shown.
        std::fs::write(bin.0.join("dur"), "100").unwrap();
        assert_eq!(search("talks%20over%2010%20min"), serde_json::json!([]));
    }

    #[test]
    fn a_filtered_topic_asks_youtube_with_the_filter_and_falls_back_without_it() {
        let (bin, base, _d) = rig();
        let id = add(&base, "conference talks over 30 min")["id"]
            .as_str()
            .unwrap()
            .to_string();
        let (status, v) = json(&base, "GET", &format!("api/lens?id={id}&refresh=1"), "");
        assert_eq!(status, 200, "{v}");
        assert_eq!(
            asked(&bin),
            ["'https://www.youtube.com/results?search_query=conference+talks&sp=EgQQARgC'"]
        );
        assert_eq!(v["videos"].as_array().unwrap().len(), 2);
        // The window is told what is searched for, not how.
        assert_eq!(v["searches"], serde_json::json!(["conference talks"]));

        // A filter that finds nothing is not trusted: the topic is searched plainly.
        std::fs::write(bin.0.join("nofilter"), "").unwrap();
        let (_, v) = json(&base, "GET", &format!("api/lens?id={id}&force=1"), "");
        assert_eq!(asked(&bin).len(), 3);
        assert_eq!(asked(&bin)[2], "'ytsearch30:conference talks'");
        assert_eq!(v["videos"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn a_streamed_search_with_rules_sends_what_was_kept() {
        let (_bin, base, _d) = rig();
        let mut c = TcpStream::connect(addr(&base)).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let target = path(&base, "api/search?q=talks%20over%2030%20min&stream=1");
        write!(c, "GET {target} HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let lines: Vec<String> = BufReader::new(c).lines().map_while(Result::ok).collect();
        let blank = lines.iter().position(|l| l.is_empty()).unwrap();
        let body: Vec<Value> = lines[blank + 1..]
            .iter()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(body.len(), 2, "{lines:?}");
        assert_eq!(body[0]["why"][1], "over 30:00");
    }

    #[test]
    fn a_lens_refreshed_while_the_route_changes_is_an_error_and_is_not_saved() {
        let bin = TempDir::new();
        let (s, base, _d) = server_with(Config {
            ytdlp: searching_ytdlp(&bin.0),
            ..Config::default()
        });
        let id = add(&base, "bread")["id"].as_str().unwrap().to_string();
        std::fs::write(bin.0.join("delay"), "1").unwrap();
        let asking = {
            let (base, id) = (base.clone(), id.clone());
            std::thread::spawn(move || {
                json(&base, "GET", &format!("api/lens?id={id}&refresh=1"), "")
            })
        };
        wait_for("yt-dlp to start", Duration::from_secs(10), || {
            !asked(&bin).is_empty()
        });
        switch_to_tor(&base);
        let (status, v) = asking.join().unwrap();
        assert_eq!(status, 500, "{v}");
        assert!(
            v["error"].as_str().unwrap().contains("The route changed"),
            "{v}"
        );
        assert!(!s.paths.lens_cache(&id).exists());
    }

    /// Three channels: two about hand-tool woodworking and one about systems
    /// programming, with what they uploaded lately; and one more that is
    /// followed, whose videos a search is going to find.
    fn follow_some_channels(d: &TempDir) {
        use crate::feed::{FeedCache, Seen};
        use crate::subs::{Sub, Subs};
        use crate::video::Video;
        let paths = d.paths();
        let channels = [
            (
                "UCwood000000000000000000a",
                "Northlight Woodworks",
                vec![
                    "A dovetail box with four hand tools",
                    "Sharpening a hand plane properly",
                    "Dovetail joints without a jig",
                    "Restoring an old hand plane",
                ],
            ),
            (
                "UCwood000000000000000000b",
                "Quiet Bench",
                vec![
                    "Hand plane basics",
                    "My first dovetail",
                    "Tuning a hand plane",
                    "Dovetail saw comparison",
                ],
            ),
            (
                "UCsys0000000000000000000a",
                "Low Level Hours",
                vec![
                    "Why the borrow checker says no",
                    "Allocators, from scratch",
                    "A bump allocator in 50 lines",
                    "The borrow checker and lifetimes",
                ],
            ),
            // The channel the first search's first result is from.
            ("UCaaaaaaaaaaaaaaaaaaaaa1", "One", vec![]),
        ];
        let mut subs = Subs::default();
        let mut cache = FeedCache::default();
        for (id, name, titles) in channels {
            subs.add(Sub {
                id: id.into(),
                name: name.into(),
            });
            for (n, title) in titles.iter().enumerate() {
                let video = Video {
                    // (The last letter tells the two woodworking channels apart.)
                    id: format!("{}{}{n:02}", &id[2..10], &id[id.len() - 1..]),
                    title: (*title).into(),
                    channel: Some(name.into()),
                    channel_id: Some(id.into()),
                    published: Some(1_700_000_000 - n as i64),
                    duration: None,
                    views: None,
                    short: false,
                    live: false,
                };
                cache.seen.insert(
                    video.id.clone(),
                    Seen {
                        video,
                        first_seen: 0,
                        last_seen: 0,
                    },
                );
            }
        }
        crate::store::save(&paths.subscriptions(), &subs).unwrap();
        crate::store::save_compact(&paths.feed_cache(), &cache).unwrap();
    }

    #[test]
    fn topic_feeds_ask_nobody_until_they_are_turned_on_and_then_once_a_day() {
        let (bin, base, d) = rig();
        follow_some_channels(&d);
        // Off, as it is unless it is turned on: nothing is worked out, nothing searched.
        let (_, v) = json(&base, "GET", "api/topics", "");
        assert_eq!(v, serde_json::json!({ "enabled": false, "groups": [] }));
        let (status, v) = json(&base, "GET", "api/topics?refresh=1", "");
        assert_eq!(status, 200, "{v}");
        assert_eq!(v["groups"], serde_json::json!([]));
        assert!(asked(&bin).is_empty());

        let (status, cfg) = json(&base, "POST", "api/config", r#"{"topicFeeds":true}"#);
        assert_eq!(status, 200, "{cfg}");
        assert_eq!(cfg["topicFeeds"], true);
        // Listing them is reading what is on this machine.
        let (_, v) = json(&base, "GET", "api/topics", "");
        assert_eq!(v["enabled"], true);
        let groups = v["groups"].as_array().unwrap();
        assert_eq!(groups.len(), 2, "{v}");
        assert_eq!(
            groups[0]["channels"],
            serde_json::json!(["Northlight Woodworks", "Quiet Bench"])
        );
        assert_eq!(groups[0]["searches"][0], "hand plane");
        assert_eq!(groups[0]["videos"], serde_json::json!([]));
        assert!(groups[0].get("fetchedAt").is_none());
        assert_eq!(
            groups[1]["channels"],
            serde_json::json!(["Low Level Hours"])
        );
        assert!(asked(&bin).is_empty());

        // A refresh sends each group's searches, and nothing else.
        let (status, v) = json(&base, "GET", "api/topics?refresh=1", "");
        assert_eq!(status, 200, "{v}");
        let mut wanted: Vec<String> = v["groups"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|g| g["searches"].as_array().unwrap().clone())
            .map(|s| format!("'ytsearch20:{}'", s.as_str().unwrap()))
            .collect();
        wanted.sort();
        // In no fixed order.
        let sorted = || {
            let mut sent = asked(&bin);
            sent.sort();
            sent
        };
        assert_eq!(sorted(), wanted);
        let first = &v["groups"][0];
        assert!(first["fetchedAt"].as_i64().unwrap() > 0);
        // What was found, with the phrase that found it; and nothing from a
        // channel that is already followed (the first search's first result).
        let videos = first["videos"].as_array().unwrap();
        assert!(!videos.is_empty(), "{first}");
        assert_eq!(videos[0]["why"][0], "hand plane");
        for video in videos {
            assert_ne!(video["channel_id"], "UCaaaaaaaaaaaaaaaaaaaaa1", "{video}");
        }
        let all = v["groups"].to_string();
        assert!(
            !all.contains("aaaaaaaaa11"),
            "a followed channel's video is on a topic shelf"
        );

        // Within the day, a refresh has nothing to do.
        let (_, again) = json(&base, "GET", "api/topics?refresh=1", "");
        assert_eq!(sorted(), wanted);
        assert_eq!(again["groups"][0]["videos"], first["videos"]);

        // A group that is hidden is gone, and stays gone.
        let id = first["id"].as_str().unwrap();
        let (status, _) = json(&base, "POST", &format!("api/topics/hide?id={id}"), "");
        assert_eq!(status, 200);
        let (_, v) = json(&base, "GET", "api/topics", "");
        assert_eq!(v["groups"].as_array().unwrap().len(), 1);
        assert_eq!(
            v["groups"][0]["channels"],
            serde_json::json!(["Low Level Hours"])
        );

        // Turned off again, there is nothing, and nothing is asked.
        json(&base, "POST", "api/config", r#"{"topicFeeds":false}"#);
        let (_, v) = json(&base, "GET", "api/topics?refresh=1", "");
        assert_eq!(v, serde_json::json!({ "enabled": false, "groups": [] }));
        assert_eq!(sorted(), wanted);
    }

    fn topics_on(base: &str) {
        let (status, cfg) = json(base, "POST", "api/config", r#"{"topicFeeds":true}"#);
        assert_eq!(status, 200, "{cfg}");
    }

    #[test]
    fn a_topic_whose_search_fails_stops_the_refresh() {
        let (bin, base, d) = rig();
        follow_some_channels(&d);
        topics_on(&base);
        std::fs::write(bin.0.join("failfor"), "hand plane").unwrap();
        let (status, v) = json(&base, "GET", "api/topics?refresh=1", "");
        assert_eq!(status, 200, "{v}");
        let groups = v["groups"].as_array().unwrap();
        assert_eq!(groups.len(), 2, "{v}");
        // The one that failed has nothing and is said to have failed...
        assert_eq!(groups[0]["videos"], serde_json::json!([]));
        assert!(groups[0].get("fetchedAt").is_none());
        assert_eq!(v["errors"].as_array().unwrap().len(), 1, "{v}");
        // ...and nothing more was searched for: offline, every search would
        // fail, and each would count.
        let sent = asked(&bin);
        assert_eq!(sent.last().unwrap(), "'ytsearch20:hand plane'", "{sent:?}");
        assert_eq!(sent.iter().filter(|s| s.contains("hand plane")).count(), 1);
    }

    #[test]
    fn turning_topic_feeds_off_stops_a_refresh_that_is_running() {
        let (bin, base, d) = rig();
        follow_some_channels(&d);
        topics_on(&base);
        let (_, listed) = json(&base, "GET", "api/topics", "");
        let all: usize = listed["groups"]
            .as_array()
            .unwrap()
            .iter()
            .map(|g| g["searches"].as_array().unwrap().len())
            .sum();
        assert!(all >= 3, "{listed}");
        std::fs::write(bin.0.join("delay"), "1").unwrap();
        let asking = {
            let base = base.clone();
            std::thread::spawn(move || json(&base, "GET", "api/topics?refresh=1", ""))
        };
        wait_for("yt-dlp to start", Duration::from_secs(10), || {
            !asked(&bin).is_empty()
        });
        json(&base, "POST", "api/config", r#"{"topicFeeds":false}"#);
        let (status, v) = asking.join().unwrap();
        // It stopped: the answer is not the feeds, and the rest was never searched for.
        assert!(status != 200 || v["enabled"] == false, "{v}");
        assert!(asked(&bin).len() < all, "{:?}", asked(&bin));
        // Off, nothing that was worked out is left on disk either.
        assert!(!d.paths().topics_cache().exists());
        let (_, v) = json(&base, "GET", "api/topics", "");
        assert_eq!(v, serde_json::json!({ "enabled": false, "groups": [] }));
    }

    #[test]
    fn a_hidden_topic_stays_hidden_when_its_channels_change() {
        use crate::feed::{FeedCache, Seen};
        use crate::subs::{Sub, Subs};
        use crate::video::Video;
        let (bin, base, d) = rig();
        follow_some_channels(&d);
        topics_on(&base);
        let (_, v) = json(&base, "GET", "api/topics", "");
        let wood = v["groups"][0]["id"].as_str().unwrap().to_string();
        assert_eq!(v["groups"][0]["searches"][0], "hand plane");
        let (status, _) = json(&base, "POST", &format!("api/topics/hide?id={wood}"), "");
        assert_eq!(status, 200);

        // One more woodworking channel is followed: the group is another
        // group now, about the same thing.
        let paths = d.paths();
        let mut subs: Subs = crate::store::load(&paths.subscriptions()).unwrap();
        subs.add(Sub {
            id: "UCwood000000000000000000c".into(),
            name: "Shavings".into(),
        });
        crate::store::save(&paths.subscriptions(), &subs).unwrap();
        let mut cache: FeedCache = crate::store::load_cache(&paths.feed_cache()).unwrap();
        for (n, title) in [
            "Hand plane restoration",
            "A dovetail in ten minutes",
            "Which hand plane first",
            "Dovetail markers",
        ]
        .iter()
        .enumerate()
        {
            let video = Video {
                id: format!("woodc0000{n:02}"),
                title: (*title).into(),
                channel: Some("Shavings".into()),
                channel_id: Some("UCwood000000000000000000c".into()),
                published: Some(1_700_000_000 - n as i64),
                duration: None,
                views: None,
                short: false,
                live: false,
            };
            cache.seen.insert(
                video.id.clone(),
                Seen {
                    video,
                    first_seen: 0,
                    last_seen: 0,
                },
            );
        }
        crate::store::save_compact(&paths.feed_cache(), &cache).unwrap();

        let (_, v) = json(&base, "GET", "api/topics?refresh=1", "");
        let groups = v["groups"].as_array().unwrap();
        assert_eq!(groups.len(), 1, "{v}");
        assert_eq!(
            groups[0]["channels"],
            serde_json::json!(["Low Level Hours"])
        );
        for line in asked(&bin) {
            assert!(
                !line.contains("hand plane") && !line.contains("dovetail"),
                "{line}"
            );
        }
    }

    #[test]
    fn a_day_has_a_budget_of_searches_whatever_the_groups_are() {
        let (bin, base, d) = rig();
        follow_some_channels(&d);
        topics_on(&base);
        // Twelve searches were already made in the last day (for groups that are gone).
        let now = crate::app::now();
        let spent = crate::topics::Searched {
            at: vec![now - 60; 12],
        };
        crate::store::save_compact(&d.paths().topic_searches(), &spent).unwrap();
        let (status, v) = json(&base, "GET", "api/topics?refresh=1", "");
        assert_eq!(status, 200, "{v}");
        assert!(asked(&bin).is_empty(), "{:?}", asked(&bin));
        assert_eq!(v["groups"][0]["videos"], serde_json::json!([]));
        // Said once, for all the groups it held back.
        let errors = v["errors"].as_array().unwrap();
        assert_eq!(errors.len(), 1, "{v}");
        assert_eq!(errors[0], crate::app::TOPICS_SPENT, "{v}");
        // A day on, there is a budget again.
        let old = crate::topics::Searched {
            at: vec![now - 25 * 3600; 12],
        };
        crate::store::save_compact(&d.paths().topic_searches(), &old).unwrap();
        json(&base, "GET", "api/topics?refresh=1", "");
        assert!(!asked(&bin).is_empty());
        // What is noted is when each was sent, and nothing else, in a file only
        // you can read.
        use std::os::unix::fs::PermissionsExt;
        let file = d.paths().topic_searches();
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let kept: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!(kept.as_object().unwrap().len(), 1, "{kept}");
        assert_eq!(kept["at"].as_array().unwrap().len(), asked(&bin).len());
    }

    #[test]
    fn a_day_with_some_room_left_says_so_and_searches_what_fits() {
        let (bin, base, d) = rig();
        follow_some_channels(&d);
        topics_on(&base);
        // Eleven made in the last day: one more fits, which the first group's
        // two searches don't.
        let now = crate::app::now();
        let spent = crate::topics::Searched {
            at: vec![now - 60; crate::topics::BUDGET - 1],
        };
        crate::store::save_compact(&d.paths().topic_searches(), &spent).unwrap();
        let (status, v) = json(&base, "GET", "api/topics?refresh=1", "");
        assert_eq!(status, 200, "{v}");
        assert_eq!(asked(&bin), ["'ytsearch20:borrow checker'"]);
        let errors = v["errors"].as_array().unwrap();
        assert_eq!(errors.len(), 1, "{v}");
        let said = errors[0].as_str().unwrap();
        // Not that the twelve were made, nor that one more is all it takes.
        assert!(
            !said.contains("twelve") && !said.contains("oldest"),
            "{said}"
        );
        assert_eq!(said, crate::app::TOPICS_SPENT);
    }

    /// Topic feeds on, outside the server: for driving `app::topics` directly.
    fn topics_at(bin: &TempDir, d: &TempDir) -> Runtime {
        follow_some_channels(d);
        let cfg = Config {
            ytdlp: searching_ytdlp(&bin.0),
            topic_feeds: true,
            ..Config::default()
        };
        Runtime::new(cfg, &d.paths(), None, false).unwrap()
    }

    /// How many searches the groups of `follow_some_channels` make in all.
    fn all_searches(rt: &Runtime, d: &TempDir) -> usize {
        app::topics(rt, &d.paths(), false, &|_, _| {}, &|| true)
            .unwrap()
            .groups
            .iter()
            .map(|g| g.group.searches.len())
            .sum()
    }

    #[test]
    fn a_refresh_cut_off_in_the_middle_of_a_group_has_counted_what_it_sent() {
        let (bin, d) = (TempDir::new(), TempDir::new());
        let rt = topics_at(&bin, &d);
        // Each refresh is stopped during its first search (the window went
        // away, say): the search had gone out all the same.
        for _ in 0..20 {
            let before = asked(&bin).len();
            let wanted = || asked(&bin).len() == before;
            let r = app::topics(&rt, &d.paths(), true, &|_, _| {}, &wanted);
            assert!(r.is_err() || asked(&bin).len() == before);
        }
        assert_eq!(
            asked(&bin).len(),
            crate::topics::BUDGET,
            "{:?}",
            asked(&bin)
        );
    }

    #[test]
    fn a_topic_refresh_pauses_between_all_its_searches_and_stops_in_a_pause() {
        let (bin, d) = (TempDir::new(), TempDir::new());
        let mut rt = topics_at(&bin, &d);
        let all = all_searches(&rt, &d);
        assert!(all >= 2, "{all}");
        let paused = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = std::sync::Arc::clone(&paused);
        rt.pause = std::sync::Arc::new(move |_| {
            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        app::topics(&rt, &d.paths(), true, &|_, _| {}, &|| true).unwrap();
        assert_eq!(asked(&bin).len(), all);
        // Between one group's searches and the next group's alike.
        assert_eq!(paused.load(std::sync::atomic::Ordering::SeqCst), all - 1);

        // Due again, and turned off during the first pause: nothing more is
        // sent, or counted.
        std::fs::remove_file(d.paths().topics_cache()).unwrap();
        std::fs::remove_file(d.paths().topic_searches()).unwrap();
        let off = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let turn_off = std::sync::Arc::clone(&off);
        rt.pause = std::sync::Arc::new(move |wanted| {
            turn_off.store(true, std::sync::atomic::Ordering::SeqCst);
            assert!(!wanted());
        });
        let wanted = || !off.load(std::sync::atomic::Ordering::SeqCst);
        let r = app::topics(&rt, &d.paths(), true, &|_, _| {}, &wanted);
        assert!(r.is_err());
        assert_eq!(asked(&bin).len(), all + 1);
        let counted: crate::topics::Searched =
            crate::store::load(&d.paths().topic_searches()).unwrap();
        assert_eq!(counted.at.len(), 1);
    }

    #[test]
    fn turning_topic_feeds_off_and_on_does_not_start_the_day_over() {
        let (bin, base, d) = rig();
        follow_some_channels(&d);
        // (Each round's refresh would send all of the groups' searches.)
        for _ in 0..6 {
            topics_on(&base);
            let (status, v) = json(&base, "GET", "api/topics?refresh=1", "");
            assert_eq!(status, 200, "{v}");
            json(&base, "POST", "api/config", r#"{"topicFeeds":false}"#);
        }
        assert!(
            asked(&bin).len() <= crate::topics::BUDGET,
            "{}: {:?}",
            asked(&bin).len(),
            asked(&bin)
        );
    }

    #[test]
    fn two_refreshes_at_once_search_for_each_group_once_and_keep_to_the_budget() {
        let (bin, d) = (TempDir::new(), TempDir::new());
        let rt = topics_at(&bin, &d);
        let all = all_searches(&rt, &d);
        assert!((2..crate::topics::BUDGET).contains(&all), "{all}");
        // An older version kept the day's searches in the cache: there were
        // already some today.
        let earlier = crate::topics::BUDGET - all;
        let now = app::now();
        std::fs::create_dir_all(&d.paths().cache).unwrap();
        std::fs::write(
            d.paths().topics_cache(),
            format!(
                r#"{{"groups":{{}},"searched":{:?}}}"#,
                vec![now - 60; earlier]
            ),
        )
        .unwrap();
        std::fs::write(bin.0.join("delay"), "0.2").unwrap();
        std::thread::scope(|s| {
            for _ in 0..2 {
                s.spawn(|| app::topics(&rt, &d.paths(), true, &|_, _| {}, &|| true).unwrap());
            }
        });
        let sent = asked(&bin);
        let mut once = sent.clone();
        once.sort();
        once.dedup();
        assert_eq!(once.len(), sent.len(), "a search was sent twice: {sent:?}");
        assert!(sent.len() + earlier <= crate::topics::BUDGET, "{sent:?}");
    }

    #[test]
    fn a_refresh_with_no_connection_counts_one_search() {
        let (bin, d) = (TempDir::new(), TempDir::new());
        let rt = topics_at(&bin, &d);
        // Every search fails: the refresh stops at its first.
        std::fs::write(bin.0.join("failfor"), "ytsearch").unwrap();
        for n in 1..=5 {
            let r = app::topics(&rt, &d.paths(), true, &|_, _| {}, &|| true).unwrap();
            assert_eq!(r.errors.len(), 1, "{:?} {:?}", r.errors, asked(&bin));
            assert_eq!(asked(&bin).len(), n, "{:?}", asked(&bin));
        }
        let counted: crate::topics::Searched =
            crate::store::load(&d.paths().topic_searches()).unwrap();
        assert_eq!(counted.at.len(), 5);
    }

    #[test]
    fn results_dated_in_the_future_are_due() {
        let (bin, d) = (TempDir::new(), TempDir::new());
        let rt = topics_at(&bin, &d);
        let all = all_searches(&rt, &d);
        app::topics(&rt, &d.paths(), true, &|_, _| {}, &|| true).unwrap();
        assert_eq!(asked(&bin).len(), all);
        // The clock was set back since: what was found seems to be from next week.
        let mut kept: crate::topics::Results =
            crate::store::load(&d.paths().topics_cache()).unwrap();
        for found in kept.groups.values_mut() {
            found.fetched_at = app::now() + 7 * 24 * 3600;
        }
        crate::store::save_compact(&d.paths().topics_cache(), &kept).unwrap();
        app::topics(&rt, &d.paths(), true, &|_, _| {}, &|| true).unwrap();
        assert_eq!(asked(&bin).len(), 2 * all);
    }

    #[test]
    fn an_unreadable_count_of_the_days_searches_means_no_searches() {
        let (bin, d) = (TempDir::new(), TempDir::new());
        let rt = topics_at(&bin, &d);
        std::fs::create_dir_all(&d.paths().data).unwrap();
        std::fs::write(d.paths().data.join("topic-searches.json"), "{cut sh").unwrap();
        let r = app::topics(&rt, &d.paths(), true, &|_, _| {}, &|| true).unwrap();
        assert!(asked(&bin).is_empty(), "{:?}", asked(&bin));
        assert_eq!(r.errors.len(), 1, "{:?}", r.errors);
        assert!(r.errors[0].contains("can't be read"), "{:?}", r.errors);
    }

    #[test]
    fn a_refresh_that_waits_for_the_lock_while_topic_feeds_are_turned_off_saves_nothing() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let (bin, d) = (TempDir::new(), TempDir::new());
        let rt = topics_at(&bin, &d);
        let paths = d.paths();
        let listed = app::topics(&rt, &paths, false, &|_, _| {}, &|| true).unwrap();
        let first = listed.groups[0].group.searches.len();
        let on = AtomicBool::new(true);
        let (give, held) = std::sync::mpsc::channel();
        std::thread::scope(|s| {
            let refresh = s.spawn(|| {
                // Once the first group's searches are back, someone else
                // takes the lock before the refresh can save them.
                let progress = |done: usize, _| {
                    if done == first {
                        give.send(crate::store::lock(&paths).unwrap()).unwrap();
                    }
                };
                let wanted = || on.load(Ordering::SeqCst);
                app::topics(&rt, &paths, true, &progress, &wanted).map(|_| ())
            });
            let lock = held.recv().unwrap();
            std::thread::sleep(Duration::from_millis(300));
            // What turning it off does, in its order: off, then forgotten.
            on.store(false, Ordering::SeqCst);
            let _ = std::fs::remove_file(paths.topics_cache());
            drop(lock);
            assert!(refresh.join().unwrap().is_err());
        });
        assert!(!paths.topics_cache().exists(), "found after it was deleted");
    }

    #[test]
    fn a_search_counted_while_topic_feeds_were_turned_off_is_not_sent() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let (bin, d) = (TempDir::new(), TempDir::new());
        let rt = topics_at(&bin, &d);
        let paths = d.paths();
        let on = AtomicBool::new(true);
        let taken = AtomicBool::new(false);
        let (give, held) = std::sync::mpsc::channel();
        std::thread::scope(|s| {
            let refresh = s.spawn(|| {
                // As the first group begins, someone else takes the lock, so
                // that counting its first search has to wait.
                let progress = |done: usize, _| {
                    if done == 0 && !taken.swap(true, Ordering::SeqCst) {
                        give.send(crate::store::lock(&paths).unwrap()).unwrap();
                    }
                };
                let wanted = || on.load(Ordering::SeqCst);
                app::topics(&rt, &paths, true, &progress, &wanted).map(|_| ())
            });
            let lock = held.recv().unwrap();
            std::thread::sleep(Duration::from_millis(300));
            // Turned off while it waits: off, then forgotten.
            on.store(false, Ordering::SeqCst);
            let _ = std::fs::remove_file(paths.topics_cache());
            drop(lock);
            assert!(refresh.join().unwrap().is_err());
        });
        assert!(
            asked(&bin).is_empty(),
            "sent after it was turned off: {:?}",
            asked(&bin)
        );
    }

    /// The engine starting, with topic feeds on or off.
    fn engine_starts(d: &TempDir, topic_feeds: bool) {
        let cfg = Config {
            topic_feeds,
            ..Config::default()
        };
        let rt = Runtime::new(cfg, &d.paths(), None, false).unwrap();
        drop(crate::server::Server::new(rt, d.paths()));
    }

    fn searched_at(d: &TempDir, at: Vec<i64>) {
        let searched = crate::topics::Searched { at };
        crate::store::save_compact(&d.paths().topic_searches(), &searched).unwrap();
    }

    fn searched(d: &TempDir) -> Vec<i64> {
        crate::store::load::<crate::topics::Searched>(&d.paths().topic_searches())
            .unwrap()
            .at
    }

    #[test]
    fn when_the_days_topic_searches_were_made_is_deleted_at_start_once_a_day_old() {
        let d = TempDir::new();
        let now = app::now();
        let old = now - crate::topics::STALE - 60;
        // Topic feeds were turned off; since then a day has gone by for one
        // of the searches, and then the engine starts.
        searched_at(&d, vec![old, now - 60]);
        engine_starts(&d, false);
        assert_eq!(searched(&d), [now - 60]);
        // A day on for all of them: nothing is left, not even the file.
        searched_at(&d, vec![old; 3]);
        engine_starts(&d, false);
        assert!(!d.paths().topic_searches().exists());
        // The same while they are on.
        searched_at(&d, vec![old; 3]);
        engine_starts(&d, true);
        assert!(!d.paths().topic_searches().exists());
        // Starting with nothing to tidy leaves nothing behind.
        let fresh = TempDir::new();
        engine_starts(&fresh, false);
        assert!(!fresh.paths().data.exists());
    }

    #[test]
    fn turning_topic_feeds_off_deletes_the_searches_that_are_a_day_old() {
        let d = TempDir::new();
        let now = app::now();
        searched_at(&d, vec![now - crate::topics::STALE - 60, now - 60]);
        app::forget_topics(&d.paths()).unwrap();
        assert_eq!(searched(&d), [now - 60]);
        searched_at(&d, vec![now - crate::topics::STALE; 2]);
        app::forget_topics(&d.paths()).unwrap();
        assert!(!d.paths().topic_searches().exists());
    }

    #[test]
    fn what_topic_feeds_found_is_deleted_at_start_while_they_are_off() {
        let d = TempDir::new();
        let cache = d.paths().topics_cache();
        std::fs::create_dir_all(&d.paths().cache).unwrap();
        // Left behind: a setting changed by hand, or a forget that failed.
        std::fs::write(&cache, r#"{"groups":{}}"#).unwrap();
        engine_starts(&d, true);
        assert!(cache.exists());
        engine_starts(&d, false);
        assert!(!cache.exists());
    }

    #[test]
    fn topic_feeds_are_off_before_what_they_found_is_deleted() {
        let (_bin, base, d) = rig();
        follow_some_channels(&d);
        topics_on(&base);
        // The store is busy (a refresh is saving, say).
        let lock = crate::store::lock(&d.paths()).unwrap();
        let off = {
            let base = base.clone();
            std::thread::spawn(move || json(&base, "POST", "api/config", r#"{"topicFeeds":false}"#))
        };
        // Already off for everything that asks, while the deleting waits its turn.
        wait_for("topic feeds to be off", Duration::from_secs(5), || {
            json(&base, "GET", "api/config", "").1["topicFeeds"] == false
        });
        drop(lock);
        assert_eq!(off.join().unwrap().0, 200);
    }
}
