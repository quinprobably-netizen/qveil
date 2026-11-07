# veil

A private, account-free YouTube frontend for the terminal. Search, subscribe
and watch from a fuzzy picker straight into [mpv], with no Google account,
no cookies and no browser, and an optional Tor route for every byte.

> **Prototype.** Works day to day, but expect rough edges. Not affiliated with
> YouTube or Google.

```
$ veil sub add @Fireship @NoBoilerplate
$ veil feed            # latest uploads → fzf → mpv
$ veil search rust in 100 seconds
$ veil play 'https://youtu.be/5C_HPTJg5ek?si=tracking&t=30'   # si= dropped, t= kept
$ veil --tor feed      # same thing, every request on its own Tor circuit
```

## Privacy model

veil can't make you invisible to YouTube: it still has to ask YouTube for
videos. The goal is to give YouTube as little as possible to link together,
and to keep everything else on your machine.

| | What veil does |
|---|---|
| **Account** | Never signs in. Subscriptions are a local list of channel IDs. |
| **Cookies** | Never stored or sent. yt-dlp runs with `--ignore-config --no-cookies --no-cookies-from-browser`, so a cookie line in your yt-dlp config can't leak in, and its cache is separate from any logged-in yt-dlp use. |
| **Subscription list** | Never sent as a whole. Each channel is fetched separately from the public RSS feed, in random order. With `--tor`, each fetch uses a different circuit, so YouTube sees unrelated exit IPs. |
| **Tracking parameters** | Every URL is reduced to its video ID; `si=`, `pp=`, `feature=` and similar are dropped before any request. |
| **Headers** | Feed requests send a common browser User-Agent and no Referer. |
| **IP address** | Visible to YouTube unless you use a proxy. `--proxy` / `--tor` routes search, feeds, metadata **and video bytes**: with SOCKS, yt-dlp streams the video through the proxy into mpv, so nothing goes around it. `socks5://` is refused because it resolves DNS locally; use `socks5h://`. |
| **Player state** | mpv runs with `--save-position-on-quit=no --save-watch-history=no`. |
| **Local data** | Files are created `0600` in `0700` directories. Watch history is **off** by default. `veil wipe` deletes all of it. |
| **Telemetry** | None. veil only talks to YouTube, through yt-dlp or the RSS endpoint. |

**What YouTube can still see:** the IP you connect from (unless you use a
proxy), each search and video you play, and the timing pattern of your
requests. Even over Tor, a long session of related videos can be correlated
by content. veil reduces linkability; it doesn't eliminate it.

`veil config` prints where your data lives and what is exposed with your
current settings.

## Install

Needs [Rust], [yt-dlp] (kept up to date: it follows YouTube's changes), a JS
runtime for yt-dlp ([deno] recommended), [mpv], ffmpeg, and optionally [fzf]
for the picker (without it you get a numbered list).

```sh
cargo install --git https://github.com/quinprobably-netizen/veil
```

For Tor, run the `tor` service. veil uses `127.0.0.1:9050`.

## Usage

```
veil search <query…>   [-n 20] [--list|--json]
veil feed              [-n 80] [--no-shorts] [--offline] [--list|--json]
veil play <url|id>
veil sub add <@handle|channel URL|UC…>…
veil sub rm <id|name>
veil sub list
veil sub import <subscriptions.csv | newpipe.json | freetube.db>
veil sub export        > newpipe.json
veil history [--clear]
veil config
veil wipe [--yes]
```

Global flags: `--proxy <url>`, `--tor`.

`--json` output is stable enough to build a GUI on, so a desktop shell or
launcher can sit on top of veil.

**Importing:** Google Takeout's `subscriptions.csv`, NewPipe's export and
FreeTube's `subscriptions.db` are parsed offline. Importing makes no network
requests.

## Configuration

Optional: `~/.config/veil/config.toml`. See
[`config.example.toml`](config.example.toml). Unknown keys are an error, so
a typo such as `histroy = true` can't silently do nothing.

## How it works

- **Feeds:** YouTube's public RSS feed for each channel. That endpoint
  sometimes returns 404/500, so veil retries once and then falls back to the
  channel's Videos tab via yt-dlp with approximate dates (shown with `~`). A
  channel's back catalogue is never shown as new.
- **Search, channel lookup and stream URLs:** [yt-dlp]. YouTube's private API,
  signature and n-parameter deciphering, and PO tokens change constantly.
  Relying on yt-dlp means veil gets its fixes by updating one package.
- **Playback:** direct stream URLs handed to mpv. Through a SOCKS proxy,
  yt-dlp downloads and pipes into mpv instead, because mpv can't use SOCKS
  and the stream URLs are tied to the IP that requested them.

```
src/
  ids.rs     URL → video/channel ID; strips tracking parameters
  net.rs     proxy validation, per-request Tor circuit isolation, HTTP client
  feed.rs    RSS parsing, fallback, local feed cache and ordering
  ytdlp.rs   isolated yt-dlp invocations
  player.rs  mpv: direct or piped through the proxy
  subs.rs    subscription store, Takeout/NewPipe/FreeTube import, export
  store.rs   XDG paths, atomic 0600 writes
```

## Known limitations

- Seeking is limited in SOCKS/Tor mode (it's a pipe).
- No comments, playlists or thumbnails yet. Thumbnails would be another
  request per video to YouTube's image CDN.
- Feed dates from the fallback path are day-level approximations.

## License

MIT

[mpv]: https://mpv.io
[yt-dlp]: https://github.com/yt-dlp/yt-dlp
[deno]: https://deno.com
[fzf]: https://github.com/junegunn/fzf
[Rust]: https://rustup.rs
