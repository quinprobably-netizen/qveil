//! `GET api/comments`: a video's comments, fetched when the window asks.
//!
//! They cost a lookup of their own, so they are never fetched ahead of
//! time, and what was fetched is kept (in memory only) so that going back to
//! a tab doesn't ask YouTube again.

use super::Server;
use super::flights::Flights;
use super::http::{Request, Watch, respond};
use crate::comments::{Comments, Sort};
use crate::errors;
use crate::ids;
use crate::util::lock;
use anyhow::{Context, Result, bail};
use serde_json::json;
use std::collections::VecDeque;
use std::net::TcpStream;
use std::sync::{Arc, Mutex};

/// How many answers are kept.
const KEPT: usize = 16;
/// How many threads are asked for when the window doesn't say.
const DEFAULT: usize = 40;
/// The fewest and the most threads one request may ask for.
const RANGE: (usize, usize) = (20, 200);

/// What was asked: the video, the order, how many threads, with replies or not.
type Asked = (String, Sort, usize, bool);

/// The answers fetched lately, oldest first. In memory only: comments are
/// what other people wrote under a video the user opened, which is not
/// something to leave on disk.
#[derive(Default)]
pub(super) struct Kept {
    answers: Mutex<VecDeque<(Asked, Arc<Comments>)>>,
    /// The lookups running, so that the same one isn't run twice at once.
    flights: Flights<Asked>,
}

impl Kept {
    fn get(&self, asked: &Asked) -> Option<Arc<Comments>> {
        lock(&self.answers)
            .iter()
            .find(|(a, _)| a == asked)
            .map(|(_, c)| Arc::clone(c))
    }

    fn put(&self, asked: Asked, comments: Arc<Comments>) {
        let mut kept = lock(&self.answers);
        kept.retain(|(a, _)| *a != asked);
        kept.push_back((asked, comments));
        while kept.len() > KEPT {
            kept.pop_front();
        }
    }
}

impl Server {
    /// `GET api/comments?v=…&sort=top|new&n=…&replies=0|1`. Like a play
    /// lookup, it ends (and yt-dlp with it) when the window hangs up: the
    /// tab was closed, or another order was asked for.
    pub(super) fn comments(self: &Arc<Self>, out: &mut TcpStream, req: &Request) -> Result<()> {
        let watch = Watch::new(out);
        match self.comments_for(req, &|| watch.gone()) {
            Ok(found) => {
                let body = json!(*found).to_string();
                respond(out, 200, "application/json", body.as_bytes())
            }
            // Nobody is listening for the reason.
            Err(_) if watch.gone() => Ok(()),
            Err(e) => {
                let body = errors::body(&format!("{e:#}")).to_string();
                respond(out, 500, "application/json", body.as_bytes())
            }
        }
    }

    fn comments_for(&self, req: &Request, abort: &dyn Fn() -> bool) -> Result<Arc<Comments>> {
        let target = req.param("v")?;
        let video = ids::parse_video(target)
            .with_context(|| format!("{target:?} isn't a YouTube video"))?;
        let sort = match req.query.get("sort").map(String::as_str) {
            Some("new") => Sort::New,
            _ => Sort::Top,
        };
        let n = req.num("n", DEFAULT).clamp(RANGE.0, RANGE.1);
        let asked: Asked = (video.id.clone(), sort, n, req.flag("replies"));
        if let Some(found) = self.comments.get(&asked) {
            return Ok(found);
        }
        // The same lookup may be running for another request: this one waits
        // for it, and then finds its answer kept. If it ended without one,
        // this one asks for itself.
        let Some(_turn) = self.comments.flights.take(&asked, abort) else {
            bail!(crate::ytdlp::CANCELLED);
        };
        if let Some(found) = self.comments.get(&asked) {
            return Ok(found);
        }
        let rt = self.runtime();
        let found = rt.yt.comments(&video.watch_url(), sort, n, asked.3, abort);
        // Not kept if the route changed meanwhile: what came over a route
        // that was left is dropped, like everything else that did.
        let found = Arc::new(self.guarded(&rt, found)?);
        self.comments.put(asked, Arc::clone(&found));
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use crate::config::Config;
    use crate::server::testkit::*;
    use crate::store::testing::{TempDir, script};
    use serde_json::Value;
    use std::io::Write;
    use std::net::TcpStream;
    use std::time::Duration;

    const ID: &str = "dQw4w9WgXcQ";

    /// A yt-dlp that keeps its arguments and its config file, counts its
    /// runs, waits `delay` seconds and answers with two threads and a reply.
    fn commenting_ytdlp(dir: &std::path::Path, delay: &str) -> String {
        script(
            &dir.join("yt-dlp"),
            &format!(
                r#"d=$(dirname "$0")
n=$(cat "$d/runs" 2>/dev/null || echo 0)
echo $((n + 1)) > "$d/runs.new" && mv "$d/runs.new" "$d/runs"
echo "$*" > "$d/args"
while [ $# -gt 0 ]; do [ "$1" = --config-locations ] && cp "$2" "$d/asked"; shift; done
sleep {delay}
cat <<'JSON'
{{"comment_count":1204,"comments":[
 {{"id":"a","parent":"root","author":"@chan","text":"Fixed","is_pinned":true,"author_is_uploader":true}},
 {{"id":"b","parent":"root","author":"@mara","text":"Nice"}},
 {{"id":"b.1","parent":"b","author":"@x","text":"Agreed"}}]}}
JSON
"#
            ),
        )
    }

    fn get(base: &str, query: &str) -> (u16, String) {
        http(
            base,
            "GET",
            &path(base, &format!("api/comments?{query}")),
            "",
        )
    }

    #[test]
    fn comments_ask_for_top_level_only_and_are_answered_from_memory_the_second_time() {
        let bin = TempDir::new();
        let (_s, base, _d) = server_with(Config {
            ytdlp: commenting_ytdlp(&bin.0, "0"),
            ..Config::default()
        });
        let (status, body) = get(&base, &format!("v={ID}"));
        assert_eq!(status, 200, "{body}");
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["count"], 1204);
        assert_eq!(v["sort"], "top");
        assert_eq!(v["comments"].as_array().unwrap().len(), 2);
        assert_eq!(v["comments"][1]["replies"][0]["text"], "Agreed");

        let args = std::fs::read_to_string(bin.0.join("args")).unwrap();
        assert!(
            args.contains("--write-comments") && args.contains("--skip-download"),
            "{args}"
        );
        // Neither the video nor what is asked about it is on the command line.
        assert!(
            !args.contains(ID) && !args.contains("max_comments"),
            "{args}"
        );
        let asked = std::fs::read_to_string(bin.0.join("asked")).unwrap();
        assert!(
            asked.contains(
                "--extractor-args 'youtube:skip=hls,dash;comment_sort=top;max_comments=40,40,0,0'"
            ),
            "{asked}"
        );
        let target = asked.lines().last().unwrap();
        assert!(target.contains("watch") && target.contains(ID), "{target}");

        // The same question again is answered without asking YouTube.
        assert_eq!(get(&base, &format!("v={ID}")).1, body);
        assert_eq!(runs(&bin.0), 1);

        // Another question is another lookup: newest first, as many as are
        // allowed, with replies.
        let (status, body) = get(&base, &format!("v={ID}&sort=new&n=5000&replies=1"));
        assert_eq!(status, 200, "{body}");
        assert_eq!(runs(&bin.0), 2);
        let asked = std::fs::read_to_string(bin.0.join("asked")).unwrap();
        assert!(
            asked.contains("comment_sort=new;max_comments=800,200,600,3'"),
            "{asked}"
        );
        // And too few is brought up to the least that is asked for.
        get(&base, &format!("v={ID}&n=1"));
        let asked = std::fs::read_to_string(bin.0.join("asked")).unwrap();
        assert!(asked.contains("max_comments=20,20,0,0'"), "{asked}");
    }

    #[test]
    fn the_same_comments_asked_for_twice_at_once_are_looked_up_once() {
        let bin = TempDir::new();
        let (_s, base, _d) = server_with(Config {
            ytdlp: commenting_ytdlp(&bin.0, "2"),
            ..Config::default()
        });
        let ask = || {
            let base = base.clone();
            std::thread::spawn(move || get(&base, &format!("v={ID}")))
        };
        let first = ask();
        wait_for("the lookup to start", Duration::from_secs(10), || {
            runs(&bin.0) >= 1
        });
        let second = ask();
        let (first, second) = (first.join().unwrap(), second.join().unwrap());
        assert_eq!(first.0, 200, "{}", first.1);
        assert_eq!(second, first);
        assert_eq!(runs(&bin.0), 1);
    }

    #[test]
    fn a_request_that_waited_for_one_whose_client_hung_up_looks_up_for_itself() {
        let bin = TempDir::new();
        let (_s, base, _d) = server_with(Config {
            ytdlp: commenting_ytdlp(&bin.0, "1"),
            ..Config::default()
        });
        let mut c = TcpStream::connect(addr(&base)).unwrap();
        let target = path(&base, &format!("api/comments?v={ID}"));
        write!(c, "GET {target} HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        wait_for("the lookup to start", Duration::from_secs(10), || {
            runs(&bin.0) >= 1
        });
        let second = {
            let base = base.clone();
            std::thread::spawn(move || get(&base, &format!("v={ID}")))
        };
        drop(c);
        let (status, body) = second.join().unwrap();
        assert_eq!(status, 200, "{body}");
        assert_eq!(runs(&bin.0), 2);
    }

    #[test]
    fn what_is_not_a_video_has_no_comments() {
        let (_s, base, _d) = server();
        let (status, body) = get(&base, "v=https://evil.example/x");
        assert_eq!(status, 500);
        assert!(body.contains("isn't a YouTube video"), "{body}");
    }

    #[test]
    fn hanging_up_stops_the_comments_lookup() {
        let bin = TempDir::new();
        let (_s, base, _d) = server_with(Config {
            ytdlp: hanging_ytdlp(&bin.0),
            ..Config::default()
        });
        let mut c = TcpStream::connect(addr(&base)).unwrap();
        let target = path(&base, &format!("api/comments?v={ID}"));
        write!(c, "GET {target} HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let (pid, private) = hanging_running(&bin.0);
        drop(c);
        assert_stopped_and_cleaned_up(&pid, &private);
    }

    #[test]
    fn comments_fetched_while_the_route_changes_are_an_error_and_are_not_kept() {
        let bin = TempDir::new();
        let (_s, base, _d) = server_with(Config {
            ytdlp: commenting_ytdlp(&bin.0, "1"),
            ..Config::default()
        });
        let asking = {
            let base = base.clone();
            std::thread::spawn(move || get(&base, &format!("v={ID}")))
        };
        wait_for("yt-dlp to start", Duration::from_secs(10), || {
            bin.0.join("asked").exists()
        });
        switch_to_tor(&base);
        let (status, body) = asking.join().unwrap();
        assert_eq!(status, 500, "{body}");
        assert!(body.contains("The route changed"), "{body}");
        // Nothing of it was kept: the next request asks again.
        let before = runs(&bin.0);
        let (status, body) = get(&base, &format!("v={ID}"));
        assert_eq!(status, 200, "{body}");
        assert_eq!(runs(&bin.0), before + 1);
    }
}
