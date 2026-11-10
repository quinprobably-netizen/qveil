//! qveil-core: the engine behind the qveil desktop app, and a command line of
//! its own. `qveil-core serve` runs the loopback server the app talks to.

/// Prints to stderr, and ignores a failure to: when what reads it has gone
/// (`qveil feed 2>&1 | head -1`), `eprintln!` would panic. Defined before the
/// modules so that they can use it too.
macro_rules! note {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stderr(), $($arg)*);
    }};
}

/// `println!` for a function that returns a `Result`: when the reader of
/// stdout has hung up (`qveil config | head -1`) it returns `Hangup`, which
/// `main` takes for a quiet end, where `println!` would panic with "Broken pipe".
macro_rules! outln {
    ($($arg:tt)*) => {
        $crate::emit(format_args!("{}\n", format_args!($($arg)*)))?
    };
}

/// `print!`, the same way as `outln!`.
macro_rules! out {
    ($($arg:tt)*) => {
        $crate::emit(format_args!($($arg)*))?
    };
}

mod app;
mod comments;
mod config;
mod errors;
mod feed;
mod ids;
mod inflight;
mod lens;
mod net;
mod pick;
mod player;
mod privacy;
mod private;
mod proc;
mod resume;
mod server;
mod signals;
mod store;
mod subs;
mod util;
mod video;
mod ytdlp;

use anyhow::{Context, Result};
use app::Runtime;
use clap::{Args, Parser, Subcommand};
use config::Config;
use errors::friendly;
use serde::Serialize;
use server::Server;
use std::io::{BufRead, IsTerminal, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use store::Paths;
use video::Video;

#[derive(Parser)]
#[command(
    name = "qveil",
    // The binary is qveil-core, started by `qveil`; help should show the name people type.
    bin_name = "qveil",
    version,
    about = "A private, account-free YouTube frontend",
    after_help = "Run `qveil` with no command (or `qveil open`) to open the app window."
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

/// The route of a command that makes network requests. Only those take it:
/// the others never leave the machine, so they have no use for it.
#[derive(Args, Clone, Default)]
struct Route {
    /// Route all traffic through this proxy (http://, https://, socks5h://). Overrides the config
    #[arg(long, value_name = "URL")]
    proxy: Option<String>,
    /// Use the local Tor SOCKS port (127.0.0.1:9050) with a fresh circuit per request
    #[arg(long, conflicts_with = "proxy")]
    tor: bool,
}

/// What a command line argument shows to other users of this machine.
const IN_PROC: &str = "Anything you type here is visible to other users of this machine, through /proc, while the command runs. The app window doesn't have that problem.";

#[derive(Subcommand)]
enum Cmd {
    /// Search for videos and pick one to play
    #[command(visible_alias = "s", after_help = IN_PROC)]
    Search {
        /// What to search for (several words are joined)
        #[arg(required = true)]
        query: Vec<String>,
        /// How many results to ask for
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: usize,
        #[command(flatten)]
        out: Output,
        #[command(flatten)]
        route: Route,
    },
    /// Play a video by URL or ID (tracking parameters are dropped)
    #[command(visible_alias = "p", after_help = IN_PROC)]
    Play {
        /// A YouTube video URL, or its 11-character ID
        target: String,
        #[command(flatten)]
        route: Route,
    },
    /// Read a video's comments
    #[command(after_help = IN_PROC)]
    Comments {
        /// A YouTube video URL, or its 11-character ID
        target: String,
        /// Newest first instead of top first
        #[arg(long)]
        new: bool,
        /// How many comments to ask for (20 to 200)
        #[arg(short = 'n', long, default_value_t = 40)]
        limit: usize,
        /// Also fetch up to three replies under each comment (slower)
        #[arg(long)]
        replies: bool,
        /// Print the comments as JSON
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        route: Route,
    },
    /// Latest uploads from your subscriptions
    #[command(visible_alias = "f")]
    Feed {
        /// How many videos to show
        #[arg(short = 'n', long, default_value_t = 80)]
        limit: usize,
        /// Hide Shorts
        #[arg(long)]
        no_shorts: bool,
        /// Don't fetch anything; show what's cached
        #[arg(long)]
        offline: bool,
        /// Fetch every channel, including ones fetched in the last 15 minutes
        #[arg(long, conflicts_with = "offline")]
        force: bool,
        #[command(flatten)]
        out: Output,
        #[command(flatten)]
        route: Route,
    },
    /// Manage subscriptions (stored only on this machine)
    #[command(subcommand)]
    Sub(SubCmd),
    /// Feeds you describe in words (stored only on this machine)
    #[command(subcommand)]
    Lens(LensCmd),
    /// Show how a search or a lens would be read, without asking YouTube
    #[command(after_help = IN_PROC)]
    Read {
        /// The words to read (several are joined)
        #[arg(required = true)]
        text: Vec<String>,
    },
    /// Show or clear the local watch history (off unless `history = true`)
    History {
        /// Delete the watch history instead of showing it
        #[arg(long)]
        clear: bool,
        /// Print the history as JSON
        #[arg(long)]
        json: bool,
    },
    /// Show or forget where unfinished videos were stopped
    Resume {
        /// Forget every position instead of showing them
        #[arg(long)]
        clear: bool,
        /// Print the positions as JSON
        #[arg(long)]
        json: bool,
    },
    /// Show file locations, effective settings and what is exposed
    Config,
    /// What qveil does, and doesn't, about your privacy on the configured route
    Privacy {
        /// Print the README's privacy section instead (Markdown)
        #[arg(long)]
        markdown: bool,
    },
    /// Delete all local qveil data: subscriptions, history and caches
    Wipe {
        /// Don't ask for confirmation
        #[arg(long)]
        yes: bool,
    },
    /// Run the loopback server for the desktop app; prints its base URL as
    /// JSON and runs until stdin closes
    #[command(hide = true)]
    Serve {
        // Not used: they are here so that `run` can refuse them with a reason.
        #[arg(long, hide = true)]
        proxy: Option<String>,
        #[arg(long, hide = true)]
        tor: bool,
    },
}

#[derive(Args)]
struct Output {
    /// Print results as JSON instead of opening the picker
    #[arg(long, conflicts_with = "list")]
    json: bool,
    /// Print results as text instead of opening the picker
    #[arg(long)]
    list: bool,
}

#[derive(Subcommand)]
enum LensCmd {
    /// Add a lens: what it should be about, and any rules ("over 20 min", "no music")
    #[command(after_help = IN_PROC)]
    Add {
        /// The lens, in words (several are joined)
        #[arg(required = true)]
        text: Vec<String>,
    },
    /// List the lenses
    #[command(visible_alias = "ls")]
    List {
        /// Print the lenses as JSON
        #[arg(long)]
        json: bool,
    },
    /// Show a lens's videos, as last fetched
    Show {
        /// The lens: its number in `qveil lens ls`, or its ID
        which: String,
        /// Run its searches again first (one YouTube search per topic)
        #[arg(long)]
        refresh: bool,
        #[command(flatten)]
        out: Output,
        #[command(flatten)]
        route: Route,
    },
    /// Remove a lens
    Rm {
        /// The lens: its number in `qveil lens ls`, or its ID
        which: String,
    },
}

#[derive(Subcommand)]
enum SubCmd {
    /// Subscribe: @handle, channel URL or UC… ID
    #[command(after_help = IN_PROC)]
    Add {
        /// The channels: @handle, channel URL or UC… ID
        #[arg(required = true)]
        channels: Vec<String>,
        #[command(flatten)]
        route: Route,
    },
    /// Unsubscribe by channel ID or name
    Rm {
        /// The channel's ID or name, as `qveil sub list` shows it
        channel: String,
    },
    /// List subscriptions
    #[command(visible_alias = "ls")]
    List {
        /// Print the subscriptions as JSON
        #[arg(long)]
        json: bool,
    },
    /// Import from a Google Takeout subscriptions.csv or a NewPipe/FreeTube export (offline)
    Import {
        /// The file to read
        file: PathBuf,
    },
    /// Export as NewPipe-compatible JSON on stdout
    Export,
}

/// The reader of our stdout hung up before we had written everything, as
/// `head` does after its first line. It has what it wanted, so this isn't a
/// failure, and it is no reason to print anything either.
#[derive(Debug)]
struct Hangup;

impl std::fmt::Display for Hangup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the reader of our output went away")
    }
}

impl std::error::Error for Hangup {}

fn hung_up(e: &std::io::Error) -> anyhow::Error {
    if e.kind() == std::io::ErrorKind::BrokenPipe {
        Hangup.into()
    } else {
        anyhow::Error::new(std::io::Error::new(e.kind(), e.to_string()))
    }
}

/// Writes to stdout (see `outln!`).
fn emit(args: std::fmt::Arguments<'_>) -> Result<()> {
    std::io::stdout()
        .lock()
        .write_fmt(args)
        .map_err(|e| hung_up(&e))
}

fn flush() -> Result<()> {
    std::io::stdout().flush().map_err(|e| hung_up(&e))
}

fn main() -> ExitCode {
    // The process guard (see `proc::guard`) is this same binary, started by
    // the engine; it has no command line of its own to parse.
    if std::env::args_os()
        .nth(1)
        .is_some_and(|a| a == proc::guard::ARG)
    {
        proc::guard::main();
        return ExitCode::SUCCESS;
    }
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) if e.downcast_ref::<Hangup>().is_some() => ExitCode::SUCCESS,
        Err(e) => {
            note!("qveil: {}", friendly(&format!("{e:#}")));
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    // The app's route is a setting it can change at any time. A flag given
    // here would be lost the first time it did, and the app would carry on
    // over a route nobody chose.
    if let Cmd::Serve { proxy, tor } = &cli.cmd
        && (proxy.is_some() || *tor)
    {
        anyhow::bail!(
            "`serve` doesn't take --proxy or --tor: the app's route is a setting, saved in its config file"
        );
    }
    // Nothing about the machine is needed to print it, so this works anywhere
    // (a build machine regenerating the README, say).
    if let Cmd::Privacy { markdown: true } = &cli.cmd {
        outln!("{}", privacy::markdown());
        return Ok(());
    }
    let route = match &cli.cmd {
        Cmd::Search { route, .. }
        | Cmd::Play { route, .. }
        | Cmd::Feed { route, .. }
        | Cmd::Comments { route, .. }
        | Cmd::Lens(LensCmd::Show { route, .. })
        | Cmd::Sub(SubCmd::Add { route, .. }) => route.clone(),
        _ => Route::default(),
    };
    let paths = Paths::from_env()?;
    store::secure(&paths);
    let cfg = Config::load(&paths.config)?;
    let rt = Runtime::new(cfg, &paths, route.proxy, route.tor)?;
    let server = Server::new(rt, paths);
    let rt = server.runtime();
    let paths = &server.paths;
    // From here on yt-dlp processes may be started, and a signal must not
    // leave them (and the files with the proxy's password) behind.
    let stopping = Arc::clone(&server);
    if let Err(e) = signals::on_stop(move || stopping.shutdown()) {
        note!("qveil: couldn't watch for Ctrl+C and the like: {e}");
    }

    match cli.cmd {
        Cmd::Serve { .. } => serve(&server),
        Cmd::Search {
            query, limit, out, ..
        } => {
            let query = query.join(" ");
            // A query with rules in it ("talks over 30 min") is read the way
            // the window's search box reads it; any other is sent as typed.
            let reading = lens::read::read(&query, app::now());
            if reading.has_rules() && !reading.topics.is_empty() {
                let found = app::smart_search(&rt, paths, &reading, limit, &|| true)?;
                if out.json {
                    return print_json(&found);
                }
                let rows = found.into_iter().map(|r| (r.video, None)).collect();
                return browse(&server, rows, out.list);
            }
            let videos = app::search(&rt, &query, limit)?;
            if out.json {
                return print_json(&videos);
            }
            browse(
                &server,
                videos.into_iter().map(|v| (v, None)).collect(),
                out.list,
            )
        }
        Cmd::Play { target, .. } => {
            let v = ids::parse_video(&target)
                .with_context(|| format!("{target:?} isn't a YouTube video URL or ID"))?;
            player::play(&rt.cfg, &server.open(&v)?)
        }
        Cmd::Feed {
            limit,
            no_shorts,
            offline,
            force,
            out,
            ..
        } => {
            if !offline {
                note!("fetching feeds (proxy: {})…", rt.net.describe());
            }
            let refresh = match (offline, force) {
                (true, _) => app::Refresh::Off,
                (false, true) => app::Refresh::Force,
                (false, false) => app::Refresh::Stale,
            };
            let r = app::feed(&rt, paths, refresh, no_shorts, limit, &|| true)?;
            if r.fresh > 0 {
                note!(
                    "{} channel(s) were fetched in the last {} minutes and are not fetched again (--force fetches them all)",
                    r.fresh,
                    feed::FRESH_SECS / 60
                );
            }
            if r.channels == 0 {
                anyhow::bail!(
                    "no subscriptions yet: try `qveil sub add @handle` or `qveil sub import subscriptions.csv`"
                );
            }
            if r.fallbacks > 0 {
                note!(
                    "note: RSS was unavailable for {} channel(s); dates marked ~ are approximate",
                    r.fallbacks
                );
            }
            for e in &r.errors {
                note!("warning: {e}");
            }
            if out.json {
                return print_json(&r.entries);
            }
            let rows = r
                .entries
                .into_iter()
                .map(|e| (e.video, Some((e.date, e.estimated))))
                .collect();
            browse(&server, rows, out.list)
        }
        Cmd::Comments {
            target,
            new,
            limit,
            replies,
            json,
            ..
        } => {
            let v = ids::parse_video(&target)
                .with_context(|| format!("{target:?} isn't a YouTube video URL or ID"))?;
            let sort = if new {
                comments::Sort::New
            } else {
                comments::Sort::Top
            };
            let found =
                rt.yt
                    .comments(&v.watch_url(), sort, limit.clamp(20, 200), replies, &|| {
                        false
                    })?;
            if json {
                return print_json(&found);
            }
            if found.comments.is_empty() {
                note!("no comments, or they are turned off");
            }
            for c in &found.comments {
                print_comment(c, "")?;
                for reply in &c.replies {
                    print_comment(reply, "  ")?;
                }
                outln!("");
            }
            Ok(())
        }
        Cmd::Sub(cmd) => sub(&rt, paths, cmd),
        Cmd::Lens(cmd) => lenses(&server, cmd),
        Cmd::Read { text } => {
            let reading = lens::read::read(&text.join(" "), app::now());
            for tag in reading.tags() {
                outln!("{}  {}", tag.label, tag.value);
            }
            outln!("searches:");
            for topic in &reading.topics {
                outln!("  {topic}");
            }
            if let Err(e) = reading.check() {
                note!("qveil: {e}");
            }
            Ok(())
        }
        Cmd::History { clear, json } => history(&rt.cfg, paths, clear, json),
        Cmd::Resume { clear, json } => resumed(&rt.cfg, paths, clear, json),
        Cmd::Config => show_config(&rt, paths),
        Cmd::Privacy { .. } => {
            outln!("{}", privacy::text(&rt.net, &rt.cfg));
            Ok(())
        }
        Cmd::Wipe { yes } => wipe(paths, yes),
    }
}

fn serve(server: &Arc<Server>) -> Result<()> {
    let base = server.listen()?;
    outln!("{}", serde_json::json!({ "base": base }));
    flush()?;
    // The app holds our stdin; when it exits, for any reason, so do we. Its
    // yt-dlp processes don't outlive it: they are killed and their files removed.
    let mut buf = [0u8; 256];
    let mut stdin = std::io::stdin();
    while matches!(stdin.read(&mut buf), Ok(n) if n > 0) {}
    server.shutdown();
    Ok(())
}

/// One comment: who and when on a line, then what they wrote.
fn print_comment(c: &comments::Comment, indent: &str) -> Result<()> {
    let mut head = vec![c.author.clone()];
    if c.pinned {
        head.push("pinned".into());
    }
    head.extend(c.time_text.clone());
    if let Some(likes) = c.likes {
        head.push(format!("\u{2665} {}", video::fmt_count(likes)));
    }
    outln!("{indent}{}", head.join("  \u{b7}  "));
    for line in c.text.lines() {
        outln!("{indent}{line}");
    }
    Ok(())
}

fn print_json<T: Serialize>(v: &T) -> Result<()> {
    outln!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

/// Lists videos; on a terminal, loops picker → mpv → picker until cancelled.
fn browse(
    server: &Arc<Server>,
    rows: Vec<(Video, Option<(i64, bool)>)>,
    list_only: bool,
) -> Result<()> {
    if rows.is_empty() {
        note!("nothing found");
        return Ok(());
    }
    let t = app::now();
    let lines: Vec<String> = rows.iter().map(|(v, d)| v.line(*d, t)).collect();
    if list_only || !std::io::stdout().is_terminal() {
        for ((v, _), l) in rows.iter().zip(&lines) {
            outln!("{}  {l}", v.id);
        }
        return Ok(());
    }
    let cfg = &server.runtime().cfg;
    while let Some(i) = pick::pick(&lines)? {
        let r = ids::VideoRef {
            id: rows[i].0.id.clone(),
            start: None,
        };
        if let Err(e) = server.open(&r).and_then(|info| player::play(cfg, &info)) {
            note!("qveil: {}", friendly(&format!("{e:#}")));
        }
    }
    Ok(())
}

fn lenses(server: &Arc<Server>, cmd: LensCmd) -> Result<()> {
    let rt = server.runtime();
    let paths = &server.paths;
    match cmd {
        LensCmd::Add { text } => {
            let lens = lens::add(paths, &text.join(" "), app::now())?;
            outln!("added: {}", lens.text);
        }
        LensCmd::List { json } => {
            let all = lens::load(paths)?;
            if json {
                return print_json(&all);
            }
            if all.is_empty() {
                outln!("no lenses (add one with `qveil lens add <words>`)");
            }
            for (i, l) in all.iter().enumerate() {
                outln!("{}  {}", i + 1, l.text);
            }
        }
        LensCmd::Show {
            which,
            refresh,
            out,
            ..
        } => {
            let lens = lens::find(paths, &which)?;
            let refresh = if refresh {
                app::LensRefresh::Force
            } else {
                app::LensRefresh::Off
            };
            let found = app::lens(&rt, paths, &lens.id, refresh, &|_, _| {}, &|| true)?;
            if out.json {
                return print_json(&found);
            }
            if found.fetched_at.is_none() {
                note!("not fetched yet: `qveil lens show {which} --refresh` runs its searches");
                return Ok(());
            }
            let rows = found.videos.into_iter().map(|r| (r.video, None)).collect();
            return browse(server, rows, out.list);
        }
        LensCmd::Rm { which } => {
            let lens = lens::find(paths, &which)?;
            lens::remove(paths, &lens.id)?;
            outln!("removed: {}", lens.text);
        }
    }
    Ok(())
}

fn sub(rt: &Runtime, paths: &Paths, cmd: SubCmd) -> Result<()> {
    match cmd {
        SubCmd::Add { channels, .. } => {
            for input in channels {
                let (s, added) = app::add_sub(rt, paths, &input, &|| true)?;
                let verb = if added {
                    "subscribed"
                } else {
                    "already subscribed"
                };
                outln!("{verb}: {} ({})", s.name, s.id);
            }
        }
        SubCmd::Rm { channel } => {
            outln!("unsubscribed: {}", app::remove_sub(paths, &channel)?.name)
        }
        SubCmd::List { json } => {
            let subs = app::load_subs(paths)?;
            if json {
                return print_json(&subs.channels);
            }
            for s in &subs.channels {
                outln!("{}  {}", s.id, s.name);
            }
            note!("{} subscription(s)", subs.channels.len());
        }
        SubCmd::Import { file } => {
            let text = std::fs::read_to_string(&file)
                .with_context(|| format!("reading {}", file.display()))?;
            let (added, total) = app::import_subs(paths, &text)?;
            outln!("imported {added} new of {total} channel(s); no network requests were made");
        }
        SubCmd::Export => print_json(&app::load_subs(paths)?.export_newpipe())?,
    }
    Ok(())
}

/// `qveil resume`: where unfinished videos were stopped.
fn resumed(cfg: &Config, paths: &Paths, clear: bool, json: bool) -> Result<()> {
    if clear {
        resume::clear(paths)?;
        outln!("positions forgotten");
        return Ok(());
    }
    let now = app::now();
    let kept = resume::list(paths, cfg.resume_days, now)?;
    if json {
        return print_json(&kept);
    }
    if cfg.resume_days == 0 {
        outln!(
            "resume is off (set `resume_days` in {})",
            paths.config.display()
        );
        return Ok(());
    }
    if kept.is_empty() {
        outln!("nothing kept");
        return Ok(());
    }
    for p in &kept {
        let mut parts = vec![p.title.replace(['\t', '\n'], " ")];
        parts.extend(p.channel.clone());
        parts.push(format!(
            "{} of {}",
            video::fmt_duration(p.pos),
            video::fmt_duration(p.dur)
        ));
        // Rounded up: a position with an hour to go still has "1 day left".
        let days = (resume::expires(p, cfg.resume_days) - now + 86_399) / 86_400;
        parts.push(if days == 1 {
            "1 day left".into()
        } else {
            format!("{days} days left")
        });
        outln!("{}  {}", p.id, parts.join("  \u{b7}  "));
    }
    Ok(())
}

fn history(cfg: &Config, paths: &Paths, clear: bool, json: bool) -> Result<()> {
    if clear {
        app::clear_history(paths)?;
        outln!("history cleared");
        return Ok(());
    }
    let mut watched = app::load_history(paths)?.watched;
    watched.reverse();
    if json {
        return print_json(&watched);
    }
    if watched.is_empty() && !cfg.history {
        outln!(
            "history is off (set `history = true` in {})",
            paths.config.display()
        );
        return Ok(());
    }
    let t = app::now();
    for w in &watched {
        outln!("{}  {}  ·  {}", w.id, w.title, video::fmt_age(w.at, t));
    }
    Ok(())
}

fn show_config(rt: &Runtime, p: &Paths) -> Result<()> {
    let exists = |path: &std::path::Path| if path.exists() { "" } else { " (not created)" };
    outln!("config         {}{}", p.config.display(), exists(&p.config));
    outln!(
        "subscriptions  {}{}",
        p.subscriptions().display(),
        exists(&p.subscriptions())
    );
    outln!(
        "history        {}{}",
        p.history().display(),
        exists(&p.history())
    );
    outln!(
        "resume         {}{}",
        p.resume().display(),
        exists(&p.resume())
    );
    outln!("cache          {}{}", p.cache.display(), exists(&p.cache));
    // Migration couldn't merge these (both names had the same file), so they
    // are still there, and still hold subscriptions or settings.
    let legacy = p.legacy();
    if !legacy.is_empty() {
        let paths: Vec<_> = legacy.iter().map(|d| d.display().to_string()).collect();
        outln!(
            "left over from veil: {} \u{2014} `qveil wipe` removes them",
            paths.join(", ")
        );
    }
    outln!("");
    outln!("proxy          {}", rt.net.describe());
    outln!(
        "history        {}",
        if rt.cfg.history {
            "on (local only)"
        } else {
            "off"
        }
    );
    outln!(
        "resume         {}",
        match rt.cfg.resume_days {
            0 => "off".to_string(),
            1 => "1 day, then deleted (local only)".to_string(),
            n => format!("{n} days, then deleted (local only)"),
        }
    );
    outln!("max height     {}p", rt.cfg.max_height);
    outln!("");
    // What the window says too, from the same place (see `privacy`).
    outln!("{}", privacy::text(&rt.net, &rt.cfg));
    Ok(())
}

fn wipe(paths: &Paths, yes: bool) -> Result<()> {
    let present: Vec<_> = [&paths.data, &paths.cache]
        .into_iter()
        .filter(|p| p.exists())
        .cloned()
        .collect();
    // What the old name left behind, which holds the same kind of data.
    let legacy = paths.legacy();
    if present.is_empty() && legacy.is_empty() {
        outln!("nothing to wipe");
        return Ok(());
    }
    if !yes {
        outln!("this permanently deletes:");
        for p in &present {
            outln!("  {}", p.display());
        }
        for p in &legacy {
            outln!(
                "  {}  (left over from when qveil was called veil)",
                p.display()
            );
        }
        out!("type 'wipe' to confirm: ");
        flush()?;
        let mut answer = String::new();
        std::io::stdin().lock().read_line(&mut answer)?;
        if answer.trim() != "wipe" {
            anyhow::bail!("aborted");
        }
    }
    for p in present.iter().chain(&legacy) {
        std::fs::remove_dir_all(p).with_context(|| format!("removing {}", p.display()))?;
    }
    outln!("wiped (config file kept: {})", paths.config.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    /// Every command below `cmd`, with its path (`sub add`).
    fn commands(cmd: &clap::Command, path: &str, out: &mut Vec<(String, clap::Command)>) {
        for sub in cmd.get_subcommands().filter(|c| c.get_name() != "help") {
            let path = format!("{path} {}", sub.get_name());
            out.push((path.trim().to_string(), sub.clone()));
            commands(sub, &path, out);
        }
    }

    fn all_commands() -> Vec<(String, clap::Command)> {
        let root = Cli::command();
        let mut all = vec![(String::new(), root.clone())];
        commands(&root, "", &mut all);
        all
    }

    #[test]
    fn no_entry_in_the_help_is_blank() {
        for (path, cmd) in all_commands() {
            if cmd.is_hide_set() {
                continue;
            }
            assert!(
                path.is_empty() || cmd.get_about().is_some(),
                "`{path}` has no description"
            );
            for arg in cmd.get_arguments() {
                let id = arg.get_id().as_str();
                if arg.is_hide_set() || id == "help" || id == "version" {
                    continue;
                }
                assert!(
                    arg.get_help()
                        .is_some_and(|h| !h.to_string().trim().is_empty()),
                    "`qveil {path}`: {id} has no help text"
                );
            }
        }
    }

    #[test]
    fn only_the_commands_that_use_the_network_take_a_route() {
        let takes = |cmd: &clap::Command| {
            cmd.get_arguments()
                .any(|a| matches!(a.get_id().as_str(), "proxy" | "tor") && !a.is_hide_set())
        };
        for (path, cmd) in all_commands() {
            // A command with subcommands only shows what it holds itself.
            if cmd.has_subcommands() {
                assert!(!takes(&cmd), "`qveil {path}` shows a route it doesn't use");
                continue;
            }
            let uses_network = ["search", "play", "feed", "comments", "sub add", "lens show"]
                .contains(&path.as_str());
            assert_eq!(takes(&cmd), uses_network, "`qveil {path}`");
        }
        // They can't be given both, and the others don't take either.
        assert!(Cli::try_parse_from(["qveil", "feed", "--tor", "--proxy", "http://x:1"]).is_err());
        assert!(Cli::try_parse_from(["qveil", "config", "--tor"]).is_err());
        assert!(Cli::try_parse_from(["qveil", "--tor", "feed"]).is_err());
        assert!(Cli::try_parse_from(["qveil", "sub", "add", "--tor", "@x"]).is_ok());
    }

    #[test]
    fn the_commands_that_put_input_on_the_command_line_say_who_can_see_it() {
        for (path, cmd) in all_commands() {
            let says = cmd
                .get_after_help()
                .is_some_and(|h| h.to_string().contains("/proc"));
            let takes_input = ["search", "play", "comments", "sub add", "lens add", "read"]
                .contains(&path.as_str());
            assert_eq!(says, takes_input, "`qveil {path}`");
        }
    }
}
