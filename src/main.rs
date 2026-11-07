mod config;
mod feed;
mod ids;
mod net;
mod pick;
mod player;
mod store;
mod subs;
mod video;
mod ytdlp;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use config::Config;
use feed::FeedCache;
use net::Net;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use store::Paths;
use subs::{Sub, Subs};
use video::Video;
use ytdlp::Ytdlp;

#[derive(Parser)]
#[command(
    name = "veil",
    version,
    about = "A private, account-free YouTube frontend for the terminal"
)]
struct Cli {
    /// Route all traffic through this proxy (http://, https://, socks5h://). Overrides the config.
    #[arg(long, global = true, value_name = "URL")]
    proxy: Option<String>,
    /// Use the local Tor SOCKS port (127.0.0.1:9050) with a fresh circuit per request.
    #[arg(long, global = true, conflicts_with = "proxy")]
    tor: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Search for videos and pick one to play
    #[command(visible_alias = "s")]
    Search {
        #[arg(required = true)]
        query: Vec<String>,
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: usize,
        #[command(flatten)]
        out: Output,
    },
    /// Play a video by URL or ID (tracking parameters are dropped)
    #[command(visible_alias = "p")]
    Play { target: String },
    /// Latest uploads from your subscriptions
    #[command(visible_alias = "f")]
    Feed {
        #[arg(short = 'n', long, default_value_t = 80)]
        limit: usize,
        /// Hide Shorts
        #[arg(long)]
        no_shorts: bool,
        /// Don't fetch anything; show what's cached
        #[arg(long)]
        offline: bool,
        #[command(flatten)]
        out: Output,
    },
    /// Manage subscriptions (stored only on this machine)
    #[command(subcommand)]
    Sub(SubCmd),
    /// Show or clear the local watch history (off unless `history = true`)
    History {
        #[arg(long)]
        clear: bool,
    },
    /// Show file locations, effective settings and what is exposed
    Config,
    /// Delete all local veil data: subscriptions, history and caches
    Wipe {
        /// Don't ask for confirmation
        #[arg(long)]
        yes: bool,
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
enum SubCmd {
    /// Subscribe: @handle, channel URL or UC… ID
    Add {
        #[arg(required = true)]
        channels: Vec<String>,
    },
    /// Unsubscribe by channel ID or name
    Rm { channel: String },
    /// List subscriptions
    #[command(visible_alias = "ls")]
    List,
    /// Import from a Google Takeout subscriptions.csv or a NewPipe/FreeTube export (offline)
    Import { file: PathBuf },
    /// Export as NewPipe-compatible JSON on stdout
    Export,
}

#[derive(Default, Serialize, Deserialize)]
struct History {
    watched: Vec<Watched>,
}

#[derive(Serialize, Deserialize)]
struct Watched {
    id: String,
    title: String,
    at: i64,
}

const HISTORY_CAP: usize = 5000;

struct App {
    cfg: Config,
    paths: Paths,
    net: Net,
    yt: Ytdlp,
}

fn now() -> i64 {
    jiff::Timestamp::now().as_second()
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("veil: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let paths = Paths::from_env()?;
    let cfg = Config::load(&paths.config)?;
    let (proxy, isolate) = if cli.tor {
        (Some(net::TOR_PROXY.to_string()), true)
    } else {
        (
            cli.proxy.or_else(|| cfg.proxy.clone()),
            cfg.isolate_circuits,
        )
    };
    let net = Net::new(proxy, isolate)?;
    let yt = Ytdlp {
        bin: cfg.ytdlp.clone(),
        cache_dir: paths.ytdlp_cache(),
        net: net.clone(),
    };
    let app = App {
        cfg,
        paths,
        net,
        yt,
    };

    match cli.cmd {
        Cmd::Search { query, limit, out } => {
            let videos = app.yt.search(&query.join(" "), limit)?;
            if out.json {
                return print_json(&videos);
            }
            let rows = videos.into_iter().map(|v| (v, None)).collect();
            browse(&app, rows, out.list)
        }
        Cmd::Play { target } => {
            let v = ids::parse_video(&target)
                .with_context(|| format!("{target:?} isn't a YouTube video URL or ID"))?;
            play(&app, &v, None)
        }
        Cmd::Feed {
            limit,
            no_shorts,
            offline,
            out,
        } => feed(&app, limit, no_shorts, offline, out),
        Cmd::Sub(cmd) => sub(&app, cmd),
        Cmd::History { clear } => history(&app, clear),
        Cmd::Config => show_config(&app),
        Cmd::Wipe { yes } => wipe(&app, yes),
    }
}

fn print_json<T: Serialize>(v: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

/// Lists videos; on a terminal, loops picker → mpv → picker until cancelled.
fn browse(app: &App, rows: Vec<(Video, Option<(i64, bool)>)>, list_only: bool) -> Result<()> {
    if rows.is_empty() {
        eprintln!("nothing found");
        return Ok(());
    }
    let t = now();
    let lines: Vec<String> = rows.iter().map(|(v, d)| v.line(*d, t)).collect();
    if list_only || !std::io::stdout().is_terminal() {
        for ((v, _), l) in rows.iter().zip(&lines) {
            println!("{}  {l}", v.id);
        }
        return Ok(());
    }
    while let Some(i) = pick::pick(&lines)? {
        let v = &rows[i].0;
        let r = ids::VideoRef {
            id: v.id.clone(),
            start: None,
        };
        if let Err(e) = play(app, &r, Some(&v.title)) {
            eprintln!("veil: {e:#}");
        }
    }
    Ok(())
}

fn play(app: &App, v: &ids::VideoRef, title: Option<&str>) -> Result<()> {
    let title = player::play(&app.cfg, &app.net, &app.yt, v, title)?;
    if app.cfg.history {
        let path = app.paths.history();
        let mut h: History = store::load(&path)?;
        h.watched.push(Watched {
            id: v.id.clone(),
            title,
            at: now(),
        });
        let excess = h.watched.len().saturating_sub(HISTORY_CAP);
        h.watched.drain(..excess);
        store::save(&path, &h)?;
    }
    Ok(())
}

fn feed(app: &App, limit: usize, no_shorts: bool, offline: bool, out: Output) -> Result<()> {
    let subs: Subs = store::load(&app.paths.subscriptions())?;
    if subs.channels.is_empty() {
        bail!(
            "no subscriptions yet: try `veil sub add @handle` or `veil sub import subscriptions.csv`"
        );
    }
    let mut cache: FeedCache = store::load(&app.paths.feed_cache())?;
    if !offline {
        eprintln!(
            "fetching {} channels (proxy: {})…",
            subs.channels.len(),
            app.net.describe()
        );
        let (fetched, errors) =
            feed::fetch_all(&app.net, &app.yt, &subs.channels, app.cfg.feed_workers);
        let fallbacks = fetched
            .iter()
            .filter(|f| f.source == feed::Source::Fallback)
            .count();
        if fallbacks > 0 {
            eprintln!(
                "note: RSS was unavailable for {fallbacks} channel(s); dates marked ~ are approximate"
            );
        }
        for e in &errors {
            eprintln!("warning: {e}");
        }
        cache.merge(&fetched, now());
        store::save(&app.paths.feed_cache(), &cache)?;
    }
    let entries = cache.entries(&subs.channels, no_shorts, limit);
    if out.json {
        return print_json(&entries);
    }
    let rows = entries
        .into_iter()
        .map(|e| (e.video, Some((e.date, e.estimated))))
        .collect();
    browse(app, rows, out.list)
}

fn sub(app: &App, cmd: SubCmd) -> Result<()> {
    let path = app.paths.subscriptions();
    let mut subs: Subs = store::load(&path)?;
    match cmd {
        SubCmd::Add { channels } => {
            for input in channels {
                let r = ids::parse_channel(&input).with_context(|| {
                    format!("{input:?} isn't a channel (@handle, channel URL or UC… ID)")
                })?;
                let (id, name) = app.yt.channel_info(&r.url())?;
                if subs.add(Sub {
                    id: id.clone(),
                    name: name.clone(),
                }) {
                    println!("subscribed: {name} ({id})");
                } else {
                    println!("already subscribed: {name}");
                }
                store::save(&path, &subs)?;
            }
        }
        SubCmd::Rm { channel } => match subs.remove(&channel) {
            Some(s) => {
                store::save(&path, &subs)?;
                println!("unsubscribed: {}", s.name);
            }
            None => bail!("no subscription matches {channel:?}"),
        },
        SubCmd::List => {
            for s in &subs.channels {
                println!("{}  {}", s.id, s.name);
            }
            eprintln!("{} subscription(s)", subs.channels.len());
        }
        SubCmd::Import { file } => {
            let text = std::fs::read_to_string(&file)
                .with_context(|| format!("reading {}", file.display()))?;
            let found = subs::parse_import(&text)?;
            let total = found.len();
            let added = found.into_iter().filter(|s| subs.add(s.clone())).count();
            store::save(&path, &subs)?;
            println!("imported {added} new of {total} channel(s); no network requests were made");
        }
        SubCmd::Export => print_json(&subs.export_newpipe())?,
    }
    Ok(())
}

fn history(app: &App, clear: bool) -> Result<()> {
    let path = app.paths.history();
    if clear {
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        println!("history cleared");
        return Ok(());
    }
    let h: History = store::load(&path)?;
    if h.watched.is_empty() && !app.cfg.history {
        println!(
            "history is off (set `history = true` in {})",
            app.paths.config.display()
        );
        return Ok(());
    }
    let t = now();
    for w in h.watched.iter().rev() {
        println!("{}  {}  ·  {}", w.id, w.title, video::fmt_age(w.at, t));
    }
    Ok(())
}

fn show_config(app: &App) -> Result<()> {
    let p = &app.paths;
    let exists = |path: &std::path::Path| if path.exists() { "" } else { " (not created)" };
    println!("config         {}{}", p.config.display(), exists(&p.config));
    println!(
        "subscriptions  {}{}",
        p.subscriptions().display(),
        exists(&p.subscriptions())
    );
    println!(
        "history        {}{}",
        p.history().display(),
        exists(&p.history())
    );
    println!("cache          {}{}", p.cache.display(), exists(&p.cache));
    println!();
    println!("proxy          {}", app.net.describe());
    println!(
        "history        {}",
        if app.cfg.history {
            "on (local only)"
        } else {
            "off"
        }
    );
    println!("max height     {}p", app.cfg.max_height);
    println!();
    println!("Never sent: cookies, a Google account, your subscription list in one request,");
    println!("tracking URL parameters, Referer headers, or player state (mpv resume/history off).");
    if !app.net.is_socks() {
        println!("Visible to YouTube: your IP, the videos you search and play, and (one request");
        println!("per channel) the feeds you poll. Use --tor or a proxy to hide your IP.");
    }
    Ok(())
}

fn wipe(app: &App, yes: bool) -> Result<()> {
    let targets = [&app.paths.data, &app.paths.cache];
    let present: Vec<_> = targets.iter().filter(|p| p.exists()).collect();
    if present.is_empty() {
        println!("nothing to wipe");
        return Ok(());
    }
    if !yes {
        println!("this permanently deletes:");
        for p in &present {
            println!("  {}", p.display());
        }
        print!("type 'wipe' to confirm: ");
        std::io::stdout().flush()?;
        let mut answer = String::new();
        std::io::stdin().lock().read_line(&mut answer)?;
        if answer.trim() != "wipe" {
            bail!("aborted");
        }
    }
    for p in present {
        std::fs::remove_dir_all(p).with_context(|| format!("removing {}", p.display()))?;
    }
    println!("wiped (config file kept: {})", app.paths.config.display());
    Ok(())
}
