//! The JSON endpoints: settings, search, channels, subscriptions, history,
//! playing a video.

use super::http::{NDJSON_HEAD, Request, Watch, respond};
use super::{MAX_SEARCH, Server};
use crate::app::{self, Runtime};
use crate::config::Config;
use crate::errors;
use crate::ids;
use crate::inflight::Why;
use crate::net::{self, TOR_PROXY};
use crate::privacy;
use crate::resume;
use crate::util::lock;
use crate::video::Video;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::Write;
use std::net::TcpStream;
use std::sync::Arc;

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
struct ConfigPatch {
    /// "direct", "tor" or "proxy" (with `proxy`).
    route: Option<String>,
    proxy: Option<String>,
    history: Option<bool>,
    thumbnails: Option<bool>,
    max_height: Option<u32>,
    resume_days: Option<u32>,
}

/// What the window reports while a video plays: `POST api/resume`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Stopped {
    /// The video: its ID or a link to it.
    v: String,
    pos: u64,
    dur: u64,
    title: String,
    #[serde(default)]
    channel: Option<String>,
}

impl Server {
    /// `POST api/play?v=…`: resolves the video and answers with where to play
    /// it. The window can give up on this while yt-dlp is still working (a
    /// click on another video, a closed page), and then yt-dlp is stopped,
    /// with everything it started, instead of running for an answer that has
    /// nowhere to go. The video is added to the history only once the answer
    /// has been written in full: one that never reached the window wasn't watched.
    pub(super) fn play(self: &Arc<Self>, out: &mut TcpStream, req: &Request) -> Result<()> {
        let watch = Watch::new(out);
        let opened = req.param("v").and_then(|target| {
            let v = ids::parse_video(target)
                .with_context(|| format!("{target:?} isn't a YouTube video"))?;
            self.open_until(&v, &|| watch.gone())
        });
        match opened {
            Ok(opened) => {
                let body = json!(opened.info).to_string();
                respond(out, 200, "application/json", body.as_bytes())?;
                self.remember(&opened);
                Ok(())
            }
            // Nobody is listening for the reason.
            Err(_) if watch.gone() => Ok(()),
            Err(e) => {
                let body = errors::body(&format!("{e:#}")).to_string();
                respond(out, 500, "application/json", body.as_bytes())
            }
        }
    }

    /// Everything else: a JSON answer, or the error that came instead.
    pub(super) fn json_api(
        self: &Arc<Self>,
        out: &mut TcpStream,
        req: &Request,
        segs: &[&str],
    ) -> Result<()> {
        match self.api(req, segs) {
            Ok(Some(v)) => respond(out, 200, "application/json", v.to_string().as_bytes()),
            Ok(None) => {
                let body = json!({ "error": "not found", "code": "other" }).to_string();
                respond(out, 404, "application/json", body.as_bytes())
            }
            Err(e) => {
                let body = errors::body(&format!("{e:#}")).to_string();
                respond(out, 500, "application/json", body.as_bytes())
            }
        }
    }

    fn api(self: &Arc<Self>, req: &Request, segs: &[&str]) -> Result<Option<Value>> {
        let rt = self.runtime();
        let paths = &self.paths;
        let v = match (req.method.as_str(), segs) {
            ("GET", ["api", "config"]) => self.config_json(),
            ("POST", ["api", "config"]) => {
                let patch: ConfigPatch =
                    serde_json::from_slice(&req.body).context("bad settings")?;
                self.update_config(patch)?;
                self.config_json()
            }
            ("GET", ["api", "search"]) => {
                // Each result is a line of yt-dlp's output and a card in the
                // window, and `n` comes from the caller.
                let n = req.num("n", 40).clamp(1, MAX_SEARCH);
                let found = app::search(&rt, req.param("q")?, n);
                json!(self.guarded(&rt, found)?)
            }
            ("GET", ["api", "channel"]) => {
                let page = app::channel(&rt, paths, req.param("id")?);
                json!(self.guarded(&rt, page)?)
            }
            ("GET", ["api", "subs"]) => json!(app::load_subs(paths)?.channels),
            ("POST", ["api", "subs", "add"]) => {
                // Checked again under the store's lock, so a lookup that came
                // back from the old route doesn't end up in the list.
                let current = || self.still_on(&rt);
                let added = app::add_sub(&rt, paths, req.param("c")?, &current);
                let (sub, added) = self.guarded(&rt, added)?;
                json!({ "sub": sub, "added": added })
            }
            ("POST", ["api", "subs", "rm"]) => {
                json!(app::remove_sub(paths, req.param("id")?)?)
            }
            ("POST", ["api", "subs", "import"]) => {
                let text = std::str::from_utf8(&req.body).context("the file isn't text")?;
                let (added, total) = app::import_subs(paths, text)?;
                json!({ "added": added, "total": total })
            }
            ("GET", ["api", "history"]) => {
                let mut w = app::load_history(paths)?.watched;
                w.reverse();
                json!(w)
            }
            ("POST", ["api", "history", "clear"]) => {
                app::clear_history(paths)?;
                json!({})
            }
            ("GET", ["api", "read"]) => self.read_json(req),
            ("GET", ["api", "lenses"]) => self.lenses_json()?,
            ("POST", ["api", "lenses"]) => self.write_lens(None, &req.body)?,
            ("POST", ["api", "lenses", id]) => self.write_lens(Some(id), &req.body)?,
            ("POST", ["api", "lenses", id, "rm"]) => {
                crate::lens::remove(paths, id)?;
                json!({})
            }
            ("GET", ["api", "resume"]) => {
                let days = rt.cfg.resume_days;
                let kept: Vec<Value> = resume::list(paths, days, app::now())?
                    .iter()
                    .map(|p| {
                        let mut v = json!(p);
                        v["expires"] = json!(resume::expires(p, days));
                        v
                    })
                    .collect();
                json!(kept)
            }
            ("POST", ["api", "resume"]) => {
                let s: Stopped = serde_json::from_slice(&req.body).context("bad position")?;
                let video = ids::parse_video(&s.v)
                    .with_context(|| format!("{:?} isn't a YouTube video", s.v))?;
                let now = app::now();
                let position = resume::Position {
                    id: video.id,
                    pos: s.pos,
                    dur: s.dur,
                    at: now,
                    title: s.title,
                    channel: s.channel.filter(|c| !c.is_empty()),
                };
                json!({ "noted": resume::note(paths, rt.cfg.resume_days, position, now)? })
            }
            ("POST", ["api", "resume", "rm"]) => {
                let target = req.param("v")?;
                let video = ids::parse_video(target)
                    .with_context(|| format!("{target:?} isn't a YouTube video"))?;
                resume::forget(paths, &video.id)?;
                json!({})
            }
            ("POST", ["api", "resume", "clear"]) => {
                resume::clear(paths)?;
                json!({})
            }
            _ => return Ok(None),
        };
        Ok(Some(v))
    }

    fn config_json(&self) -> Value {
        let rt = self.runtime();
        let proxy = rt.net.proxy();
        let route = match proxy {
            None => "direct",
            Some(TOR_PROXY) => "tor",
            Some(_) => "proxy",
        };
        json!({
            "route": route,
            // Credentials never leave the engine: the window shows this and
            // sends it back unchanged to mean "keep the current proxy".
            "proxy": proxy.map(net::redact),
            "describe": rt.net.describe(),
            "isolated": rt.net.isolates(),
            "torRunning": net::tor_running(),
            "history": rt.cfg.history,
            "thumbnails": rt.cfg.thumbnails,
            "maxHeight": rt.cfg.max_height,
            "resumeDays": rt.cfg.resume_days,
            "configPath": self.paths.config,
            // What the window says about privacy, from the one place that
            // words it (see `privacy`).
            "privacy": privacy::describe(&rt.net, &rt.cfg),
        })
    }

    fn update_config(&self, p: ConfigPatch) -> Result<()> {
        // One change at a time: each reads the current settings, edits them and saves.
        let _one_at_a_time = lock(&self.settings);
        let rt = self.runtime();
        let mut cfg: Config = toml::from_str(&toml::to_string(&rt.cfg)?)?;
        match p.route.as_deref() {
            None => {}
            Some("direct") => cfg.proxy = None,
            Some("tor") => {
                cfg.proxy = Some(TOR_PROXY.into());
                cfg.isolate_circuits = true;
            }
            Some("proxy") => {
                let proxy = p
                    .proxy
                    .filter(|s| !s.trim().is_empty())
                    .context("enter a proxy URL")?;
                let proxy = proxy.trim();
                // The window is only ever shown the proxy with its credentials
                // redacted, and sends that back unchanged when the field wasn't
                // edited. That means "keep the proxy I have".
                cfg.proxy = match rt.net.proxy() {
                    Some(current) if net::redact(current) == proxy => Some(current.into()),
                    _ => Some(proxy.into()),
                };
            }
            Some(other) => bail!("unknown route {other:?}"),
        }
        if let Some(h) = p.history {
            cfg.history = h;
        }
        if let Some(t) = p.thumbnails {
            cfg.thumbnails = t;
        }
        if let Some(m) = p.max_height {
            cfg.max_height = m.clamp(144, 4320);
        }
        if let Some(days) = p.resume_days {
            cfg.resume_days = days.min(365);
        }
        // Validates the proxy before anything is saved.
        let mut next = Runtime::new(cfg, &self.paths, None, false)?;
        next.cfg.save(&self.paths.config)?;
        // Off means forgotten, not just no longer offered. A shorter time
        // takes what is now too old with it.
        if next.cfg.resume_days != rt.cfg.resume_days {
            resume::purge(&self.paths, next.cfg.resume_days, app::now())?;
        }
        let route_changed = !next.net.same_route(&rt.net);
        if !route_changed {
            // The same route keeps its running calls (and the registry that
            // can still stop them at exit); a new route starts with none.
            next = next.keeping_calls_of(&rt);
        }
        // Under the sessions lock (see `register`). Streams stay valid across
        // settings like thumbnails or quality, which don't touch the route; a
        // new route would make their upstream URLs play from the wrong address.
        let mut sessions = lock(&self.sessions);
        *self.runtime.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(next);
        if route_changed {
            // Not just forgotten: a relay already copying a body holds its own
            // reference, and would carry on over the old route.
            for (_, session) in sessions.drain(..) {
                session.cancel();
            }
            drop(sessions);
            // The identities of the pages of thumbnails belong to the old route too.
            self.thumbs.forget_batches();
            // The same goes for a search, a lookup or a refresh's fallback
            // that is running yt-dlp: it is killed, and its caller told to
            // try again. Closed after the swap, so what starts from now on
            // belongs to the new route.
            rt.yt.calls.close(Why::RouteChanged);
        }
        Ok(())
    }

    /// A search whose results are sent as they come in, one JSON object per
    /// line, so the window can show YouTube's first page of results while
    /// the second is still on its way. A search that fails before its first
    /// result is an ordinary error; one that fails later ends with a line
    /// `{"error": …}`.
    pub(super) fn search_stream(&self, out: &mut TcpStream, req: &Request) -> Result<()> {
        let rt = self.runtime();
        let n = req.num("n", 40).clamp(1, MAX_SEARCH);
        let mut started = false;
        let mut gone = false;
        let mut each = |v: Video| {
            let mut line = json!(v).to_string();
            line.push('\n');
            if !started {
                started = true;
                line.insert_str(0, NDJSON_HEAD);
            }
            gone = out.write_all(line.as_bytes()).is_err();
            !gone
        };
        let result = req
            .param("q")
            .and_then(|q| app::search_each(&rt, q, n, &mut each));
        // A search that was running when the route changed ends with the same
        // error as any other, whether or not some results were sent already.
        let result = self.guarded(&rt, result);
        match result {
            Err(e) if !started => {
                let body = errors::body(&format!("{e:#}")).to_string();
                respond(out, 500, "application/json", body.as_bytes())
            }
            Err(e) => {
                let line = format!("{}\n", errors::body(&format!("{e:#}")));
                out.write_all(line.as_bytes())?;
                Ok(())
            }
            Ok(_) if !started && !gone => respond(out, 200, "application/x-ndjson", b""),
            Ok(_) => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::server::testkit::*;
    use crate::store::testing::TempDir;
    use serde_json::Value;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpStream;
    use std::time::{Duration, Instant};

    #[test]
    fn settings_validate_persist_and_apply() {
        let (s, base, _d) = server();
        let (status, body) = http(
            &base,
            "POST",
            &path(&base, "api/config"),
            r#"{"route":"proxy","proxy":"socks5://x:1"}"#,
        );
        assert_eq!(status, 500);
        assert!(body.contains("socks5h"), "{body}");
        assert!(
            !s.paths.config.exists(),
            "an invalid route must not be saved"
        );

        let (status, body) = http(
            &base,
            "POST",
            &path(&base, "api/config"),
            r#"{"route":"tor","history":true}"#,
        );
        assert_eq!(status, 200, "{body}");
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            (
                v["route"].as_str(),
                v["isolated"].as_bool(),
                v["history"].as_bool()
            ),
            (Some("tor"), Some(true), Some(true))
        );
        let saved = Config::load(&s.paths.config).unwrap();
        assert_eq!(saved.proxy.as_deref(), Some(TOR_PROXY));
    }

    #[test]
    fn the_route_label_is_tor_only_for_the_preset_and_isolation_is_reported_truthfully() {
        let (_s, base, _d) = server();
        let set = |body: &str| {
            let (status, body) = http(&base, "POST", &path(&base, "api/config"), body);
            (status, serde_json::from_str::<Value>(&body).unwrap())
        };
        // Tor Browser's port is a proxy like any other: only the preset is "tor".
        let (status, v) = set(r#"{"route":"proxy","proxy":"socks5h://127.0.0.1:9150"}"#);
        assert_eq!(status, 200, "{v}");
        assert_eq!(
            (v["route"].as_str(), v["isolated"].as_bool()),
            (Some("proxy"), Some(true))
        );
        let (_, v) = set(r#"{"route":"proxy","proxy":"socks5h://localhost:9050"}"#);
        assert_eq!(
            (v["route"].as_str(), v["isolated"].as_bool()),
            (Some("proxy"), Some(true))
        );
        // The user's own credentials turn isolation off, and the description says so.
        let (status, v) = set(r#"{"route":"proxy","proxy":"socks5h://me:pw@127.0.0.1:9050"}"#);
        assert_eq!(status, 200, "{v}");
        assert_eq!(
            (v["route"].as_str(), v["isolated"].as_bool()),
            (Some("proxy"), Some(false))
        );
        let said = v["describe"].as_str().unwrap();
        assert!(
            said.contains("not isolated") && !said.contains("pw"),
            "{said}"
        );
        // Sending the redacted proxy back unchanged keeps the credentials.
        let (_, v) = set(&format!(r#"{{"route":"proxy","proxy":{}}}"#, v["proxy"]));
        assert_eq!(v["isolated"].as_bool(), Some(false));
        let (_, v) = set(r#"{"route":"tor"}"#);
        assert_eq!(
            (v["route"].as_str(), v["isolated"].as_bool()),
            (Some("tor"), Some(true))
        );
        assert!(
            v["describe"]
                .as_str()
                .unwrap()
                .contains("fresh credentials per request")
        );
    }

    #[test]
    fn positions_are_noted_listed_forgotten_and_offered_when_playing() {
        let up = Upstream::start();
        let bin = TempDir::new();
        let (_s, base, _d) = server_with(Config {
            ytdlp: fake_ytdlp(&bin.0, &up.url("/video?gen=")),
            ..Config::default()
        });
        let id = test_video().id;
        let body = format!(r#"{{"v":"{id}","pos":845,"dur":2463,"title":"T","channel":"C"}}"#);
        assert_eq!(
            http(&base, "POST", &path(&base, "api/resume"), &body),
            (200, r#"{"noted":"kept"}"#.into())
        );
        let (_, list) = http(&base, "GET", &path(&base, "api/resume"), "");
        let v: Value = serde_json::from_str(&list).unwrap();
        assert_eq!(
            (
                v[0]["id"].as_str(),
                v[0]["pos"].as_u64(),
                v[0]["title"].as_str(),
                v[0]["channel"].as_str()
            ),
            (Some(id.as_str()), Some(845), Some("T"), Some("C"))
        );
        assert_eq!(
            v[0]["expires"].as_i64().unwrap() - v[0]["at"].as_i64().unwrap(),
            7 * 86_400
        );
        let play = |target: &str| -> Value {
            let (status, body) = http(
                &base,
                "POST",
                &path(&base, &format!("api/play?v={target}")),
                "",
            );
            assert_eq!(status, 200, "{body}");
            serde_json::from_str(&body).unwrap()
        };
        assert_eq!(play(&id)["resume"], 845);
        // A link with a start time of its own wins.
        assert!(
            play(&format!("https://youtu.be/{id}?t=60"))
                .get("resume")
                .is_none()
        );
        assert_eq!(
            http(
                &base,
                "POST",
                &path(&base, &format!("api/resume/rm?v={id}")),
                ""
            )
            .0,
            200
        );
        assert_eq!(http(&base, "GET", &path(&base, "api/resume"), "").1, "[]");
        assert!(play(&id).get("resume").is_none());
        // What isn't a video is refused, not filed.
        let (status, body) = http(
            &base,
            "POST",
            &path(&base, "api/resume"),
            r#"{"v":"https://evil.example/x","pos":100,"dur":400,"title":"T"}"#,
        );
        assert_eq!(status, 500, "{body}");
    }

    #[test]
    fn turning_resume_off_deletes_what_was_kept() {
        let (s, base, _d) = server();
        let body = r#"{"v":"dQw4w9WgXcQ","pos":100,"dur":400,"title":"T"}"#;
        http(&base, "POST", &path(&base, "api/resume"), body);
        assert!(s.paths.resume().exists());
        let (_, cfg) = http(&base, "GET", &path(&base, "api/config"), "");
        assert_eq!(
            serde_json::from_str::<Value>(&cfg).unwrap()["resumeDays"],
            7
        );
        let (status, cfg) = http(
            &base,
            "POST",
            &path(&base, "api/config"),
            r#"{"resumeDays":0}"#,
        );
        assert_eq!(status, 200, "{cfg}");
        assert_eq!(
            serde_json::from_str::<Value>(&cfg).unwrap()["resumeDays"],
            0
        );
        assert!(!s.paths.resume().exists());
        assert_eq!(
            http(&base, "POST", &path(&base, "api/resume"), body).1,
            r#"{"noted":"off"}"#
        );
        let (_, cfg) = http(
            &base,
            "POST",
            &path(&base, "api/config"),
            r#"{"resumeDays":9999}"#,
        );
        assert_eq!(
            serde_json::from_str::<Value>(&cfg).unwrap()["resumeDays"],
            365
        );
        http(&base, "POST", &path(&base, "api/resume"), body);
        assert_eq!(
            http(&base, "POST", &path(&base, "api/resume/clear"), "").0,
            200
        );
        assert!(!s.paths.resume().exists());
    }

    #[test]
    fn search_results_are_capped() {
        let bin = TempDir::new();
        let (_s, base, _d) = server_with(Config {
            ytdlp: crate::store::testing::script(
                &bin.0.join("yt-dlp"),
                // The target is the last thing in the config file.
                r#"while [ $# -gt 0 ]; do [ "$1" = --config-locations ] && tail -n 1 "$2" > "$(dirname "$0")/asked"; shift; done
echo '{"entries":[]}'"#,
            ),
            ..Config::default()
        });
        for (n, asked) in [("1000", "60"), ("0", "1"), ("x", "40"), ("25", "25")] {
            let (status, body) = http(
                &base,
                "GET",
                &path(&base, &format!("api/search?q=a&n={n}")),
                "",
            );
            assert_eq!(status, 200, "{body}");
            let target = std::fs::read_to_string(bin.0.join("asked")).unwrap();
            assert_eq!(target.trim(), format!("'ytsearch{asked}:a'"), "n={n}");
        }
    }

    /// A yt-dlp that prints search results one per line, the way
    /// `--lazy-playlist -j` does: two, a pause, then a third. `tail` is shell
    /// run after them.
    fn streaming_ytdlp(dir: &std::path::Path, tail: &str) -> String {
        let line =
            |id: &str| format!("echo '{{\"id\":\"{id}\",\"title\":\"t\",\"channel\":\"c\"}}'");
        crate::store::testing::script(
            &dir.join("yt-dlp"),
            &format!(
                // Carries on when its output is closed, like a program that
                // doesn't die of SIGPIPE: only being killed stops it.
                "trap '' PIPE\necho \"$*\" > \"$(dirname \"$0\")/args\"\n{}\n{}\nsleep 1\n{}\n{tail}\n",
                line("aaaaaaaaaaa"),
                line("bbbbbbbbbbb"),
                line("ccccccccccc")
            ),
        )
    }

    #[test]
    fn a_streamed_search_sends_each_result_as_it_comes() {
        let bin = TempDir::new();
        let (_s, base, _d) = server_with(Config {
            ytdlp: streaming_ytdlp(&bin.0, ""),
            ..Config::default()
        });
        let mut c = TcpStream::connect(addr(&base)).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let target = path(&base, "api/search?q=rust&stream=1");
        write!(c, "GET {target} HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let started = Instant::now();
        let mut r = BufReader::new(c);
        let mut head = String::new();
        while !head.ends_with("\r\n\r\n") {
            assert!(r.read_line(&mut head).unwrap() > 0);
        }
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        assert!(head.contains("application/x-ndjson"), "{head}");
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        r.read_line(&mut line).unwrap();
        // The first two arrive before yt-dlp's pause is over.
        assert!(
            started.elapsed() < Duration::from_millis(900),
            "{:?}",
            started.elapsed()
        );
        let mut rest = String::new();
        r.read_to_string(&mut rest).unwrap();
        let ids: Vec<String> = line
            .lines()
            .chain(rest.lines())
            .map(|l| {
                serde_json::from_str::<Value>(l).unwrap()["id"]
                    .as_str()
                    .unwrap()
                    .into()
            })
            .collect();
        assert_eq!(ids, ["aaaaaaaaaaa", "bbbbbbbbbbb", "ccccccccccc"]);
        let args = std::fs::read_to_string(bin.0.join("args")).unwrap();
        assert!(args.contains("--lazy-playlist"), "{args}");
    }

    #[test]
    fn a_streamed_search_that_fails_says_so() {
        let bin = TempDir::new();
        // Fails after its results: the stream ends with the error.
        let (_s, base, _d) = server_with(Config {
            ytdlp: streaming_ytdlp(&bin.0, "echo 'ERROR: the network went away' >&2; exit 1"),
            ..Config::default()
        });
        let (status, body) = http(&base, "GET", &path(&base, "api/search?q=a&stream=1"), "");
        assert_eq!(status, 200);
        let last: Value = serde_json::from_str(body.lines().last().unwrap()).unwrap();
        assert!(
            last["error"]
                .as_str()
                .unwrap()
                .contains("network went away"),
            "{body}"
        );
        assert_eq!(last["code"], "other");
        assert_eq!(body.lines().count(), 4, "{body}");

        // Fails before any: an ordinary error.
        let bin = TempDir::new();
        let (_s, base, _d) = server_with(Config {
            ytdlp: crate::store::testing::script(
                &bin.0.join("yt-dlp"),
                "echo 'ERROR: nope' >&2; exit 1",
            ),
            ..Config::default()
        });
        let (status, body) = http(&base, "GET", &path(&base, "api/search?q=a&stream=1"), "");
        assert_eq!(status, 500);
        assert!(body.contains("nope"), "{body}");
    }

    #[test]
    fn a_streamed_search_stops_yt_dlp_when_the_window_goes_away() {
        let bin = TempDir::new();
        let done = bin.0.join("finished");
        let (_s, base, _d) = server_with(Config {
            ytdlp: streaming_ytdlp(
                &bin.0,
                &format!(
                    "for i in 1 2 3 4 5; do echo '{{\"id\":\"ddddddddddd\",\"title\":\"t\"}}'; sleep 0.1; done; sleep 2; touch '{}'",
                    done.display()
                ),
            ),
            ..Config::default()
        });
        let mut c = TcpStream::connect(addr(&base)).unwrap();
        let target = path(&base, "api/search?q=rust&stream=1");
        write!(c, "GET {target} HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let mut first = [0u8; 64];
        let _ = c.read(&mut first).unwrap();
        drop(c);
        // The later results find nobody to read them, and yt-dlp is stopped
        // instead of carrying on.
        std::thread::sleep(Duration::from_millis(4500));
        assert!(!done.exists(), "yt-dlp ran to the end");
    }

    #[test]
    fn a_route_switch_stops_work_that_started_on_the_old_route() {
        // Each of these is a request that waits on yt-dlp.
        let requests = [
            ("GET", "api/search?q=a"),
            ("GET", "api/search?q=a&stream=1"),
            ("GET", "api/channel?id=UC0000000000000000000001"),
            ("POST", "api/subs/add?c=@somebody"),
            ("POST", "api/play?v=dQw4w9WgXcQ"),
        ];
        for (method, target) in requests {
            let bin = TempDir::new();
            let (_s, base, _d) = server_with(Config {
                ytdlp: hanging_ytdlp(&bin.0),
                ..Config::default()
            });
            let call = {
                let base = base.clone();
                std::thread::spawn(move || http(&base, method, &path(&base, target), ""))
            };
            let (pid, private) = hanging_running(&bin.0);
            assert!(private.is_dir());

            let switched = Instant::now();
            switch_to_tor(&base);
            let (status, body) = call.join().unwrap();
            // It doesn't wait for the 60 s that yt-dlp would have been given.
            assert!(
                switched.elapsed() < Duration::from_secs(5),
                "{target}: {:?}",
                switched.elapsed()
            );
            assert_eq!(status, 500, "{target}: {body}");
            let said: Value = serde_json::from_str(body.lines().last().unwrap()).unwrap();
            assert_eq!(said["error"], "The route changed; try again", "{target}");
            assert_eq!(said["code"], "route_changed", "{target}");
            assert_stopped_and_cleaned_up(&pid, &private);
        }
    }

    #[test]
    fn a_settings_change_that_keeps_the_route_leaves_running_work_alone_but_registered() {
        let bin = TempDir::new();
        let (_s, base, _d) = server_with(Config {
            ytdlp: hanging_ytdlp(&bin.0),
            ..Config::default()
        });
        let search = {
            let base = base.clone();
            std::thread::spawn(move || http(&base, "GET", &path(&base, "api/search?q=a"), ""))
        };
        let (pid, private) = hanging_running(&bin.0);
        let (status, body) = http(
            &base,
            "POST",
            &path(&base, "api/config"),
            r#"{"history":true,"maxHeight":720}"#,
        );
        assert_eq!(status, 200, "{body}");
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            std::path::Path::new("/proc").join(&pid).exists(),
            "a setting that isn't the route must not stop a search"
        );
        // The new runtime still knows about it: leaving the route stops it.
        switch_to_tor(&base);
        let (status, body) = search.join().unwrap();
        assert_eq!(status, 500, "{body}");
        assert!(body.contains("The route changed; try again"), "{body}");
        assert_stopped_and_cleaned_up(&pid, &private);
    }

    #[test]
    fn the_proxy_is_shown_without_credentials_and_kept_when_sent_back() {
        let (s, base, _d) = server();
        let post = |body: &str| http(&base, "POST", &path(&base, "api/config"), body);
        let (status, body) =
            post(r#"{"route":"proxy","proxy":"http://user:secret@127.0.0.1:3128"}"#);
        assert_eq!(status, 200, "{body}");
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["proxy"], "http://***@127.0.0.1:3128");
        assert!(!body.contains("secret"), "{body}");
        let (_, shown) = http(&base, "GET", &path(&base, "api/config"), "");
        assert!(!shown.contains("secret"), "{shown}");
        let saved = |s: &Server| Config::load(&s.paths.config).unwrap().proxy;
        assert_eq!(
            saved(&s).as_deref(),
            Some("http://user:secret@127.0.0.1:3128")
        );

        // The window sends back what it was shown: that keeps the real proxy.
        let (status, _) = post(r#"{"route":"proxy","proxy":"http://***@127.0.0.1:3128"}"#);
        assert_eq!(status, 200);
        assert_eq!(
            saved(&s).as_deref(),
            Some("http://user:secret@127.0.0.1:3128")
        );
        assert_eq!(s.runtime().net.proxy(), saved(&s).as_deref());

        // Anything else replaces it.
        post(r#"{"route":"proxy","proxy":"http://10.0.0.1:8080"}"#);
        assert_eq!(saved(&s).as_deref(), Some("http://10.0.0.1:8080"));
    }

    use crate::net::TOR_PROXY;
    use crate::store::testing::script;

    fn json_of(body: &str) -> Value {
        serde_json::from_str(body.lines().last().unwrap())
            .unwrap_or_else(|e| panic!("{body:?}: {e}"))
    }

    #[test]
    fn every_kind_of_failure_comes_back_with_its_code() {
        let refused = "ERROR: [youtube] x: Unable to download API page: Failed to establish a new connection: [Errno 111] Connection refused";
        let cases: &[(Option<&str>, &str, &str)] = &[
            (
                None,
                "ERROR: [youtube] x: Sign in to confirm you're not a bot. Use --cookies",
                "bot_check",
            ),
            (
                None,
                "ERROR: [youtube] x: Sign in to confirm your age. This video may be inappropriate for some users.",
                "age_restricted",
            ),
            (
                None,
                "ERROR: [youtube] x: Private video. Sign in if you've been granted access to this video",
                "private",
            ),
            (
                None,
                "ERROR: [youtube] x: Unable to download webpage: HTTP Error 429: Too Many Requests",
                "rate_limited",
            ),
            (
                None,
                "ERROR: [youtube] x: nsig extraction failed: Some formats may be missing",
                "ytdlp_outdated",
            ),
            (
                None,
                "WARNING: [youtube] No supported JavaScript runtime could be found.",
                "deno_missing",
            ),
            (Some(TOR_PROXY), refused, "tor_down"),
            (Some("socks5h://10.0.0.2:1080"), refused, "proxy_refused"),
            (None, "ERROR: something nobody foresaw", "other"),
        ];
        for (proxy, said, code) in cases {
            let bin = TempDir::new();
            let (_s, base, _d) = server_with(Config {
                proxy: proxy.map(String::from),
                ytdlp: script(
                    &bin.0.join("yt-dlp"),
                    &format!("echo \"{said}\" >&2\nexit 1\n"),
                ),
                ..Config::default()
            });
            // Whichever endpoint it comes out of.
            for (method, target) in [
                ("GET", "api/search?q=a"),
                ("GET", "api/search?q=a&stream=1"),
                ("GET", "api/channel?id=UC0000000000000000000001"),
                ("POST", "api/subs/add?c=@somebody"),
                ("POST", "api/play?v=dQw4w9WgXcQ"),
            ] {
                let (status, body) = http(&base, method, &path(&base, target), "");
                assert_eq!(status, 500, "{target}: {body}");
                let v = json_of(&body);
                assert_eq!(v["code"], *code, "{said} / {target}: {body}");
                let text = v["error"].as_str().unwrap();
                assert!(!text.is_empty() && !text.contains("ERROR:"), "{text}");
                let mut keys: Vec<_> = v.as_object().unwrap().keys().collect();
                keys.sort();
                assert_eq!(keys, ["code", "error"]);
            }
        }
    }

    #[test]
    fn a_live_stream_is_refused_with_its_code() {
        let bin = TempDir::new();
        let (_s, base, _d) = server_with(Config {
            ytdlp: script(
                &bin.0.join("yt-dlp"),
                r#"echo '{"is_live":true,"url":"https://gv.test/x","protocol":"https"}'"#,
            ),
            ..Config::default()
        });
        let (status, body) = http(&base, "POST", &path(&base, "api/play?v=dQw4w9WgXcQ"), "");
        assert_eq!(status, 500);
        assert_eq!(json_of(&body)["code"], "live", "{body}");
    }

    #[test]
    fn what_is_not_a_failure_of_yt_dlp_has_a_code_too() {
        let (_s, base, _d) = server();
        let (status, body) = http(&base, "GET", &path(&base, "api/nowhere"), "");
        assert_eq!(status, 404);
        assert_eq!(
            json_of(&body),
            json!({ "error": "not found", "code": "other" })
        );
        for (method, target) in [
            ("POST", "api/play?v=https://evil.example/x"),
            ("GET", "api/search"),
            ("POST", "api/subs/rm?id=nobody"),
            ("POST", "api/config"),
        ] {
            let (status, body) = http(&base, method, &path(&base, target), "");
            assert_eq!(status, 500, "{target}");
            assert_eq!(json_of(&body)["code"], "other", "{target}: {body}");
        }
    }

    #[test]
    fn the_config_carries_what_to_say_about_privacy_for_the_route_in_use() {
        let (_s, base, _d) = server();
        let get = || {
            let (_, body) = http(&base, "GET", &path(&base, "api/config"), "");
            serde_json::from_str::<Value>(&body).unwrap()["privacy"].clone()
        };
        let set = |body: &str| {
            let (status, body) = http(&base, "POST", &path(&base, "api/config"), body);
            assert_eq!(status, 200, "{body}");
            serde_json::from_str::<Value>(&body).unwrap()["privacy"].clone()
        };
        let direct = get();
        let mut keys: Vec<_> = direct.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, ["pages", "points", "routes", "summary"]);
        assert!(direct["summary"].as_str().unwrap().starts_with("Direct"));
        let points = |p: &Value| -> Vec<String> {
            p["points"]
                .as_array()
                .unwrap()
                .iter()
                .map(|s| s.as_str().unwrap().to_string())
                .collect()
        };
        assert!(
            points(&direct)
                .iter()
                .all(|p| !p.contains("fresh SOCKS credentials"))
        );

        // What it says follows the route, and is in the answer to a change.
        let tor = set(r#"{"route":"tor"}"#);
        assert!(tor["summary"].as_str().unwrap().contains("Tor"));
        assert!(points(&tor).iter().any(|p| {
            p.contains("fresh SOCKS credentials per request (a separate circuit if this is Tor)")
        }));
        let shared = set(r#"{"route":"proxy","proxy":"socks5h://me:secret@127.0.0.1:9050"}"#);
        assert!(shared["summary"].as_str().unwrap().contains("not isolated"));
        assert!(!shared.to_string().contains("secret"));
        assert_eq!(get(), shared);
        // The same lines whatever the route, for the popup's options and the pages.
        assert_eq!(direct["routes"], tor["routes"]);
        assert_eq!(direct["pages"], shared["pages"]);
        assert!(
            direct["routes"]["direct"].is_string() && direct["pages"]["history_on"].is_string()
        );
    }

    #[test]
    fn a_search_marks_the_streams_that_are_live_or_coming() {
        let bin = TempDir::new();
        let (_s, base, _d) = server_with(Config {
            ytdlp: script(
                &bin.0.join("yt-dlp"),
                r#"echo '{"entries":[
                    {"id":"aaaaaaaaaaa","title":"now","live_status":"is_live"},
                    {"id":"bbbbbbbbbbb","title":"later","live_status":"is_upcoming"},
                    {"id":"ccccccccccc","title":"over","live_status":"was_live"},
                    {"id":"ddddddddddd","title":"plain"}]}'"#,
            ),
            ..Config::default()
        });
        let (status, body) = http(&base, "GET", &path(&base, "api/search?q=a"), "");
        assert_eq!(status, 200, "{body}");
        let v: Value = serde_json::from_str(&body).unwrap();
        let live: Vec<_> = v
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e.get("live").cloned())
            .collect();
        // Only there when true.
        assert_eq!(live, [Some(json!(true)), Some(json!(true)), None, None]);
    }

    #[test]
    fn giving_up_on_a_play_request_stops_yt_dlp_and_adds_nothing_to_the_history() {
        let bin = TempDir::new();
        let (s, base, _d) = server_with(Config {
            ytdlp: hanging_ytdlp(&bin.0),
            history: true,
            ..Config::default()
        });
        let mut c = TcpStream::connect(addr(&base)).unwrap();
        write!(
            c,
            "POST {} HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\n\r\n",
            path(&base, "api/play?v=dQw4w9WgXcQ")
        )
        .unwrap();
        let (pid, private) = hanging_running(&bin.0);
        let gave_up = Instant::now();
        drop(c);
        // yt-dlp would have been given 90 s; it is gone within moments, with
        // the private files it was given.
        assert_stopped_and_cleaned_up(&pid, &private);
        assert!(
            gave_up.elapsed() < Duration::from_secs(3),
            "{:?}",
            gave_up.elapsed()
        );
        wait_for("the registry to be empty", Duration::from_secs(5), || {
            s.runtime().yt.calls.running() == 0
        });
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            !s.paths.history().exists(),
            "a video that was never answered"
        );
        assert!(lock(&s.sessions).is_empty());
    }

    /// Reads one reply the way a client does: by its Content-Length, not by
    /// waiting for the connection to close.
    fn read_reply(c: &mut TcpStream) -> (u16, String) {
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            let mut b = [0u8; 1];
            assert_eq!(c.read(&mut b).unwrap(), 1, "the head ended early");
            head.push(b[0]);
        }
        let head = String::from_utf8(head).unwrap();
        let len: usize = head
            .lines()
            .find_map(|l| {
                l.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(|v| v.trim().parse().unwrap())
            })
            .unwrap();
        let mut body = vec![0u8; len];
        c.read_exact(&mut body).unwrap();
        (
            head[9..12].parse().unwrap(),
            String::from_utf8(body).unwrap(),
        )
    }

    #[test]
    fn a_video_is_added_to_the_history_only_after_its_answer_has_been_written() {
        let up = Upstream::start();
        let bin = TempDir::new();
        let (s, base, _d) = server_with(Config {
            ytdlp: fake_ytdlp(&bin.0, &up.url("/video?gen=")),
            history: true,
            ..Config::default()
        });
        // The history file is a pipe that nobody has opened for writing yet,
        // so reading it blocks: recording the video can't finish until the
        // test lets it.
        std::fs::create_dir_all(&s.paths.data).unwrap();
        let fifo = std::ffi::CString::new(s.paths.history().to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);

        let mut c = TcpStream::connect(addr(&base)).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        write!(
            c,
            "POST {} HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\n\r\n",
            path(&base, "api/play?v=dQw4w9WgXcQ")
        )
        .unwrap();
        // The answer arrives in full, with the history still stuck.
        let (status, body) = read_reply(&mut c);
        assert_eq!(status, 200, "{body}");
        let info: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(info["id"], "dQw4w9WgXcQ");
        assert!(
            info["video"]
                .as_str()
                .unwrap()
                .starts_with("http://127.0.0.1:")
        );
        assert!(
            std::fs::metadata(s.paths.history())
                .unwrap()
                .file_type()
                .is_fifo_like()
        );

        // Let the recording go on: an empty history to add to. It replaces
        // the pipe with a file when it is done, and only then is it safe to
        // read (reading the pipe would wait for a writer).
        std::fs::write(s.paths.history(), r#"{"watched":[]}"#).unwrap();
        wait_for("the history to be written", Duration::from_secs(10), || {
            std::fs::symlink_metadata(s.paths.history()).is_ok_and(|m| m.is_file())
        });
        assert_eq!(app::load_history(&s.paths).unwrap().watched.len(), 1);
    }

    trait FifoLike {
        fn is_fifo_like(&self) -> bool;
    }

    impl FifoLike for std::fs::FileType {
        fn is_fifo_like(&self) -> bool {
            std::os::unix::fs::FileTypeExt::is_fifo(self)
        }
    }
}
