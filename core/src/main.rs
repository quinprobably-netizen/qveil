//! veil-core: the engine behind the veil desktop app, and a command line of
//! its own. `veil-core serve` runs the loopback server the app talks to.

mod app;
mod config;
mod feed;
mod ids;
mod net;
mod pick;
mod player;
mod server;
mod store;
mod subs;
mod video;
mod ytdlp;

use anyhow::{Context, Result};
use app::Runtime;
use clap::{Args, Parser, Subcommand};
use config::Config;
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
    name = "veil",
    version,
    about = "A private, account-free YouTube frontend"
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
        #[arg(long)]
        json: bool,
    },
    /// Show file locations, effective settings and what is exposed
    Config,
    /// Delete all local veil data: subscriptions, history and caches
    Wipe {
        /// Don't ask for confirmation
        #[arg(long)]
        yes: bool,
    },
    /// Run the loopback server for the desktop app; prints its base URL as
    /// JSON and runs until stdin closes
    #[command(hide = true)]
    Serve,
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
    List {
        #[arg(long)]
        json: bool,
    },
    /// Import from a Google Takeout subscriptions.csv or a NewPipe/FreeTube export (offline)
    Import { file: PathBuf },
    /// Export as NewPipe-compatible JSON on stdout
    Export,
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
    let rt = Runtime::new(cfg, &paths, cli.proxy, cli.tor)?;
    let server = Server::new(rt, paths);
    let rt = server.runtime();
    let paths = &server.paths;

    match cli.cmd {
        Cmd::Serve => serve(&server),
        Cmd::Search { query, limit, out } => {
            let videos = app::search(&rt, &query.join(" "), limit)?;
            if out.json {
                return print_json(&videos);
            }
            browse(
                &server,
                videos.into_iter().map(|v| (v, None)).collect(),
                out.list,
            )
        }
        Cmd::Play { target } => {
            let v = ids::parse_video(&target)
                .with_context(|| format!("{target:?} isn't a YouTube video URL or ID"))?;
            player::play(&rt.cfg, &server.open(&v)?)
        }
        Cmd::Feed {
            limit,
            no_shorts,
            offline,
            out,
        } => {
            if !offline {
                eprintln!("fetching feeds (proxy: {})…", rt.net.describe());
            }
            let r = app::feed(&rt, paths, !offline, no_shorts, limit)?;
            if r.channels == 0 {
                anyhow::bail!(
                    "no subscriptions yet: try `veil sub add @handle` or `veil sub import subscriptions.csv`"
                );
            }
            if r.fallbacks > 0 {
                eprintln!(
                    "note: RSS was unavailable for {} channel(s); dates marked ~ are approximate",
                    r.fallbacks
                );
            }
            for e in &r.errors {
                eprintln!("warning: {e}");
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
        Cmd::Sub(cmd) => sub(&rt, paths, cmd),
        Cmd::History { clear, json } => history(&rt.cfg, paths, clear, json),
        Cmd::Config => show_config(&rt, paths),
        Cmd::Wipe { yes } => wipe(paths, yes),
    }
}

fn serve(server: &Arc<Server>) -> Result<()> {
    let base = server.listen()?;
    let mut out = std::io::stdout().lock();
    writeln!(out, "{}", serde_json::json!({ "base": base }))?;
    out.flush()?;
    drop(out);
    // The app holds our stdin; when it exits, for any reason, so do we.
    let mut buf = [0u8; 256];
    let mut stdin = std::io::stdin();
    while matches!(stdin.read(&mut buf), Ok(n) if n > 0) {}
    Ok(())
}

fn print_json<T: Serialize>(v: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

/// Lists videos; on a terminal, loops picker → mpv → picker until cancelled.
fn browse(
    server: &Arc<Server>,
    rows: Vec<(Video, Option<(i64, bool)>)>,
    list_only: bool,
) -> Result<()> {
    if rows.is_empty() {
        eprintln!("nothing found");
        return Ok(());
    }
    let t = app::now();
    let lines: Vec<String> = rows.iter().map(|(v, d)| v.line(*d, t)).collect();
    if list_only || !std::io::stdout().is_terminal() {
        for ((v, _), l) in rows.iter().zip(&lines) {
            println!("{}  {l}", v.id);
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
            eprintln!("veil: {e:#}");
        }
    }
    Ok(())
}

fn sub(rt: &Runtime, paths: &Paths, cmd: SubCmd) -> Result<()> {
    match cmd {
        SubCmd::Add { channels } => {
            for input in channels {
                let (s, added) = app::add_sub(rt, paths, &input)?;
                let verb = if added {
                    "subscribed"
                } else {
                    "already subscribed"
                };
                println!("{verb}: {} ({})", s.name, s.id);
            }
        }
        SubCmd::Rm { channel } => {
            println!("unsubscribed: {}", app::remove_sub(paths, &channel)?.name)
        }
        SubCmd::List { json } => {
            let subs = app::load_subs(paths)?;
            if json {
                return print_json(&subs.channels);
            }
            for s in &subs.channels {
                println!("{}  {}", s.id, s.name);
            }
            eprintln!("{} subscription(s)", subs.channels.len());
        }
        SubCmd::Import { file } => {
            let text = std::fs::read_to_string(&file)
                .with_context(|| format!("reading {}", file.display()))?;
            let (added, total) = app::import_subs(paths, &text)?;
            println!("imported {added} new of {total} channel(s); no network requests were made");
        }
        SubCmd::Export => print_json(&app::load_subs(paths)?.export_newpipe())?,
    }
    Ok(())
}

fn history(cfg: &Config, paths: &Paths, clear: bool, json: bool) -> Result<()> {
    if clear {
        app::clear_history(paths)?;
        println!("history cleared");
        return Ok(());
    }
    let mut watched = app::load_history(paths)?.watched;
    watched.reverse();
    if json {
        return print_json(&watched);
    }
    if watched.is_empty() && !cfg.history {
        println!(
            "history is off (set `history = true` in {})",
            paths.config.display()
        );
        return Ok(());
    }
    let t = app::now();
    for w in &watched {
        println!("{}  {}  ·  {}", w.id, w.title, video::fmt_age(w.at, t));
    }
    Ok(())
}

fn show_config(rt: &Runtime, p: &Paths) -> Result<()> {
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
    println!("proxy          {}", rt.net.describe());
    println!(
        "history        {}",
        if rt.cfg.history {
            "on (local only)"
        } else {
            "off"
        }
    );
    println!("max height     {}p", rt.cfg.max_height);
    println!();
    println!("Never sent: cookies, a Google account, your subscription list in one request,");
    println!("tracking URL parameters, Referer headers, or player state (mpv resume/history off).");
    if !rt.net.is_socks() {
        println!("Visible to YouTube: your IP, the videos you search and play, and (one request");
        println!("per channel) the feeds you poll. Use --tor or a proxy to hide your IP.");
    }
    Ok(())
}

fn wipe(paths: &Paths, yes: bool) -> Result<()> {
    let present: Vec<_> = [&paths.data, &paths.cache]
        .into_iter()
        .filter(|p| p.exists())
        .collect();
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
            anyhow::bail!("aborted");
        }
    }
    for p in present {
        std::fs::remove_dir_all(p).with_context(|| format!("removing {}", p.display()))?;
    }
    println!("wiped (config file kept: {})", paths.config.display());
    Ok(())
}
