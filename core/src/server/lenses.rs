//! The lens endpoints, and how a search query is read.
//!
//! `GET api/read` says how a text would be read, without asking anyone.
//! `api/lenses` lists, adds, changes and removes lenses. `GET api/lens`
//! answers with a lens's ranked results, fetching them when they are due.

use super::http::{NDJSON_HEAD, Request, Watch, respond};
use super::{MAX_SEARCH, Server};
use crate::app::{self, LensRefresh, Runtime};
use crate::errors;
use crate::lens::read::{Reading, read, read_search};
use crate::lens::{self, Lens};
use crate::store::Paths;
use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{Value, json};
use std::cell::{Cell, RefCell};
use std::io::Write;
use std::net::TcpStream;
use std::sync::Arc;

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

fn write_line(out: &mut TcpStream, line: &Value) -> std::io::Result<()> {
    out.write_all(format!("{line}\n").as_bytes())
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

    /// `GET api/lens?id=…&refresh=0|1&force=0|1`: the lens and its results.
    /// With `&stream=1` the reply is lines, in the feed's format:
    /// `{"progress":{"done":N,"total":M}}` as its searches finish, and last
    /// `{"result": …}` or `{"error": …, "code": …}`.
    pub(super) fn lens(self: &Arc<Self>, out: &mut TcpStream, req: &Request) -> Result<()> {
        let rt = self.runtime();
        let watch = Watch::new(out);
        let stream = req.flag("stream");
        let refresh = if req.flag("force") {
            LensRefresh::Force
        } else if req.flag("refresh") {
            LensRefresh::Stale
        } else {
            LensRefresh::Off
        };
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
        let wanted = || !watch.gone() && self.still_on(&rt);
        let result = req
            .param("id")
            .and_then(|id| app::lens(&rt, &self.paths, id, refresh, &progress, &wanted));
        let result = self.guarded(&rt, result);
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
for a in "$@"; do [ "$a" = -J ] && whole=1; done
while [ $# -gt 0 ]; do [ "$1" = --config-locations ] && tail -n 1 "$2" >> "$d/asked"; shift; done
[ -f "$d/delay" ] && sleep "$(cat "$d/delay")"
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
        assert_eq!(
            asked(&bin),
            ["'ytsearch30:bread'", "'ytsearch30:sourdough'"]
        );
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
}
