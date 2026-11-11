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
use crate::spare;
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
/// How often a running call asks whether it is still wanted. The window
/// hanging up is only looked for this often anyway (`http::WATCH_EVERY`).
const ABORT_EVERY: Duration = Duration::from_millis(100);
/// yt-dlp's own limit for one connection. The deadlines above bound the whole
/// call, and this keeps a single dead connection from using up all of it.
const SOCKET_TIMEOUT: &str = "20";

/// Why a live stream is refused: the relay can only carry a plain download.
const LIVE_UNSUPPORTED: &str =
    "Live streams aren't supported yet: they can't be relayed through your route.";
const MANIFEST_ONLY: &str = "This video is only offered as an HLS/DASH stream, which can't be relayed through your route yet.";

/// What an answer without a format to play ends with.
const NOTHING_PLAYABLE: &str = "no playable format";
const NO_URL: &str = "format has no URL";

/// What a lookup that its caller gave up on (the window hung up) ends with.
/// Nobody is left to read it; it is only the end of the call.
pub const CANCELLED: &str = "The request was cancelled";

/// The format selector only takes plain downloads, so the HLS manifest that
/// yt-dlp fetches for every video (a quarter of a second of a 1.3 s resolve,
/// measured) is never used. Skipped when resolving; `explain_no_format` still
/// fetches it, to tell a live stream from a video that has no plain download.
const SKIP_MANIFESTS: &str = "youtube:skip=hls,dash";

/// How a search is asked for. `approximate_date` turns YouTube's "3 days
/// ago" into a timestamp, so results can show an age like the feed does.
/// `skip=webpage` leaves out the YouTube homepage that yt-dlp otherwise
/// fetches before every search, only for its configuration (8.2 s of an
/// 11 s search over Tor, measured): it uses its built-in one instead, and
/// the search requests themselves are the same. (A channel's tabs are not
/// asked for this way: there it would put two requests in place of the
/// channel's page, not leave one out.)
const SEARCH_ARGS: &str = "youtubetab:approximate_date;skip=webpage";

/// How a video is looked up to be played, by default: `SKIP_MANIFESTS`,
/// which is yt-dlp's own way, the watch page and then YouTube's player
/// answer. The page brings what the player answer doesn't have (chapters,
/// the channel's name, the upload date), so a play has them at once.
///
/// The quick lookup, off unless `quick_lookup` is set (experimental): one
/// request, YouTube's player answer for the client whose streams are played.
/// Left to itself, yt-dlp first fetches the watch page (a megabyte of HTML,
/// 0.85 to 1.07 s over Tor, measured), for the `web` client's player answer
/// and the page data in it; but `web`'s streams need a token that yt-dlp
/// doesn't have and are dropped anyway. So the page, the clients' own
/// configuration pages, the page data and the `web` client are all left
/// out, and the rest of yt-dlp's default clients are asked as they would
/// have been. Measured over Tor it failed for an ordinary video (Big Buck
/// Bunny: one `visionos` player request, an error-sized answer, 2.7 s, most
/// likely for want of the page's context), and then the usual lookup ran
/// after it (see `worth_the_usual_way`): 6.0 s in place of 3.3 s, and one
/// more request. So it is off until that is understood. What the page data
/// had is then fetched apart, when it is wanted: see `details_of`.
const LOOKUP: &str =
    "youtube:skip=hls,dash;player_skip=webpage,configs,initial_data;player_client=default,-web";
/// How a video's chapters are looked up after a quick lookup (see
/// `LOOKUP`): the page data, from YouTube's `next` answer, and the player
/// answer that yt-dlp can't do without (it has no way to skip that), but
/// neither the page nor the player's JavaScript, and nothing is downloaded.
const CHAPTERS_LOOKUP: &str =
    "youtube:skip=hls,dash;player_skip=webpage,configs,js;player_client=default,-web";

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
    /// Whether a video is looked up without its watch page (see `LOOKUP`):
    /// the `quick_lookup` setting, off by default.
    pub quick_lookup: bool,
}

#[derive(Clone, Debug)]
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
    /// Whether the video may have chapters that the lookup didn't ask for:
    /// it was a quick one, which leaves the page data out (see `LOOKUP`),
    /// and the description named none. They are then looked up apart, when
    /// they are wanted.
    pub chapters_later: bool,
    /// Whether the lookup (a quick one) left the page data out, and with
    /// it the channel's name and the upload date: see `details_of`.
    pub details_later: bool,
}

/// What the page data says of a video, which a lookup to play it leaves
/// out (see `Ytdlp::details_of`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Details {
    pub chapters: Vec<Chapter>,
    pub channel: Option<String>,
    /// Unix seconds.
    pub published: Option<i64>,
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

    /// Keeps a spare yt-dlp ready for the calls on this route from now on,
    /// where yt-dlp can be run that way (see `spare`).
    pub fn keep_spare(&self) {
        if let Some(launch) = spare::launch_for(&self.bin) {
            self.calls.want_spare(&launch);
        }
    }

    /// Starts `cmd` through the registry, which refuses once the route it
    /// belongs to has been left. The spare takes it, if there is one.
    fn start(&self, cmd: &mut Command, call: &Call) -> Result<Run> {
        if let Some(launch) = spare::launch_for(&self.bin)
            && let Some(line) = spare::args_line(cmd)
            && let Some(run) = self.calls.claim_spare(&launch, &line, call.dir.path())
        {
            return Ok(run);
        }
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
    /// `abort` is asked every `ABORT_EVERY` while the child runs, and when
    /// it says yes the child is killed (with everything it started) and the
    /// call ends with `CANCELLED`. In between, this waits for the child to
    /// end and wakes up the moment it does.
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
            run.wait_exit(
                deadline
                    .saturating_duration_since(Instant::now())
                    .min(ABORT_EVERY),
            );
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
        let v = self.json(
            &["--flat-playlist", "--extractor-args", SEARCH_ARGS, "-J"],
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
    ///
    /// With `sp`, one of YouTube's own filters (see `lens::sp`), the search
    /// is asked for as the results page the filter menu links to, which is
    /// the only form that takes one.
    pub fn search_each(
        &self,
        query: &str,
        limit: usize,
        sp: Option<&str>,
        each: &mut dyn FnMut(Video) -> bool,
    ) -> Result<usize> {
        let encode =
            |s: &str| url::form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>();
        let (q, range) = match sp {
            None => (format!("ytsearch{limit}:{query}"), None),
            Some(sp) => (
                format!(
                    "https://www.youtube.com/results?search_query={}&sp={}",
                    encode(query),
                    encode(sp)
                ),
                // A results page has no end of its own.
                Some(format!("1:{limit}")),
            ),
        };
        crate::store::ensure_dir(&self.cache_dir)?;
        let call = Call::new(&self.net, None, &q)?;
        let mut cmd = self.command(&call);
        cmd.args([
            "--flat-playlist",
            "--lazy-playlist",
            "--extractor-args",
            SEARCH_ARGS,
            "-j",
        ]);
        if let Some(range) = &range {
            cmd.args(["-I", range]);
        }
        let mut run = self.start(&mut cmd, &call)?;
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
        if self.quick_lookup {
            let quick = self.json_with(&args, Some(LOOKUP), url, RESOLVE, abort);
            match quick.and_then(|v| streams_from(&v, url)) {
                Ok(mut s) => {
                    s.chapters_later = s.chapters.is_empty();
                    s.details_later = true;
                    return Ok(s);
                }
                Err(e) if !worth_the_usual_way(&e) => return Err(e),
                // Asked the usual way, page and all, below.
                Err(_) => {}
            }
        }
        let found = self
            .json_with(&args, Some(SKIP_MANIFESTS), url, RESOLVE, abort)
            .and_then(|v| streams_from(&v, url));
        match found {
            Err(e) if is_no_format(&e) => Err(self.explain_no_format(url, e, abort)),
            found => found,
        }
    }

    /// What the page data says of a video, which a quick lookup to play it
    /// leaves out (see `LOOKUP`): its chapters (what YouTube shows as its
    /// chapters, which may have been made by YouTube, or else those the
    /// description names), its channel's name and when it was uploaded.
    /// Asked the usual way, page and all, unless the quick lookup is on.
    pub fn details_of(&self, url: &str, abort: &dyn Fn() -> bool) -> Result<Details> {
        let args = [
            "-J",
            "--no-playlist",
            "--skip-download",
            "--ignore-no-formats-error",
        ];
        let usual = || self.json_with(&args, Some(SKIP_MANIFESTS), url, QUICK, abort);
        let v = if self.quick_lookup {
            match self.json_with(&args, Some(CHAPTERS_LOOKUP), url, QUICK, abort) {
                Err(e) if worth_the_usual_way(&e) => usual()?,
                found => found?,
            }
        } else {
            usual()?
        };
        Ok(Details {
            chapters: chapters(&v),
            channel: str_field(&v, &["channel", "uploader"]),
            published: published(&v),
        })
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

/// Whether a quick lookup (see `LOOKUP`) that ended with `e` is worth
/// asking again the usual way, page and all. yt-dlp asks more of YouTube
/// when it has the page: a yt-dlp whose only default client is `web` has
/// nothing left to ask without it, and a video made for kids is played by
/// clients that yt-dlp only adds when the page says what it is (and those
/// that the quick lookup asks can't play it). So whatever yt-dlp failed
/// with, or a result with nothing playable in it, is asked again; but not a
/// lookup that was given up on, or stopped, or ran out of time, or a route
/// that is down, which the usual way would only wait for again, nor a live
/// stream, which neither can play.
fn worth_the_usual_way(e: &anyhow::Error) -> bool {
    if let Some(f) = e.downcast_ref::<Failed>() {
        return !matches!(
            crate::errors::explain(&f.raw).code,
            crate::errors::Code::TorDown | crate::errors::Code::ProxyRefused
        );
    }
    // What `streams_from` found in an answer.
    let said = e.to_string();
    said == NOTHING_PLAYABLE || said == NO_URL
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
        .context(NOTHING_PLAYABLE)?;
    let audio_fmt = formats.iter().find(|f| !is_video(f));
    let audio = audio_fmt.and_then(|f| f["url"].as_str()).map(String::from);
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
    let duration = v["duration"].as_f64().map(|d| d as u64);
    let description: Option<String> =
        str_field(v, &["description"]).map(|d| d.chars().take(DESCRIPTION_MAX).collect());
    let mut chapters = chapters(v);
    if chapters.is_empty()
        && let (Some(d), Some(length)) = (&description, duration)
    {
        chapters = description_chapters(d, length);
    }
    Ok(Streams {
        title,
        channel: str_field(v, &["channel", "uploader"]),
        channel_id: str_field(v, &["channel_id"]).filter(|c| ids::is_channel_id(c)),
        duration,
        video: video_fmt["url"].as_str().context(NO_URL)?.into(),
        audio,
        headers,
        description,
        views: v["view_count"].as_u64(),
        likes: v["like_count"].as_u64(),
        published: published(v),
        chapters,
        chapters_later: false,
        details_later: false,
    })
}

/// When the video was uploaded (or, for a stream, started), in Unix seconds:
/// to the second where yt-dlp has it, or else the start of the day it was.
fn published(v: &Value) -> Option<i64> {
    v["timestamp"]
        .as_i64()
        .or(v["release_timestamp"].as_i64())
        .or_else(|| {
            let day = jiff::civil::Date::strptime("%Y%m%d", v["upload_date"].as_str()?).ok()?;
            Some(
                day.to_zoned(jiff::tz::TimeZone::UTC)
                    .ok()?
                    .timestamp()
                    .as_second(),
            )
        })
}

/// The chapters a description names, by YouTube's rule for them: lines
/// that start with a time and go on with a title, the first at 0:00, three
/// or more, each later than the one before and within the video. (yt-dlp
/// reads them the same way when it has the page data, but not without it.)
fn description_chapters(description: &str, duration: u64) -> Vec<Chapter> {
    let found: Vec<Chapter> = description
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let (stamp, rest) = line.split_at(line.find(|c: char| c.is_whitespace())?);
            let start = clock_seconds(stamp)?;
            // A dash or the like between the time and the title.
            let title = rest
                .trim_start()
                .trim_start_matches(|c: char| !c.is_alphanumeric() && !c.is_whitespace())
                .trim();
            (!title.is_empty()).then(|| Chapter {
                start,
                title: title.into(),
            })
        })
        .take(CHAPTERS_MAX)
        .collect();
    let in_order = found.windows(2).all(|w| w[0].start < w[1].start);
    let fits = found.last().is_some_and(|c| c.start < duration);
    if found.len() >= 3 && found[0].start == 0 && in_order && fits {
        found
    } else {
        Vec::new()
    }
}

/// `m:ss`, `mm:ss` or `h:mm:ss`, in seconds.
fn clock_seconds(stamp: &str) -> Option<u64> {
    let parts: Vec<&str> = stamp.split(':').collect();
    let num = |p: &str, max_len: usize| {
        (!p.is_empty() && p.len() <= max_len && p.bytes().all(|b| b.is_ascii_digit()))
            .then(|| p.parse::<u64>().ok())
            .flatten()
    };
    match parts.as_slice() {
        [m, s] if s.len() == 2 => Some(num(m, 2)? * 60 + num(s, 2).filter(|&s| s < 60)?),
        [h, m, s] if m.len() == 2 && s.len() == 2 => Some(
            num(h, 3)? * 3600
                + num(m, 2).filter(|&m| m < 60)? * 60
                + num(s, 2).filter(|&s| s < 60)?,
        ),
        _ => None,
    }
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
        // And the day it was uploaded, at its start, for either.
        let s = resolved(
            r#"{"url":"https://gv.test/c","protocol":"https","vcodec":"avc1","upload_date":"20251109"}"#,
        )
        .unwrap();
        assert_eq!(s.published, Some(1762646400));
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
            quick_lookup: false,
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

    /// yt-dlp keeps what it works out once per version of YouTube's player
    /// (the signature solutions) in its cache, so every call has to be
    /// given the same one, and one that outlives it: not the call's private
    /// directory, which is removed when it ends. (yt-dlp 2026.08 keeps
    /// nothing else there: it fetches the player's JavaScript anew in every
    /// call, and the lookup to play a video doesn't need it at all.)
    #[test]
    fn every_call_is_given_the_one_lasting_cache() {
        let dir = TempDir::new();
        let yt = ytdlp(script(&dir.0.join("yt-dlp"), SPY), None);
        let cache_of = |argv: &str| {
            argv.lines()
                .skip_while(|l| *l != "--cache-dir")
                .nth(1)
                .unwrap()
                .to_string()
        };
        yt.search("x", 1).unwrap();
        let (first, private) = (spied(&dir, "argv"), spied(&dir, "private-dir"));
        let _ = yt.resolve("https://www.youtube.com/watch?v=dQw4w9WgXcQ", "b");
        let second = spied(&dir, "argv");
        assert_eq!(cache_of(&first), yt.cache_dir.display().to_string());
        assert_eq!(cache_of(&first), cache_of(&second));
        assert!(!cache_of(&first).starts_with(private.trim()));
        assert!(yt.cache_dir.is_dir());
    }

    #[test]
    fn a_search_skips_the_homepage_however_it_is_asked() {
        let dir = TempDir::new();
        let yt = ytdlp(script(&dir.0.join("yt-dlp"), SPY), None);
        let skips = |argv: &str| {
            argv.lines()
                .any(|l| l == "youtubetab:approximate_date;skip=webpage")
        };
        yt.search("x", 1).unwrap();
        assert!(skips(&spied(&dir, "argv")), "{}", spied(&dir, "argv"));
        for sp in [None, Some("EgIYAg%3D%3D")] {
            yt.search_each("x", 1, sp, &mut |_| true).unwrap();
            assert!(
                skips(&spied(&dir, "argv")),
                "{sp:?}: {}",
                spied(&dir, "argv")
            );
        }
    }

    /// `ytdlp`, with the quick lookup (see `LOOKUP`) turned on.
    fn quick(mut yt: Ytdlp) -> Ytdlp {
        yt.quick_lookup = true;
        yt
    }

    #[test]
    fn a_lookup_fetches_the_watch_page_unless_the_quick_lookup_is_on() {
        let dir = TempDir::new();
        let yt = ytdlp(script(&dir.0.join("yt-dlp"), SPY), None);
        let _ = yt.resolve("https://www.youtube.com/watch?v=dQw4w9WgXcQ", "b");
        let config = spied(&dir, "config");
        assert!(
            config.contains("--extractor-args 'youtube:skip=hls,dash'\n"),
            "{config}"
        );
        assert!(!config.contains("player_skip"), "{config}");

        let dir = TempDir::new();
        let yt = quick(ytdlp(script(&dir.0.join("yt-dlp"), SPY), None));
        let _ = yt.resolve("https://www.youtube.com/watch?v=dQw4w9WgXcQ", "b");
        assert!(
            spied(&dir, "config").contains(
                "--extractor-args 'youtube:skip=hls,dash;player_skip=webpage,configs,initial_data;player_client=default,-web'\n"
            ),
            "{}",
            spied(&dir, "config")
        );
    }

    /// With the page in the lookup, what it has (chapters, the channel's
    /// name, the upload date) comes with the streams, and nothing is left
    /// to be looked up later.
    #[test]
    fn a_lookup_with_the_page_has_its_details_in_one_call() {
        let dir = TempDir::new();
        let yt = ytdlp(
            script(
                &dir.0.join("yt-dlp"),
                r#"echo run >> "$(dirname "$0")/calls"
echo '{"title":"T","channel":"Chan","upload_date":"20251109","url":"https://gv.test/c","protocol":"https","vcodec":"avc1","duration":600,"chapters":[{"start_time":0,"title":"Intro"}]}'"#,
            ),
            None,
        );
        let s = yt
            .resolve("https://www.youtube.com/watch?v=dQw4w9WgXcQ", "b")
            .unwrap();
        assert_eq!(s.channel.as_deref(), Some("Chan"));
        assert_eq!(s.published, Some(1762646400));
        assert_eq!(s.chapters.len(), 1);
        assert!(!s.chapters_later && !s.details_later);
        assert_eq!(spied(&dir, "calls"), "run\n");
        // Without chapters, none are looked for later either.
        let dir = TempDir::new();
        let yt = ytdlp(
            script(
                &dir.0.join("yt-dlp"),
                r#"echo '{"title":"T","url":"https://gv.test/c","protocol":"https","vcodec":"avc1","duration":600}'"#,
            ),
            None,
        );
        let s = yt
            .resolve("https://www.youtube.com/watch?v=dQw4w9WgXcQ", "b")
            .unwrap();
        assert!(s.chapters.is_empty() && !s.chapters_later && !s.details_later);
    }

    #[test]
    fn a_yt_dlp_left_with_no_client_to_ask_is_asked_the_usual_way() {
        let dir = TempDir::new();
        let yt = ytdlp(
            script(
                &dir.0.join("yt-dlp"),
                r#"while [ $# -gt 0 ]; do [ "$1" = --config-locations ] && conf="$2"; shift; done
if grep -q "player_client=default,-web" "$conf"; then
  echo quick >> "$(dirname "$0")/calls"
  echo 'ERROR: [youtube] x: No player clients have been requested' >&2; exit 1
fi
echo usual >> "$(dirname "$0")/calls"
echo '{"title":"T","url":"https://gv.test/c","protocol":"https","vcodec":"avc1"}'"#,
            ),
            None,
        );
        let yt = quick(yt);
        let s = yt
            .resolve("https://www.youtube.com/watch?v=dQw4w9WgXcQ", "b")
            .unwrap();
        assert_eq!(s.video, "https://gv.test/c");
        // Asked the usual way, the page was in it, chapters and all.
        assert!(!s.chapters_later);
        assert_eq!(spied(&dir, "calls"), "quick\nusual\n");
    }

    /// A yt-dlp whose quick lookup (the player answer only, see `LOOKUP`)
    /// ends with `quick`, a line of shell, and whose lookup the usual way,
    /// page and all, answers with a playable video. Each call is logged as
    /// `quick` or `usual`.
    fn quick_then_usual(dir: &TempDir, quick: &str) -> Ytdlp {
        self::quick(ytdlp(
            script(
                &dir.0.join("yt-dlp"),
                &format!(
                    r#"while [ $# -gt 0 ]; do [ "$1" = --config-locations ] && conf="$2"; shift; done
if grep -q "player_skip=webpage" "$conf"; then
  echo quick >> "$(dirname "$0")/calls"
  {quick}
fi
echo usual >> "$(dirname "$0")/calls"
echo '{{"title":"T","url":"https://gv.test/c","protocol":"https","vcodec":"avc1","duration":600,"chapters":[{{"start_time":0,"title":"Intro"}}]}}'"#
                ),
            ),
            None,
        ))
    }

    /// A video made for kids, among others, is only played by the clients
    /// that yt-dlp adds when the watch page says so, and the quick lookup
    /// has no page: whatever it fails with (but being given up on), the
    /// video is looked up again the usual way.
    #[test]
    fn a_video_the_quick_lookup_cannot_play_is_looked_up_the_usual_way() {
        for quick in [
            // What visionos says of a video made for kids, more or less.
            "echo 'ERROR: [youtube] x: Video unavailable. This video is not available' >&2; exit 1",
            "echo 'ERROR: [youtube] x: Sign in to confirm your age. This video may be inappropriate for some users.' >&2; exit 1",
            // Nothing playable left once the unplayable clients are dropped.
            "echo 'ERROR: [youtube] x: Requested format is not available. Use --list-formats for a list of available formats' >&2; exit 1",
            "echo 'ERROR: [youtube] x: No video formats found!' >&2; exit 1",
            // An answer without a playable format in it.
            r#"echo '{"title":"T","requested_formats":[]}'; exit 0"#,
        ] {
            let dir = TempDir::new();
            let yt = quick_then_usual(&dir, quick);
            let s = yt
                .resolve("https://www.youtube.com/watch?v=dQw4w9WgXcQ", "b")
                .unwrap_or_else(|e| panic!("{quick}: {e:#}"));
            assert_eq!(s.video, "https://gv.test/c", "{quick}");
            // The page was in it, chapters and all.
            assert!(!s.chapters_later, "{quick}");
            assert_eq!(spied(&dir, "calls"), "quick\nusual\n", "{quick}");
        }
    }

    #[test]
    fn a_lookup_given_up_on_or_a_route_that_is_down_is_not_looked_up_again() {
        let dir = TempDir::new();
        let yt = quick_then_usual(&dir, "sleep 30");
        let started = Instant::now();
        let err = yt
            .resolve_until("https://www.youtube.com/watch?v=dQw4w9WgXcQ", "b", &|| {
                started.elapsed() > Duration::from_millis(300)
            })
            .unwrap_err();
        assert_eq!(err.to_string(), CANCELLED);
        assert_eq!(spied(&dir, "calls"), "quick\n");

        let dir = TempDir::new();
        let mut yt = quick_then_usual(
            &dir,
            "echo 'ERROR: [youtube] x: Unable to download API page: [Errno 111] Connection refused' >&2; exit 1",
        );
        yt.net = Net::new(Some(TOR_PROXY.into()), true).unwrap();
        let err = yt
            .resolve("https://www.youtube.com/watch?v=dQw4w9WgXcQ", "b")
            .unwrap_err();
        assert!(err.to_string().contains("Tor"), "{err}");
        assert_eq!(spied(&dir, "calls"), "quick\n");
    }

    #[test]
    fn chapters_the_quick_lookup_cannot_have_are_looked_up_the_usual_way() {
        let dir = TempDir::new();
        let yt = quick_then_usual(
            &dir,
            "echo 'ERROR: [youtube] x: Video unavailable. This video is not available' >&2; exit 1",
        );
        let found = yt
            .details_of("https://www.youtube.com/watch?v=dQw4w9WgXcQ", &|| false)
            .unwrap();
        assert_eq!(
            found.chapters,
            [Chapter {
                start: 0,
                title: "Intro".into()
            }]
        );
        assert_eq!(spied(&dir, "calls"), "quick\nusual\n");
    }

    #[test]
    fn details_are_looked_up_with_the_page_and_without_the_streams() {
        for quick_lookup in [false, true] {
            details_are_looked_up(quick_lookup);
        }
    }

    fn details_are_looked_up(quick_lookup: bool) {
        let dir = TempDir::new();
        let answer = r#"{"duration":600,"channel":"Chan","uploader":"Chan","upload_date":"20251109","chapters":[{"start_time":0,"title":"Intro"},{"start_time":90,"title":"Part"}]}"#;
        let mut yt = ytdlp(
            script(
                &dir.0.join("yt-dlp"),
                &format!(
                    "{}echo '{answer}'\n",
                    SPY.trim_end().trim_end_matches(r#"echo '{"entries":[]}'"#)
                ),
            ),
            None,
        );
        yt.quick_lookup = quick_lookup;
        let found = yt
            .details_of("https://www.youtube.com/watch?v=dQw4w9WgXcQ", &|| false)
            .unwrap();
        assert_eq!(
            found.chapters,
            [
                Chapter {
                    start: 0,
                    title: "Intro".into()
                },
                Chapter {
                    start: 90,
                    title: "Part".into()
                }
            ]
        );
        // The page data names the channel, and the day it was uploaded,
        // which a lookup to play the video has neither of.
        assert_eq!(found.channel.as_deref(), Some("Chan"));
        assert_eq!(found.published, Some(1762646400));
        let config = spied(&dir, "config");
        // The page and all, unless the quick lookup is on: then the page
        // data from YouTube's `next` answer, without the page.
        let args = if quick_lookup {
            "--extractor-args 'youtube:skip=hls,dash;player_skip=webpage,configs,js;player_client=default,-web'\n"
        } else {
            "--extractor-args 'youtube:skip=hls,dash'\n"
        };
        assert!(config.contains(args), "{config}");
        let argv = spied(&dir, "argv");
        for arg in [
            "--skip-download",
            "--ignore-no-formats-error",
            "--no-playlist",
        ] {
            assert!(argv.lines().any(|l| l == arg), "{arg} in {argv}");
        }
    }

    #[test]
    fn a_lookup_without_the_page_finds_the_chapters_written_in_the_description() {
        let json = |desc: &str, chapters: &str| {
            format!(
                r#"{{"url":"https://gv.test/c","protocol":"https","vcodec":"avc1","duration":600,"description":{},"chapters":{chapters}}}"#,
                serde_json::to_string(desc).unwrap()
            )
        };
        let chapter = |start, title: &str| Chapter {
            start,
            title: title.into(),
        };
        let desc =
            "What this is about.\n\n0:00 Intro\n1:30 - The middle part\n 9:05 The end \nThanks!";
        let s = resolved(&json(desc, "null")).unwrap();
        assert_eq!(
            s.chapters,
            [
                chapter(0, "Intro"),
                chapter(90, "The middle part"),
                chapter(545, "The end")
            ]
        );
        // As YouTube has them: from 0:00, three or more, in order, within the video.
        for desc in [
            "1:00 a\n2:00 b\n3:00 c",
            "0:00 a\n2:00 b",
            "0:00 a\n3:00 b\n2:00 c",
            "0:00 a\n3:00 b\n11:00 c",
            "0:00 a\n1:00:00 b\n2:00:00 c",
        ] {
            assert!(
                resolved(&json(desc, "null")).unwrap().chapters.is_empty(),
                "{desc:?}"
            );
        }
        assert_eq!(
            resolved(&json("0:00 a\n1:00 b\n1:01:00 c", "null"))
                .unwrap()
                .chapters
                .len(),
            0
        );
        // An hour and more is h:mm:ss.
        let long = r#"{"url":"https://gv.test/c","protocol":"https","vcodec":"avc1","duration":7200,"description":"0:00 a\n30:00 b\n1:05:30 c"}"#;
        assert_eq!(resolved(long).unwrap().chapters[2], chapter(3930, "c"));
        // YouTube's own chapters, where yt-dlp has them, win.
        let s = resolved(&json(desc, r#"[{"start_time":0,"title":"Theirs"}]"#)).unwrap();
        assert_eq!(s.chapters, [chapter(0, "Theirs")]);
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
if grep -q "^--extractor-args 'youtube:skip=hls,dash" "$conf"; then
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
        // The lookup, which finds no format, and the one that says why.
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
        // The credentials are in the config file, never on the command line.
        let creds = first
            .split_once("socks5h://")
            .and_then(|(_, rest)| rest.split_once('@'))
            .map(|(creds, _)| creds.to_string())
            .unwrap_or_else(|| panic!("{first}"));
        assert!(!spied(&dir, "argv").contains(&creds));
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
            let gone = || {
                std::fs::read_to_string(format!("/proc/{}/stat", pid.trim()))
                    .map(|s| s.rsplit(") ").next().is_some_and(|r| r.starts_with('Z')))
                    .unwrap_or(true)
            };
            // The group was sent SIGKILL, but the call returns as soon as
            // the leader has ended, and a grandchild may take a moment longer.
            let killed = Instant::now();
            while !gone() {
                assert!(
                    killed.elapsed() < Duration::from_secs(5),
                    "{name} {pid} was left running"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
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

#[cfg(test)]
mod spare_tests {
    use super::*;
    use crate::inflight::Why;
    use crate::store::testing::TempDir;
    use std::path::Path;

    /// A yt-dlp written in Python, installed the way pip installs it: the
    /// entry script, and the `yt_dlp` package beside it. It logs when it is
    /// imported and when it runs, by which process, and what it was given.
    fn python_ytdlp(dir: &Path) -> Option<String> {
        let python = std::env::split_paths(&std::env::var_os("PATH")?)
            .map(|d| d.join("python3"))
            .find(|p| p.is_file())?;
        let pkg = dir.join("yt_dlp");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(
            pkg.join("__init__.py"),
            r#"import json, os, subprocess, sys, time
HERE = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
def log(what):
    with open(os.path.join(HERE, "log"), "a") as f:
        f.write(f"{what} {os.getpid()} {time.time()}\n")
log("import")
def main(argv=None):
    log("run")
    args = sys.argv[1:]
    conf = args[args.index("--config-locations") + 1]
    target = open(conf, encoding="utf-8-sig").read().splitlines()[-1]
    with open(os.path.join(HERE, f"call-{os.getpid()}"), "w") as f:
        json.dump({"argv": args, "stdin": os.readlink("/proc/self/fd/0"),
                   "pgid": os.getpgid(0), "argv0": sys.argv[0]}, f)
    if "hang" in target:
        child = subprocess.Popen(["sleep", "60"])
        with open(os.path.join(HERE, "hung"), "w") as f:
            f.write(f"{os.getpid()} {child.pid} {os.path.dirname(conf)}\n")
        time.sleep(60)
    print('{"entries":[]}')
    return 0
"#,
        )
        .unwrap();
        let script = dir.join("yt-dlp");
        std::fs::write(
            &script,
            format!(
                "#!{}\nimport sys\nfrom yt_dlp import main\nif __name__ == '__main__':\n    sys.exit(main())\n",
                python.display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        Some(script.display().to_string())
    }

    fn yt(bin: String) -> Ytdlp {
        Ytdlp {
            cache_dir: Path::new(&bin).with_file_name("cache").join("qveil"),
            bin,
            net: Net::new(Some("http://user:SECRETPW@127.0.0.1:3128".into()), true).unwrap(),
            calls: Arc::default(),
            quick_lookup: false,
        }
    }

    /// The log's lines: what happened, and in which process.
    fn log(dir: &Path) -> Vec<(String, u32)> {
        std::fs::read_to_string(dir.join("log"))
            .unwrap_or_default()
            .lines()
            .map(|l| {
                let mut w = l.split(' ');
                (
                    w.next().unwrap().to_string(),
                    w.next().unwrap().parse().unwrap(),
                )
            })
            .collect()
    }

    /// Waits until a process has imported yt-dlp and not run it: the spare.
    fn spare_pid(dir: &Path) -> u32 {
        let started = Instant::now();
        loop {
            let log = log(dir);
            if let Some((_, pid)) = log
                .iter()
                .rev()
                .find(|(what, pid)| what == "import" && !log.contains(&("run".into(), *pid)))
            {
                return *pid;
            }
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "no spare: {log:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Waits until the spare is ready for a call, and returns its process id.
    fn ready_spare(yt: &Ytdlp, dir: &Path) -> u32 {
        let started = Instant::now();
        while !yt.calls.spare_ready() {
            assert!(started.elapsed() < Duration::from_secs(10), "no spare");
            std::thread::sleep(Duration::from_millis(10));
        }
        spare_pid(dir)
    }

    fn call(dir: &Path, pid: u32) -> Value {
        serde_json::from_str(&std::fs::read_to_string(dir.join(format!("call-{pid}"))).unwrap())
            .unwrap()
    }

    fn gone(pid: u32) -> bool {
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .map(|s| s.rsplit(") ").next().is_some_and(|r| r.starts_with('Z')))
            .unwrap_or(true)
    }

    fn wait_gone(pid: u32) {
        let started = Instant::now();
        while !gone(pid) {
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "{pid} still runs"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// The call's arguments, with its private config file's path left out.
    fn argv_without_config(c: &Value) -> Vec<String> {
        let args: Vec<String> = serde_json::from_value(c["argv"].clone()).unwrap();
        let at = args.iter().position(|a| a == "--config-locations").unwrap();
        args.iter()
            .enumerate()
            .filter(|(i, _)| *i != at + 1)
            .map(|(_, a)| a.clone())
            .collect()
    }

    #[test]
    fn a_spare_takes_the_next_call_exactly_as_a_new_process_would() {
        let dir = TempDir::new();
        let Some(bin) = python_ytdlp(&dir.0) else {
            return;
        };
        let yt = yt(bin.clone());
        // Without a spare asked for, a call starts the usual way.
        yt.search("q", 5).unwrap();
        let usual = log(&dir.0);
        assert_eq!(usual[0].0, "import");
        assert_eq!(usual[0].1, usual[1].1);
        let usual = call(&dir.0, usual[0].1);

        yt.keep_spare();
        let spare = ready_spare(&yt, &dir.0);
        // Its own process group, as every child of the engine has.
        assert_eq!(unsafe { libc::getpgid(spare as libc::pid_t) } as u32, spare);
        let started = Instant::now();
        yt.search("q", 5).unwrap();
        let took = started.elapsed();
        // The spare ran it. (The one that replaces it may have started
        // since, and logged its import after that.)
        let runs: Vec<_> = log(&dir.0)
            .into_iter()
            .filter(|(w, _)| w == "run")
            .collect();
        assert_eq!(runs.last().unwrap(), &("run".to_string(), spare));
        let taken = call(&dir.0, spare);
        // The same arguments, and nothing to read on stdin.
        assert_eq!(argv_without_config(&taken), argv_without_config(&usual));
        assert_eq!(taken["stdin"], "/dev/null");
        assert_eq!(taken["argv0"], usual["argv0"]);
        assert_eq!(taken["pgid"], spare);
        // Nothing private shows on its command line either.
        let cmdline = std::fs::read(format!("/proc/{}/cmdline", spare_pid_next(&dir.0, spare)))
            .unwrap_or_default();
        let cmdline = String::from_utf8_lossy(&cmdline);
        assert!(
            !cmdline.contains("SECRETPW") && !cmdline.contains("ytsearch"),
            "{cmdline}"
        );
        wait_gone(spare);
        eprintln!("a call on the spare took {took:?}");

        // The next spare is there, and a route change ends it.
        let next = spare_pid(&dir.0);
        assert_ne!(next, spare);
        yt.calls.close(Why::RouteChanged);
        wait_gone(next);
    }

    /// The spare started after `after`.
    fn spare_pid_next(dir: &Path, after: u32) -> u32 {
        let started = Instant::now();
        loop {
            let pid = spare_pid(dir);
            if pid != after {
                return pid;
            }
            assert!(started.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn a_spare_whose_call_is_given_up_on_is_killed_with_what_it_started() {
        let dir = TempDir::new();
        let Some(bin) = python_ytdlp(&dir.0) else {
            return;
        };
        let yt = yt(bin);
        yt.keep_spare();
        let spare = ready_spare(&yt, &dir.0);
        let hung = dir.0.join("hung");
        let abort = || hung.exists();
        let err = yt
            .resolve_until("https://www.youtube.com/watch?v=hanghanghan", "b", &abort)
            .unwrap_err();
        assert_eq!(err.to_string(), CANCELLED);
        let text = std::fs::read_to_string(&hung).unwrap();
        let w: Vec<&str> = text.split_whitespace().collect();
        assert_eq!(w[0].parse::<u32>().unwrap(), spare);
        wait_gone(spare);
        wait_gone(w[1].parse().unwrap());
        assert!(!Path::new(w[2]).exists(), "its private files are left");
        yt.calls.close(Why::RouteChanged);
    }

    #[test]
    fn a_spare_of_a_yt_dlp_that_was_upgraded_since_runs_it_afresh() {
        let dir = TempDir::new();
        let Some(bin) = python_ytdlp(&dir.0) else {
            return;
        };
        let yt = yt(bin);
        yt.keep_spare();
        let spare = ready_spare(&yt, &dir.0);
        let init = dir.0.join("yt_dlp").join("__init__.py");
        let text = std::fs::read_to_string(&init).unwrap();
        std::fs::write(&init, text.replace("log(\"run\")", "log(\"run2\")")).unwrap();
        yt.search("q", 1).unwrap();
        let log = log(&dir.0);
        // The same process, which imported the new code and ran that.
        let of_spare: Vec<_> = log
            .iter()
            .filter(|(_, pid)| *pid == spare)
            .map(|(what, _)| what.as_str())
            .collect();
        assert_eq!(of_spare, ["import", "import", "run2"]);
        yt.calls.close(Why::RouteChanged);
    }

    #[test]
    fn a_spare_that_cannot_start_leaves_calls_to_start_the_usual_way() {
        let dir = TempDir::new();
        let Some(bin) = python_ytdlp(&dir.0) else {
            return;
        };
        // Plugins that were loaded on import: the spare refuses to serve.
        std::fs::write(
            dir.0.join("yt_dlp").join("globals.py"),
            "class _I:\n    value = True\nall_plugins_loaded = _I()\n",
        )
        .unwrap();
        let init = dir.0.join("yt_dlp").join("__init__.py");
        let text = std::fs::read_to_string(&init).unwrap();
        std::fs::write(&init, format!("from . import globals\n{text}")).unwrap();
        let yt = yt(bin);
        yt.keep_spare();
        let started = Instant::now();
        while yt.calls.running() > 0 || log(&dir.0).is_empty() {
            assert!(started.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(10));
        }
        yt.search("q", 1).unwrap();
        let log = log(&dir.0);
        let (last_import, last_run) = (&log[log.len() - 2], &log[log.len() - 1]);
        assert_eq!(
            (last_import.0.as_str(), last_run.0.as_str()),
            ("import", "run")
        );
        assert_eq!(last_import.1, last_run.1, "a fresh process ran the call");
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(yt.calls.running(), 0, "another spare was started");
        yt.calls.close(Why::RouteChanged);
    }
}
