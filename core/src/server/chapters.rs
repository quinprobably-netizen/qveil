//! `GET api/chapters`: what a video's page data says, fetched when the
//! window asks: its chapters, its channel's name and when it was uploaded.
//!
//! A lookup to play a video has them, with the watch page; but a quick one
//! (`quick_lookup`, experimental: see `ytdlp::LOOKUP`) leaves out YouTube's
//! page data, so a video whose description names no chapters may still have
//! some (YouTube makes them for some videos), and a video played without a
//! card to take them from (a pasted link) has no channel name or age. The
//! play answer then says so (`chaptersLater`, `detailsLater`), and only then
//! does the window ask here. They cost a lookup of their own, so they are only fetched when the
//! window wants them (the Chapters tab is opened, or the video has no
//! channel name or age to show), one lookup at a time, and what was fetched
//! is kept (in memory only) so that asking again doesn't ask YouTube again.
//! A channel name found this way goes in the video's history entry, if it
//! has one without a name.

use super::Server;
use super::http::{Request, Watch, respond};
use crate::app;
use crate::errors;
use crate::ids::{self, VideoRef};
use crate::util::lock;
use crate::ytdlp::Details;
use anyhow::{Context, Result, bail};
use serde_json::json;
use std::collections::VecDeque;
use std::net::TcpStream;
use std::sync::{Arc, Mutex};

/// How many answers are kept.
const KEPT: usize = 16;

/// The answers fetched lately, oldest first, and the lock that one lookup
/// holds while it runs: a second request for the same video waits for the
/// first and takes its answer, instead of asking YouTube a second time.
#[derive(Default)]
pub(super) struct Kept {
    found: Mutex<VecDeque<(String, Arc<Details>)>>,
    running: Mutex<()>,
}

impl Kept {
    fn get(&self, id: &str) -> Option<Arc<Details>> {
        lock(&self.found)
            .iter()
            .find(|(v, _)| v == id)
            .map(|(_, c)| Arc::clone(c))
    }

    fn put(&self, id: &str, details: Arc<Details>) {
        let mut kept = lock(&self.found);
        kept.retain(|(v, _)| v != id);
        kept.push_back((id.to_string(), details));
        while kept.len() > KEPT {
            kept.pop_front();
        }
    }
}

impl Server {
    /// `GET api/chapters?v=…`: `{"chapters": [{start, title}], "channel",
    /// "published"}`, the last two where the page data has them. Like a play
    /// lookup, it ends (and yt-dlp with it) when the window hangs up: it left
    /// the video.
    pub(super) fn chapters(self: &Arc<Self>, out: &mut TcpStream, req: &Request) -> Result<()> {
        let watch = Watch::new(out);
        match self.details_for(req, &|| watch.gone()) {
            Ok(found) => {
                let mut body = json!({ "chapters": found.chapters });
                if let Some(c) = &found.channel {
                    body["channel"] = json!(c);
                }
                if let Some(p) = found.published {
                    body["published"] = json!(p);
                }
                respond(out, 200, "application/json", body.to_string().as_bytes())
            }
            // Nobody is listening for the reason.
            Err(_) if watch.gone() => Ok(()),
            Err(e) => {
                let body = errors::body(&format!("{e:#}")).to_string();
                respond(out, 500, "application/json", body.as_bytes())
            }
        }
    }

    fn details_for(&self, req: &Request, abort: &dyn Fn() -> bool) -> Result<Arc<Details>> {
        let target = req.param("v")?;
        let video = ids::parse_video(target)
            .with_context(|| format!("{target:?} isn't a YouTube video"))?;
        self.details(&video, abort)
    }

    /// Looks up what `video`'s page data says on a thread of its own, for
    /// the history entry of a video played without its channel's name
    /// (`qveil play` with a pasted link). Nobody waits for it.
    pub fn name_later(self: &Arc<Self>, video: &VideoRef) {
        let (server, video) = (Arc::clone(self), video.clone());
        let _ = std::thread::Builder::new().spawn(move || {
            if let Err(e) = server.details(&video, &|| false) {
                note!("qveil: couldn't look up the video's channel: {e:#}");
            }
        });
    }

    /// What `video`'s page data says: kept, or looked up.
    pub(super) fn details(
        &self,
        video: &VideoRef,
        abort: &dyn Fn() -> bool,
    ) -> Result<Arc<Details>> {
        if let Some(found) = self.chapters.get(&video.id) {
            return Ok(found);
        }
        let _one = lock(&self.chapters.running);
        // The lookup this one waited for may have been for the same video.
        if let Some(found) = self.chapters.get(&video.id) {
            return Ok(found);
        }
        if abort() {
            bail!(crate::ytdlp::CANCELLED);
        }
        let rt = self.runtime();
        let found = rt.yt.details_of(&video.watch_url(), abort);
        // Not kept if the route changed meanwhile: what came over a route
        // that was left is dropped, like everything else that did.
        let found = Arc::new(self.guarded(&rt, found)?);
        self.chapters.put(&video.id, Arc::clone(&found));
        if rt.cfg.history
            && let Some(channel) = &found.channel
            && let Err(e) = {
                // The play this follows adds its entry once it has answered.
                self.history_settled();
                app::name_in_history(&self.paths, &video.id, channel)
            }
        {
            note!("qveil: couldn't name the channel in the history: {e:#}");
        }
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
    /// runs, waits `delay` seconds and answers with two chapters.
    fn chapters_ytdlp(dir: &std::path::Path, delay: &str) -> String {
        script(
            &dir.join("yt-dlp"),
            &format!(
                r#"d=$(dirname "$0")
n=$(cat "$d/runs" 2>/dev/null || echo 0)
echo $((n + 1)) > "$d/runs"
echo "$*" > "$d/args"
while [ $# -gt 0 ]; do [ "$1" = --config-locations ] && cp "$2" "$d/asked"; shift; done
sleep {delay}
echo '{{"duration":600,"channel":"Chan","upload_date":"20251109","chapters":[{{"start_time":0,"title":"Intro"}},{{"start_time":90.5,"title":"Part"}}]}}'
"#
            ),
        )
    }

    fn get(base: &str, query: &str) -> (u16, String) {
        http(
            base,
            "GET",
            &path(base, &format!("api/chapters?{query}")),
            "",
        )
    }

    #[test]
    fn chapters_are_looked_up_once_and_answered_from_memory_after() {
        let bin = TempDir::new();
        let (_s, base, _d) = server_with(Config {
            ytdlp: chapters_ytdlp(&bin.0, "0.3"),
            ..Config::default()
        });
        // Two at once: one lookup between them.
        let both: Vec<_> = (0..2)
            .map(|_| {
                let base = base.clone();
                std::thread::spawn(move || get(&base, &format!("v={ID}")))
            })
            .collect();
        for asking in both {
            let (status, body) = asking.join().unwrap();
            assert_eq!(status, 200, "{body}");
            let v: Value = serde_json::from_str(&body).unwrap();
            assert_eq!(
                v,
                serde_json::json!({
                    "chapters": [{"start": 0, "title": "Intro"}, {"start": 90, "title": "Part"}],
                    "channel": "Chan",
                    "published": 1762646400
                })
            );
        }
        assert_eq!(runs(&bin.0), 1);
        let args = std::fs::read_to_string(bin.0.join("args")).unwrap();
        assert!(
            args.contains("--skip-download") && !args.contains(ID),
            "{args}"
        );
        // The page and all: the quick lookup is off.
        let asked = std::fs::read_to_string(bin.0.join("asked")).unwrap();
        assert!(
            asked.contains("--extractor-args 'youtube:skip=hls,dash'\n"),
            "{asked}"
        );
        assert!(asked.lines().last().unwrap().contains(ID), "{asked}");
        // And again later, from memory.
        assert_eq!(get(&base, &format!("v={ID}")).0, 200);
        assert_eq!(runs(&bin.0), 1);
    }

    #[test]
    fn the_channel_found_with_the_chapters_names_it_in_the_history() {
        for history in [true, false] {
            let bin = TempDir::new();
            let (s, base, _d) = server_with(Config {
                ytdlp: chapters_ytdlp(&bin.0, "0"),
                history,
                ..Config::default()
            });
            // Played from a pasted link: the lookup had no name for the channel.
            crate::app::record_history(&s.paths, watched()).unwrap();
            let (status, body) = get(&base, &format!("v={ID}"));
            assert_eq!(status, 200, "{body}");
            let entry = &crate::app::load_history(&s.paths).unwrap().watched[0];
            let named = entry.channel.as_deref();
            // With the history off, it is not written to.
            assert_eq!(named, history.then_some("Chan"), "history {history}");
        }
    }

    #[test]
    fn a_video_played_without_its_channels_name_has_it_looked_up_for_the_history() {
        let bin = TempDir::new();
        let (s, _base, _d) = server_with(Config {
            ytdlp: chapters_ytdlp(&bin.0, "0"),
            history: true,
            ..Config::default()
        });
        crate::app::record_history(&s.paths, watched()).unwrap();
        s.name_later(&crate::server::testkit::test_video());
        wait_for("the channel to be named", Duration::from_secs(10), || {
            crate::app::load_history(&s.paths).unwrap().watched[0]
                .channel
                .as_deref()
                == Some("Chan")
        });
        assert_eq!(runs(&bin.0), 1);
    }

    #[test]
    fn what_is_not_a_video_has_no_chapters() {
        let (_s, base, _d) = server();
        let (status, body) = get(&base, "v=https://evil.example/x");
        assert_eq!(status, 500);
        assert!(body.contains("isn't a YouTube video"), "{body}");
    }

    #[test]
    fn hanging_up_stops_the_chapters_lookup() {
        let bin = TempDir::new();
        let (_s, base, _d) = server_with(Config {
            ytdlp: hanging_ytdlp(&bin.0),
            ..Config::default()
        });
        let mut c = TcpStream::connect(addr(&base)).unwrap();
        let target = path(&base, &format!("api/chapters?v={ID}"));
        write!(c, "GET {target} HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let (pid, private) = hanging_running(&bin.0);
        drop(c);
        assert_stopped_and_cleaned_up(&pid, &private);
    }

    #[test]
    fn chapters_fetched_while_the_route_changes_are_an_error_and_are_not_kept() {
        let bin = TempDir::new();
        let (_s, base, _d) = server_with(Config {
            ytdlp: chapters_ytdlp(&bin.0, "1"),
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
        let before = runs(&bin.0);
        assert_eq!(get(&base, &format!("v={ID}")).0, 200);
        assert_eq!(runs(&bin.0), before + 1);
    }
}
