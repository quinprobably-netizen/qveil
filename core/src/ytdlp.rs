//! yt-dlp does the work that breaks every few weeks (InnerTube parsing,
//! signature/n-param deciphering, PO tokens), so qveil follows its fixes for free.
//! Every invocation is sandboxed from the user's own yt-dlp setup.
//!
//! What it is asked for (a search, the video being watched) and the proxy's
//! password would be readable by every local user in /proc/*/cmdline, so they
//! never go on the command line: see `Call`.

use crate::comments::{self, Comments, Sort};
use crate::errors::friendly;
use crate::ids;
use crate::inflight::{Inflight, Run, StartError};
use crate::net::{Net, USER_AGENT};
use crate::private::PrivateDir;
use crate::video::{Chapter, Video};
use anyhow::{Context, Result, anyhow, bail, ensure};
use serde_json::Value;
use std::fmt;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How long a search or a channel listing may take before yt-dlp is killed.
const QUICK: Duration = Duration::from_secs(60);
/// Resolving a video does more (player requests, the JavaScript challenge), so it gets longer.
const RESOLVE: Duration = Duration::from_secs(90);
/// Comments come a page at a time, and replies a thread at a time, so a long
/// list over a slow route takes a while.
const COMMENTS: Duration = Duration::from_secs(120);
/// How many replies are asked for under each comment, when they are.
const REPLIES_EACH: usize = 3;
/// yt-dlp's own limit for one connection. The deadlines above bound the whole
/// call, and this keeps a single dead connection from using up all of it.
const SOCKET_TIMEOUT: &str = "20";

/// Why a live stream is refused: the relay can only carry a plain download.
const LIVE_UNSUPPORTED: &str =
    "Live streams aren't supported yet: they can't be relayed through your route.";
const MANIFEST_ONLY: &str = "This video is only offered as an HLS/DASH stream, which can't be relayed through your route yet.";

/// What a lookup that its caller gave up on (the window hung up) ends with.
/// Nobody is left to read it; it is only the end of the call.
pub const CANCELLED: &str = "The request was cancelled";

/// The format selector only takes plain downloads, so the HLS manifest that
/// yt-dlp fetches for every video (a quarter of a second of a 1.3 s resolve,
/// measured) is never used. Skipped when resolving; `explain_no_format` still
/// fetches it, to tell a live stream from a video that has no plain download.
const SKIP_MANIFESTS: &str = "youtube:skip=hls,dash";

/// yt-dlp exited with an error. `raw` is its last stderr line; the message
/// shown is the readable version of it.
#[derive(Debug)]
struct Failed {
    raw: String,
}

impl fmt::Display for Failed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&friendly(&format!("yt-dlp: {}", self.raw)))
    }
}

impl std::error::Error for Failed {}

pub struct Ytdlp {
    pub bin: String,
    pub cache_dir: PathBuf,
    pub net: Net,
    /// Every call is registered here, so that a route change or the engine's
    /// exit can stop it (see `Inflight`).
    pub calls: Arc<Inflight>,
}

#[derive(Debug)]
pub struct Streams {
    pub title: String,
    pub channel: Option<String>,
    pub channel_id: Option<String>,
    pub duration: Option<u64>,
    pub video: String,
    pub audio: Option<String>,
    pub headers: Vec<(String, String)>,
    /// What the page says about the video. All of it comes with the lookup
    /// that finds the streams, so showing it costs no request.
    pub description: Option<String>,
    pub views: Option<u64>,
    pub likes: Option<u64>,
    /// Unix seconds.
    pub published: Option<i64>,
    pub chapters: Vec<Chapter>,
}

/// How much of a description is kept, in characters. YouTube allows 5000;
/// this only bounds what a page that isn't YouTube's could hand over.
const DESCRIPTION_MAX: usize = 20_000;
/// How many chapters are kept.
const CHAPTERS_MAX: usize = 200;

impl Ytdlp {
    /// The options every call shares. None of them is private, and none is
    /// what makes a call different: that is in the call's config file.
    fn command(&self, call: &Call) -> Command {
        let mut c = Command::new(&self.bin);
        c.args([
            "--ignore-config",  // a user config could add cookies or a browser profile
            "--no-plugin-dirs", // a plugin is code with its own network access, around the route
            "--no-cookies",
            "--no-cookies-from-browser",
            "--no-mark-watched",
            "--no-warnings",
            "--socket-timeout",
            SOCKET_TIMEOUT,
            // yt-dlp picks a random Chrome version for each call, which would
            // put a second browser identity beside the engine's own on the
            // same circuit. With qveil's, the video bytes (whose headers
            // yt-dlp resolves) and the engine's requests agree.
            "--user-agent",
            USER_AGENT,
        ]);
        c.arg("--cache-dir").arg(&self.cache_dir);
        // Still honoured after `--ignore-config`, which only skips the
        // default locations.
        c.arg("--config-locations").arg(&call.config);
        c.stdin(Stdio::null());
        c
    }

    /// Starts `cmd` through the registry, which refuses once the route it
    /// belongs to has been left.
    fn start(&self, cmd: &mut Command, call: &Call) -> Result<Run> {
        self.calls.start(cmd, call.dir.path()).map_err(|e| match e {
            StartError::Closed(why) => anyhow!(why.message()),
            StartError::Spawn(e) => anyhow::Error::new(e)
                .context(format!("running {:?} (is yt-dlp installed?)", self.bin)),
        })
    }

    /// Like `Command::output`, but kills the child once `limit` has passed and
    /// returns `None`. The output is read on separate threads, so a child that
    /// fills a pipe isn't left waiting for us. An error if the registry
    /// stopped it, because what it printed belongs to a route that was left.
    /// `abort` is asked every few milliseconds while the child runs, and when
    /// it says yes the child is killed (with everything it started) and the
    /// call ends with `CANCELLED`.
    fn output_within(
        &self,
        cmd: &mut Command,
        call: &Call,
        limit: Duration,
        abort: &dyn Fn() -> bool,
    ) -> Result<Option<Output>> {
        let mut run = self.start(cmd, call)?;
        let stdout = drain(run.stdout.take());
        let stderr = drain(run.stderr.take());
        let deadline = Instant::now() + limit;
        let status = loop {
            if let Some(status) = run.try_wait()? {
                break status;
            }
            let cancelled = abort();
            if cancelled || Instant::now() >= deadline {
                run.kill();
                let _ = run.wait();
                // The readers end on their own now that the pipes are closed.
                return match run.stopped() {
                    Some(why) => Err(anyhow!(why.message())),
                    None if cancelled => Err(anyhow!(CANCELLED)),
                    None => Ok(None),
                };
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        if let Some(why) = run.stopped() {
            bail!(why.message());
        }
        Ok(Some(Output {
            status,
            stdout: stdout.join().unwrap_or_default(),
            stderr: stderr.join().unwrap_or_default(),
        }))
    }

    /// Runs yt-dlp on `target` (a URL or a `ytsearchN:` query) with `args`
    /// and parses its JSON. It is killed if it takes longer than `limit`.
    fn json(&self, args: &[&str], target: &str, limit: Duration) -> Result<Value> {
        self.json_with(args, None, target, limit, &|| false)
    }

    /// Like `json`, with `extractor_args` for yt-dlp's `--extractor-args`.
    /// They go in the call's config file, not on the command line, because
    /// they name the site the call is for.
    fn json_with(
        &self,
        args: &[&str],
        extractor_args: Option<&str>,
        target: &str,
        limit: Duration,
        abort: &dyn Fn() -> bool,
    ) -> Result<Value> {
        // yt-dlp would create it with the default mode, and every directory
        // above it that doesn't exist yet along with it.
        crate::store::ensure_dir(&self.cache_dir)?;
        let call = Call::new(&self.net, extractor_args, target)?;
        let out = self.output_within(self.command(&call).args(args), &call, limit, abort)?;
        // The files are only needed while it runs.
        drop(call);
        let Some(out) = out else {
            bail!(
                "yt-dlp didn't finish within {} s and was stopped; the route may be slow or down",
                limit.as_secs()
            );
        };
        if !out.status.success() {
            return Err(self.failure(&out.stderr));
        }
        serde_json::from_slice(&out.stdout).context("yt-dlp returned invalid JSON")
    }

    /// What a failed run says: its last line on stderr.
    fn failure(&self, stderr: &[u8]) -> anyhow::Error {
        let err = String::from_utf8_lossy(stderr);
        let line = err
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("unknown error")
            .trim();
        Failed {
            raw: self
                .net
                .explain(line.strip_prefix("ERROR: ").unwrap_or(line)),
        }
        .into()
    }

    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<Video>> {
        let q = format!("ytsearch{limit}:{query}");
        // `approximate_date` turns YouTube's "3 days ago" into a timestamp, so
        // search results can show an age like the feed does.
        let v = self.json(
            &[
                "--flat-playlist",
                "--extractor-args",
                "youtubetab:approximate_date",
                "-J",
            ],
            &q,
            QUICK,
        )?;
        Ok(entries(&v))
    }

    /// The same search, handing each result to `each` as soon as yt-dlp has
    /// it: YouTube answers a search 20 results at a time, and the first
    /// page needn't wait for the second. The requests are the same as
    /// `search`'s. `each` returns false to stop (the window went away), and
    /// yt-dlp is then killed. Returns how many results there were.
    pub fn search_each(
        &self,
        query: &str,
        limit: usize,
        each: &mut dyn FnMut(Video) -> bool,
    ) -> Result<usize> {
        let q = format!("ytsearch{limit}:{query}");
        crate::store::ensure_dir(&self.cache_dir)?;
        let call = Call::new(&self.net, None, &q)?;
        let mut run = self.start(
            self.command(&call).args([
                "--flat-playlist",
                "--lazy-playlist",
                "--extractor-args",
                "youtubetab:approximate_date",
                "-j",
            ]),
            &call,
        )?;
        let stdout = run.stdout.take().context("no stdout")?;
        let stderr = drain(run.stderr.take());
        // Kills yt-dlp at the deadline, or as soon as `done` is sent.
        let (done, finished) = std::sync::mpsc::channel::<()>();
        let watchdog = {
            let proc = run.handle();
            std::thread::spawn(move || {
                let late = finished.recv_timeout(QUICK).is_err();
                proc.kill();
                late
            })
        };
        let (mut n, mut stopped) = (0, false);
        for line in std::io::BufRead::lines(std::io::BufReader::new(stdout)) {
            let Ok(line) = line else { break };
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if let Some(video) = entry(&v, &None, &None) {
                n += 1;
                if !each(video) {
                    stopped = true;
                    break;
                }
            }
        }
        // Whether it finished, was stopped or ran out of time, it ends here.
        if stopped {
            run.kill();
        }
        let status = run.wait()?;
        let _ = done.send(());
        let late = watchdog.join().unwrap_or(false) && !status.success();
        drop(call);
        let stderr = stderr.join().unwrap_or_default();
        if let Some(why) = run.stopped() {
            bail!(why.message());
        }
        if stopped || status.success() {
            return Ok(n);
        }
        if late {
            bail!(
                "yt-dlp didn't finish within {} s and was stopped; the route may be slow or down",
                QUICK.as_secs()
            );
        }
        Err(self.failure(&stderr))
    }

    /// Returns (channel id, display name).
    pub fn channel_info(&self, url: &str) -> Result<(String, String)> {
        let v = self.json(&["--flat-playlist", "-J", "-I", "0"], url, QUICK)?;
        let id = str_field(&v, &["channel_id", "id"])
            .filter(|id| ids::is_channel_id(id))
            .with_context(|| format!("{url} doesn't look like a channel"))?;
        let name = str_field(&v, &["channel", "uploader", "title"]).unwrap_or_else(|| id.clone());
        Ok((id, name))
    }

    /// Latest uploads from the channel's Videos tab. Used as a fallback when
    /// the RSS feed is down. `published` is only approximate (day-level,
    /// parsed from "3 days ago").
    pub fn channel_videos(&self, channel_id: &str, limit: usize) -> Result<Vec<Video>> {
        let url = format!("https://www.youtube.com/channel/{channel_id}/videos");
        let range = format!("1:{limit}");
        let v = self.json(
            &[
                "--flat-playlist",
                "--extractor-args",
                "youtubetab:approximate_date",
                "-J",
                "-I",
                &range,
            ],
            &url,
            QUICK,
        )?;
        Ok(entries(&v))
    }

    /// The first `n` comment threads of a video, in `sort` order, and with
    /// `replies` up to `REPLIES_EACH` replies under each. yt-dlp has no way
    /// to ask for the next page, so more is asked for from the start.
    /// Nothing is downloaded, and a video without formats (a stream that
    /// hasn't started) still has comments.
    pub fn comments(
        &self,
        url: &str,
        sort: Sort,
        n: usize,
        replies: bool,
        abort: &dyn Fn() -> bool,
    ) -> Result<Comments> {
        // max_comments is: comments in all, threads, replies in all, replies per thread.
        let limits = if replies {
            format!(
                "{},{n},{},{REPLIES_EACH}",
                n * (REPLIES_EACH + 1),
                n * REPLIES_EACH
            )
        } else {
            format!("{n},{n},0,0")
        };
        let extractor = format!(
            "{SKIP_MANIFESTS};comment_sort={};max_comments={limits}",
            sort.as_str()
        );
        let args = [
            "-J",
            "--no-playlist",
            "--skip-download",
            "--ignore-no-formats-error",
            "--write-comments",
        ];
        let v = self.json_with(&args, Some(&extractor), url, COMMENTS, abort)?;
        Ok(comments::parse(&v, sort))
    }

    /// Picks the streams to relay. `format` must only select plain downloads
    /// (see `Config::format`), and the result is checked again here because a
    /// manifest slipping through would make mpv fetch segments directly,
    /// around the route.
    pub fn resolve(&self, url: &str, format: &str) -> Result<Streams> {
        self.resolve_until(url, format, &|| false)
    }

    /// `resolve`, which ends (and kills yt-dlp) once `abort` says that nobody
    /// is waiting for the result any more: the window hung up on the request.
    pub fn resolve_until(
        &self,
        url: &str,
        format: &str,
        abort: &dyn Fn() -> bool,
    ) -> Result<Streams> {
        let args = ["-J", "--no-playlist", "-f", format];
        match self.json_with(&args, Some(SKIP_MANIFESTS), url, RESOLVE, abort) {
            Ok(v) => streams_from(&v, url),
            Err(e) if is_no_format(&e) => Err(self.explain_no_format(url, e, abort)),
            Err(e) => Err(e),
        }
    }

    /// The format selector only accepts plain downloads, so a live stream
    /// (HLS only) makes yt-dlp fail with a bare "no format". Ask again with
    /// its default selector, which does return something, to say why. This
    /// costs a second request, but only on a path that was failing anyway.
    fn explain_no_format(
        &self,
        url: &str,
        original: anyhow::Error,
        abort: &dyn Fn() -> bool,
    ) -> anyhow::Error {
        match self.json_with(&["-J", "--no-playlist"], None, url, RESOLVE, abort) {
            Ok(v) if is_live_or_upcoming(&v) => anyhow!(LIVE_UNSUPPORTED),
            Ok(v) if !chosen_formats(&v).iter().all(|f| is_plain(f)) => anyhow!(MANIFEST_ONLY),
            Err(e) if e.to_string() == CANCELLED => e,
            _ => original,
        }
    }
}

/// The private files of one yt-dlp call, removed when it is dropped.
///
/// yt-dlp reads one config file, and the proxy and the target go in it: one
/// option per line, and a bare word is a URL to fetch. It is split the way a
/// shell would split it, which is why every value is quoted. (A batch file
/// would do for the target, but it treats ` #` as the start of a comment, and
/// searches like "rust #shorts" are ordinary.)
struct Call {
    dir: PrivateDir,
    config: PathBuf,
}

impl Call {
    fn new(net: &Net, extractor_args: Option<&str>, target: &str) -> Result<Self> {
        // A target that began with `-` would be read as an option. Ours are
        // always `ytsearch…` or an https URL, so this can't happen.
        ensure!(
            !target.starts_with('-'),
            "refusing to pass {target:?} to yt-dlp"
        );
        // An explicit empty proxy is what makes Direct direct: left unset,
        // yt-dlp would use HTTP_PROXY or ALL_PROXY from the environment.
        let proxy = net.proxy_url().unwrap_or_default();
        // The file is read as text, so a control character (a newline in a
        // search, say) has no business in it.
        let target: String = target
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        // The byte order mark makes yt-dlp read the file as UTF-8 whatever
        // the locale is, so a search in any script arrives intact.
        let mut text = format!("\u{feff}--proxy {}\n", quote(&proxy));
        if let Some(e) = extractor_args {
            text.push_str(&format!("--extractor-args {}\n", quote(e)));
        }
        text.push_str(&format!("{}\n", quote(&target)));
        let dir = PrivateDir::new("qveil-ytdlp-")?;
        let config = dir.write("yt-dlp.conf", text.as_bytes())?;
        Ok(Call { dir, config })
    }
}

/// Shell-style single quotes: nothing inside is special, so `#`, spaces and
/// backslashes are kept, and a quote is closed, escaped and reopened.
fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

fn drain(pipe: Option<impl Read + Send + 'static>) -> JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut p) = pipe {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    })
}

fn is_no_format(e: &anyhow::Error) -> bool {
    e.downcast_ref::<Failed>().is_some_and(|f| {
        // With the manifests skipped, a live stream has no formats at all,
        // and yt-dlp says so in these words instead.
        f.raw.contains("Requested format is not available")
            || f.raw.contains("live stream recording is not available")
    })
}

fn is_live(v: &Value) -> bool {
    v["is_live"].as_bool() == Some(true) || v["live_status"].as_str() == Some("is_live")
}

/// A stream that is running or has been announced. A finished one (`was_live`,
/// `post_live`) is an ordinary video, and `is_live` is only set while it runs.
fn is_live_or_upcoming(v: &Value) -> bool {
    is_live(v)
        || v["is_upcoming"].as_bool() == Some(true)
        || v["live_status"].as_str() == Some("is_upcoming")
}

/// The formats yt-dlp chose: both halves of a merge, or the video itself.
fn chosen_formats(v: &Value) -> Vec<&Value> {
    match v["requested_formats"].as_array() {
        Some(fs) => fs.iter().collect(),
        None => vec![v],
    }
}

/// A plain download, as opposed to an HLS or DASH manifest. `http_dash_segments`
/// also starts with "http", so this compares the whole protocol. The selector
/// asks for https only because the relay's clients refuse plain http.
fn is_plain(f: &Value) -> bool {
    matches!(f["protocol"].as_str(), Some("https" | "http"))
}

fn streams_from(v: &Value, url: &str) -> Result<Streams> {
    if is_live_or_upcoming(v) {
        bail!(LIVE_UNSUPPORTED);
    }
    let title = str_field(v, &["title"]).unwrap_or_else(|| url.into());
    let formats = chosen_formats(v);
    if !formats.iter().all(|f| is_plain(f)) {
        bail!(MANIFEST_ONLY);
    }
    let is_video = |f: &&Value| f["vcodec"].as_str() != Some("none");
    let video_fmt = formats
        .iter()
        .copied()
        .find(is_video)
        .or(formats.first().copied())
        .context("no playable format")?;
    let audio = formats
        .iter()
        .find(|f| !is_video(f))
        .and_then(|f| f["url"].as_str())
        .map(String::from);
    // yt-dlp attaches the cookies of its in-memory jar that match the URL.
    // None of YouTube's is meant for a googlevideo host, but the relay
    // promises to send no cookies, so that doesn't rest on yt-dlp's jar.
    let headers = video_fmt["http_headers"]
        .as_object()
        .map(|h| {
            h.iter()
                .filter(|(k, _)| !k.eq_ignore_ascii_case("cookie"))
                .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default();
    Ok(Streams {
        title,
        channel: str_field(v, &["channel", "uploader"]),
        channel_id: str_field(v, &["channel_id"]).filter(|c| ids::is_channel_id(c)),
        duration: v["duration"].as_f64().map(|d| d as u64),
        video: video_fmt["url"]
            .as_str()
            .context("format has no URL")?
            .into(),
        audio,
        headers,
        description: str_field(v, &["description"])
            .map(|d| d.chars().take(DESCRIPTION_MAX).collect()),
        views: v["view_count"].as_u64(),
        likes: v["like_count"].as_u64(),
        published: v["timestamp"].as_i64().or(v["release_timestamp"].as_i64()),
        chapters: chapters(v),
    })
}

/// The video's chapters, in the order yt-dlp lists them. One without a title
/// names nothing, so it is left out.
fn chapters(v: &Value) -> Vec<Chapter> {
    v["chapters"]
        .as_array()
        .map(|cs| {
            cs.iter()
                .filter_map(|c| {
                    Some(Chapter {
                        start: c["start_time"].as_f64()?.max(0.0) as u64,
                        title: str_field(c, &["title"])?,
                    })
                })
                .take(CHAPTERS_MAX)
                .collect()
        })
        .unwrap_or_default()
}

fn str_field(v: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|k| v[*k].as_str().filter(|s| !s.is_empty()).map(String::from))
}

fn entries(v: &Value) -> Vec<Video> {
    let parent_channel = str_field(v, &["channel", "uploader"]);
    let parent_id = str_field(v, &["channel_id"]);
    v["entries"]
        .as_array()
        .map(|es| {
            es.iter()
                .filter_map(|e| entry(e, &parent_channel, &parent_id))
                .collect()
        })
        .unwrap_or_default()
}

fn entry(e: &Value, channel: &Option<String>, channel_id: &Option<String>) -> Option<Video> {
    let id = e["id"].as_str().filter(|id| ids::is_video_id(id))?;
    Some(Video {
        id: id.into(),
        title: str_field(e, &["title"]).unwrap_or_else(|| id.into()),
        channel: str_field(e, &["channel", "uploader"]).or_else(|| channel.clone()),
        channel_id: str_field(e, &["channel_id"]).or_else(|| channel_id.clone()),
        published: e["timestamp"].as_i64().or(e["release_timestamp"].as_i64()),
        duration: e["duration"].as_f64().map(|d| d as u64),
        views: e["view_count"].as_u64(),
        short: e["url"].as_str().is_some_and(|u| u.contains("/shorts/")),
        live: is_live_or_upcoming(e),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn resolved(json: &str) -> Result<Streams> {
        streams_from(
            &serde_json::from_str(json).unwrap(),
            "https://example.test/v",
        )
    }

    #[test]
    fn plain_downloads_resolve() {
        let s = resolved(
            r#"{"title":"T","duration":12.5,"protocol":"https+https","requested_formats":[
                {"url":"https://gv.test/v","protocol":"https","vcodec":"av01","http_headers":{"User-Agent":"x"}},
                {"url":"https://gv.test/a","protocol":"https","vcodec":"none"}]}"#,
        )
        .unwrap();
        assert_eq!(
            (s.video.as_str(), s.audio.as_deref()),
            ("https://gv.test/v", Some("https://gv.test/a"))
        );
        assert_eq!(s.headers, [("User-Agent".to_string(), "x".to_string())]);
        // A cookie yt-dlp would send is not relayed, whatever the case it is spelled in.
        let s = resolved(
            r#"{"url":"https://gv.test/c","protocol":"https","vcodec":"avc1","http_headers":{"User-Agent":"x","Cookie":"a=b","cookie":"c=d"}}"#,
        )
        .unwrap();
        assert_eq!(s.headers, [("User-Agent".to_string(), "x".to_string())]);
        // A single combined format has its protocol at the top level.
        let s = resolved(
            r#"{"title":"T","url":"https://gv.test/c","protocol":"https","vcodec":"avc1"}"#,
        )
        .unwrap();
        assert_eq!((s.video.as_str(), s.audio), ("https://gv.test/c", None));
    }

    #[test]
    fn details_come_with_the_streams() {
        let s = resolved(
            r#"{"title":"T","url":"https://gv.test/c","protocol":"https","vcodec":"avc1",
                "description":"Line one\nLine two","view_count":59000,"like_count":640,"timestamp":1762700000,
                "chapters":[{"start_time":0.0,"title":"Intro"},{"start_time":252.4,"title":"Ownership"},{"start_time":300,"title":""}]}"#,
        )
        .unwrap();
        assert_eq!(s.description.as_deref(), Some("Line one\nLine two"));
        assert_eq!(
            (s.views, s.likes, s.published),
            (Some(59000), Some(640), Some(1762700000))
        );
        assert_eq!(
            s.chapters,
            [
                Chapter {
                    start: 0,
                    title: "Intro".into()
                },
                Chapter {
                    start: 252,
                    title: "Ownership".into()
                }
            ]
        );
        // A release date stands in for a missing upload date, and a video
        // that says nothing has nothing.
        let s = resolved(
            r#"{"url":"https://gv.test/c","protocol":"https","vcodec":"avc1","release_timestamp":5}"#,
        )
        .unwrap();
        assert_eq!(s.published, Some(5));
        assert!(s.description.is_none() && s.views.is_none() && s.chapters.is_empty());
    }

    #[test]
    fn a_long_description_is_cut_and_chapters_are_capped() {
        let text = "é".repeat(30_000);
        let chapters: Vec<String> = (0..300)
            .map(|i| format!(r#"{{"start_time":{i},"title":"c{i}"}}"#))
            .collect();
        let s = resolved(&format!(
            r#"{{"url":"https://gv.test/c","protocol":"https","vcodec":"avc1","description":"{text}","chapters":[{}]}}"#,
            chapters.join(",")
        ))
        .unwrap();
        assert_eq!(s.description.unwrap().chars().count(), 20_000);
        assert_eq!(s.chapters.len(), 200);
    }

    #[test]
    fn live_streams_are_refused() {
        for json in [
            r#"{"is_live":true,"url":"https://gv.test/x","protocol":"https"}"#,
            r#"{"live_status":"is_live","url":"https://gv.test/x","protocol":"https"}"#,
        ] {
            let e = resolved(json).unwrap_err().to_string();
            assert_eq!(e, LIVE_UNSUPPORTED);
        }
        // A finished stream (`was_live`) is an ordinary download.
        assert!(
            resolved(r#"{"live_status":"was_live","url":"https://gv.test/x","protocol":"https"}"#)
                .is_ok()
        );
    }

    #[test]
    fn manifests_are_refused_even_when_one_half_is_plain() {
        for protocol in ["m3u8_native", "m3u8", "http_dash_segments", "mhtml", "rtmp"] {
            let json = format!(
                r#"{{"requested_formats":[
                    {{"url":"https://gv.test/v","protocol":"https","vcodec":"avc1"}},
                    {{"url":"https://gv.test/a","protocol":"{protocol}","vcodec":"none"}}]}}"#
            );
            assert_eq!(
                resolved(&json).unwrap_err().to_string(),
                MANIFEST_ONLY,
                "{protocol}"
            );
        }
        assert!(
            resolved(r#"{"url":"https://gv.test/x"}"#).is_err(),
            "no protocol, no relay"
        );
    }

    #[test]
    fn parses_flat_entries() {
        let v: Value = serde_json::from_str(
            r#"{"channel":"Chan","channel_id":"UCsBjURrPoezykLs9EqgamOA","entries":[
                {"id":"5C_HPTJg5ek","title":"Rust in 100 Seconds","duration":149.0,"view_count":2648797},
                {"id":"UCxxxxxxxxxxxxxxxxxxxxxx","title":"a channel result"},
                {"id":"abcdefghijk","url":"https://www.youtube.com/shorts/abcdefghijk","channel":"Other"}
            ]}"#,
        )
        .unwrap();
        let es = entries(&v);
        assert_eq!(es.len(), 2);
        assert_eq!(es[0].channel.as_deref(), Some("Chan"));
        assert_eq!(es[0].duration, Some(149));
        assert!(es[1].short);
        assert_eq!(es[1].channel.as_deref(), Some("Other"));
    }

    #[test]
    fn running_and_announced_streams_are_marked_live_and_finished_ones_are_not() {
        let v: Value = serde_json::from_str(
            r#"{"entries":[
                {"id":"aaaaaaaaaaa","live_status":"is_live"},
                {"id":"bbbbbbbbbbb","live_status":"is_upcoming"},
                {"id":"ccccccccccc","is_live":true},
                {"id":"ddddddddddd","live_status":"was_live"},
                {"id":"eeeeeeeeeee","live_status":"post_live"},
                {"id":"fffffffffff","live_status":"not_live"},
                {"id":"ggggggggggg","live_status":null},
                {"id":"hhhhhhhhhhh"}
            ]}"#,
        )
        .unwrap();
        let live: Vec<_> = entries(&v).iter().map(|v| v.live).collect();
        assert_eq!(live, [true, true, true, false, false, false, false, false]);
    }

    #[test]
    fn an_announced_stream_is_refused_like_a_running_one() {
        let e = resolved(
            r#"{"live_status":"is_upcoming","url":"https://gv.test/x","protocol":"https"}"#,
        )
        .unwrap_err()
        .to_string();
        assert_eq!(e, LIVE_UNSUPPORTED);
    }

    use crate::net::TOR_PROXY;
    use crate::store::testing::{TempDir, script};

    /// `bin` is in a directory of its own (a `TempDir`), and so is the cache.
    fn ytdlp(bin: String, proxy: Option<&str>) -> Ytdlp {
        Ytdlp {
            cache_dir: Path::new(&bin).with_file_name("cache").join("qveil"),
            bin,
            net: Net::new(proxy.map(String::from), true).unwrap(),
            calls: Arc::default(),
        }
    }

    /// A yt-dlp that records what anyone on the machine could see of it
    /// (its real command line from /proc) and what is in its config file,
    /// then answers with an empty listing.
    const SPY: &str = r#"dir=$(dirname "$0")
tr '\0' '\n' < /proc/$$/cmdline > "$dir/argv"
while [ $# -gt 0 ]; do
  if [ "$1" = --config-locations ]; then
    cp "$2" "$dir/config"
    stat -c %a "$2" > "$dir/file-mode"
    stat -c %a "$(dirname "$2")" > "$dir/dir-mode"
    dirname "$2" > "$dir/private-dir"
  fi
  shift
done
echo '{"entries":[]}'
"#;

    fn spied(dir: &TempDir, name: &str) -> String {
        std::fs::read_to_string(dir.0.join(name)).unwrap()
    }

    #[test]
    fn what_you_search_and_the_proxy_password_stay_out_of_argv() {
        let dir = TempDir::new();
        let yt = ytdlp(
            script(&dir.0.join("yt-dlp"), SPY),
            Some("http://user:SECRETPW@127.0.0.1:3128"),
        );
        yt.search("secret query #1 it's", 5).unwrap();

        let argv = spied(&dir, "argv");
        for needle in [
            "SECRETPW",
            "127.0.0.1:3128",
            "secret",
            "ytsearch",
            "youtube.com",
        ] {
            assert!(!argv.contains(needle), "{needle} is in argv:\n{argv}");
        }
        for flag in [
            "--ignore-config",
            "--no-plugin-dirs",
            "--no-cookies",
            "--socket-timeout",
            "--config-locations",
        ] {
            assert!(argv.lines().any(|a| a == flag), "missing {flag}:\n{argv}");
        }
        // The engine's own User-Agent, not one yt-dlp picks for itself.
        let args: Vec<&str> = argv.lines().collect();
        let at = args.iter().position(|a| *a == "--user-agent").unwrap();
        assert_eq!(args[at + 1], USER_AGENT);

        // They are in a private file instead, quoted the way yt-dlp reads it.
        assert_eq!(
            spied(&dir, "config"),
            "\u{feff}--proxy 'http://user:SECRETPW@127.0.0.1:3128'\n'ytsearch5:secret query #1 it'\\''s'\n"
        );
        assert_eq!(spied(&dir, "file-mode").trim(), "600");
        assert_eq!(spied(&dir, "dir-mode").trim(), "700");
        // And the directory is gone once yt-dlp has run.
        assert!(!Path::new(spied(&dir, "private-dir").trim()).exists());
    }

    #[test]
    fn the_cache_directories_are_created_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new();
        let yt = ytdlp(script(&dir.0.join("yt-dlp"), SPY), None);
        // Neither the cache nor the directory above it exists yet, and yt-dlp
        // would create both with the default mode.
        yt.search("x", 1).unwrap();
        for d in [&yt.cache_dir, yt.cache_dir.parent().unwrap()] {
            let mode = std::fs::metadata(d).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "{}", d.display());
        }
    }

    #[test]
    fn the_watched_url_stays_out_of_argv_too() {
        let dir = TempDir::new();
        let yt = ytdlp(script(&dir.0.join("yt-dlp"), SPY), None);
        // The empty listing isn't a playable video, which is beside the point.
        let _ = yt.resolve("https://www.youtube.com/watch?v=dQw4w9WgXcQ", "b");
        let argv = spied(&dir, "argv");
        assert!(
            !argv.contains("dQw4w9WgXcQ") && !argv.contains("youtube"),
            "{argv}"
        );
        assert!(spied(&dir, "config").contains("'https://www.youtube.com/watch?v=dQw4w9WgXcQ'"));
    }

    #[test]
    fn manifests_are_skipped_when_resolving_but_not_when_explaining_a_failure() {
        let dir = TempDir::new();
        // A live stream: without its HLS manifest yt-dlp finds no format at
        // all, and with it, it reports the stream as live.
        let yt = ytdlp(
            script(
                &dir.0.join("yt-dlp"),
                r#"while [ $# -gt 0 ]; do [ "$1" = --config-locations ] && conf="$2"; shift; done
if grep -q "^--extractor-args 'youtube:skip=hls,dash'$" "$conf"; then
  echo skipped >> "$(dirname "$0")/calls"
  echo 'ERROR: [youtube] x: This live stream recording is not available.' >&2; exit 1
fi
echo full >> "$(dirname "$0")/calls"
echo '{"is_live":true}'"#,
            ),
            None,
        );
        let err = yt
            .resolve("https://www.youtube.com/watch?v=dQw4w9WgXcQ", "b")
            .unwrap_err();
        assert_eq!(err.to_string(), LIVE_UNSUPPORTED);
        let calls = spied(&dir, "calls");
        let calls: Vec<_> = calls.lines().collect();
        assert_eq!(calls, ["skipped", "full"]);
    }

    #[test]
    fn direct_says_so_instead_of_leaving_the_proxy_to_the_environment() {
        let dir = TempDir::new();
        let yt = ytdlp(script(&dir.0.join("yt-dlp"), SPY), None);
        yt.search("x", 1).unwrap();
        assert!(
            spied(&dir, "config").starts_with("\u{feff}--proxy ''\n"),
            "{}",
            spied(&dir, "config")
        );
    }

    #[test]
    fn tor_gets_a_fresh_identity_in_every_call() {
        let dir = TempDir::new();
        let yt = ytdlp(script(&dir.0.join("yt-dlp"), SPY), Some(TOR_PROXY));
        yt.search("x", 1).unwrap();
        let first = spied(&dir, "config");
        yt.search("x", 1).unwrap();
        assert_ne!(first, spied(&dir, "config"));
        assert!(first.contains("socks5h://qveil"), "{first}");
        assert!(!spied(&dir, "argv").contains("qveil0"));
    }

    #[test]
    fn quoting_keeps_every_character_literal() {
        assert_eq!(quote(""), "''");
        assert_eq!(quote("a b #c"), "'a b #c'");
        assert_eq!(quote(r"it's \n"), r"'it'\''s \n'");
        assert!(Call::new(&Net::default(), None, "--exec=evil").is_err());
        // A newline or NUL can't end the value early or break the file.
        let net = Net::default();
        let call = Call::new(&net, None, "ytsearch1:a\nb\0c").unwrap();
        let text = std::fs::read_to_string(&call.config).unwrap();
        assert!(text.ends_with("'ytsearch1:a b c'\n"), "{text:?}");
    }

    #[test]
    fn a_hung_yt_dlp_is_killed_and_cleaned_up() {
        let dir = TempDir::new();
        // `exec` so that the process that is killed is the sleeper itself.
        let hang = format!(
            "{}\nexec sleep 60\n",
            r#"echo $$ > "$(dirname "$0")/pid"
while [ $# -gt 0 ]; do
  [ "$1" = --config-locations ] && dirname "$2" > "$(dirname "$0")/private-dir"
  shift
done"#
        );
        let yt = ytdlp(script(&dir.0.join("yt-dlp"), &hang), None);
        let started = Instant::now();
        let err = yt
            .json(&["-J"], "https://x.test/", Duration::from_millis(400))
            .unwrap_err();
        assert!(err.to_string().contains("didn't finish within"), "{err:#}");
        assert!(started.elapsed() < Duration::from_secs(5));
        let pid = spied(&dir, "pid");
        assert!(
            !Path::new("/proc").join(pid.trim()).exists(),
            "yt-dlp was left running"
        );
        assert!(!Path::new(spied(&dir, "private-dir").trim()).exists());
    }

    #[test]
    fn a_lookup_that_is_given_up_on_kills_yt_dlp_and_what_it_started() {
        let dir = TempDir::new();
        // Like yt-dlp with a deno: a child of its own, which a kill of the
        // script alone would leave running.
        let hang = r#"d=$(dirname "$0")
sleep 60 &
echo $! > "$d/grandchild"
echo $$ > "$d/pid"
while [ $# -gt 0 ]; do
  [ "$1" = --config-locations ] && dirname "$2" > "$d/private-dir"
  shift
done
wait
"#;
        let yt = ytdlp(script(&dir.0.join("yt-dlp"), hang), None);
        let started = Instant::now();
        let asked = std::sync::atomic::AtomicUsize::new(0);
        // Nobody is waiting from the third look at it on.
        let abort = || asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst) >= 3;
        let err = yt
            .resolve_until("https://www.youtube.com/watch?v=dQw4w9WgXcQ", "b", &abort)
            .unwrap_err();
        assert_eq!(err.to_string(), CANCELLED);
        assert!(started.elapsed() < Duration::from_secs(5));
        for name in ["pid", "grandchild"] {
            let pid = spied(&dir, name);
            let gone = std::fs::read_to_string(format!("/proc/{}/stat", pid.trim()))
                .map(|s| s.rsplit(") ").next().is_some_and(|r| r.starts_with('Z')))
                .unwrap_or(true);
            assert!(gone, "{name} {pid} was left running");
        }
        assert!(!Path::new(spied(&dir, "private-dir").trim()).exists());
    }

    #[test]
    fn a_lot_of_output_does_not_stall_the_deadline() {
        let dir = TempDir::new();
        // Far more than a pipe holds, written before the JSON.
        let loud = "head -c 3000000 /dev/zero | tr '\\0' ' '\necho '{\"entries\":[]}'\n";
        let yt = ytdlp(script(&dir.0.join("yt-dlp"), loud), None);
        assert!(yt.search("x", 1).unwrap().is_empty());
    }

    #[test]
    fn a_refused_proxy_is_explained_for_the_route_in_use() {
        let dir = TempDir::new();
        let refused = "echo \"ERROR: [youtube] x: Unable to download API page: SocksHTTPSConnection(host='www.youtube.com', port=443): Failed to establish a new connection: [Errno 111] Connection refused\" >&2\nexit 1\n";
        let bin = script(&dir.0.join("yt-dlp"), refused);
        let tor = ytdlp(bin.clone(), Some(TOR_PROXY));
        let msg = tor.search("x", 1).unwrap_err().to_string();
        assert!(msg.contains("Tor listens on 127.0.0.1:9050"), "{msg}");
        let other = ytdlp(bin.clone(), Some("socks5h://10.0.0.2:1080"));
        let msg = other.search("x", 1).unwrap_err().to_string();
        assert!(
            msg.contains("10.0.0.2:1080") && !msg.contains("Tor"),
            "{msg}"
        );
        // On Direct a refusal isn't the proxy's.
        let direct = ytdlp(bin, None);
        let msg = direct.search("x", 1).unwrap_err().to_string();
        assert!(msg.contains("Connection refused"), "{msg}");
    }
}
