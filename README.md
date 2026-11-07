# veil

A private, account-free YouTube app for Arch Linux and Omarchy. Subscribe,
search and watch in a native window with no Google account, no cookies and no
browser, and optionally route every byte through Tor.

![veil's feed, thumbnails veiled until hovered](docs/screenshot.png)

> **Prototype.** It works day to day, but expect rough edges. Not affiliated
> with YouTube or Google.

## Privacy model

veil can't make you invisible to YouTube: it still has to ask YouTube for
videos. The goal is to give YouTube as little as possible to link together,
and to keep everything else on your machine.

| | What veil does |
|---|---|
| **One way out** | The window and its video player only ever talk to veil's engine on `127.0.0.1`, behind a random token. Every request that leaves your machine (search, feeds, thumbnails and the video bytes themselves) is made by the engine, through the route you pick. |
| **Route** | *Direct*, *Tor*, or your own *Proxy*, always shown in the top bar: amber while YouTube can see your IP. With Tor, every feed, thumbnail and video gets its own circuit, and seeking still works because the engine relays Range requests. A video that outlives its circuit is looked up again on a fresh one. Switching routes ends the streams playing on the old one. `socks5://` is refused (it leaks DNS); use `socks5h://`. |
| **Account and cookies** | Never signed in, and no cookies stored or sent. yt-dlp runs with `--ignore-config --no-cookies --no-cookies-from-browser` and its own cache. Neither the embedded player nor `veil play` loads your mpv config or scripts, and `veil play` hands mpv its loopback URLs in a private temporary file rather than on the command line. |
| **Subscription list** | Fetched one channel per request, in random order. With Tor each request has its own circuit, so they can't be linked by IP, but a refresh is still a burst of requests for exactly your channels, so timing and the set of IDs can still correlate them. |
| **Tracking parameters** | Links are reduced to the video ID; `si=`, `pp=`, `feature=` and similar are dropped. |
| **Player state** | No resume files, no watch history in mpv. |
| **Local data** | Subscriptions and the feed cache live in `~/.local/share/veil` and `~/.cache/veil`, mode `0600`. Watch history is **off** by default. `veil wipe` deletes all of it. |
| **Telemetry** | None. |

**What YouTube can still see:** the IP you connect from (on Direct), each
search and video you play, and the timing of your requests. Even over Tor, a
long session of related videos can be linked by content. The engine's feeds
and thumbnails send a common Firefox User-Agent over a rustls TLS stack, video
bytes carry the Chrome User-Agent and headers yt-dlp resolved the stream with,
and yt-dlp makes its own requests with its own client. None of that is a real
browser's TLS handshake, so a careful observer could fingerprint it as not a
browser. veil reduces linkability; it doesn't eliminate it.

## The app

- **Feed**: latest uploads from your subscriptions. **Search** and **channel**
  pages, **Subscriptions**, and an optional **History**.
- Video plays inside the window (libmpv), with fullscreen.
- **Veiled thumbnails**: thumbnails stay blurred and muted until you hover or
  select them, so a wall of clickbait doesn't shout at you. You can switch
  this off, or not load thumbnails at all.
- **Live streams aren't supported yet.** veil relays one plain download per
  video, and a live stream (or a video offered only as HLS/DASH) is a playlist
  whose segments the player would fetch itself, around your route. veil says
  so instead of playing it.
- **Moving from YouTube**: drop a Google Takeout `subscriptions.csv`, or a
  NewPipe or FreeTube export, anywhere on the window. It's read offline.
- Follows the Omarchy theme accent, window rounding and language, live.

| Key | Action |
|---|---|
| `/` | Search |
| `1` `2` `3` | Feed, subscriptions, history |
| Arrows, `Enter` | Move and play |
| `R` | Refresh |
| `Space` | Play or pause |
| `←` `→` / `J` `L` | Back or forward 5 / 10 seconds |
| `↑` `↓` | Volume |
| `M` | Mute |
| `F` | Fullscreen |
| `Esc` | Back |
| `?` | All shortcuts |
| `Q` | Quit |

## Install

On Arch / Omarchy:

```sh
git clone https://github.com/quinprobably-netizen/veil
cd veil
bin/install          # builds and installs the package; veil appears in the launcher
```

veil is only packaged for Arch and Omarchy: `pkgbuild/` is a PKGBUILD that
builds from your checkout with makepkg. It pulls in Qt 6, mpv (libmpv), yt-dlp
(keep it updated: it follows YouTube's changes) and [deno] (yt-dlp needs a
JavaScript runtime for YouTube), and Rust is needed to build. For the Tor
route, install `tor` and run `sudo systemctl enable --now tor`.

Settings are in `~/.config/veil/config.toml`; a commented example is installed
as `/usr/share/doc/veil/config.example.toml`.

## Command line

`veil` with a command runs the engine directly, so scripts and agents can use
it without a display (`veil` alone, or `veil open`, opens the window):

```
veil feed --json
veil search rust in 100 seconds --json
veil sub add @handle
veil sub import subscriptions.csv
veil sub export > newpipe.json
veil play <url>            # plays in a separate mpv window
veil --tor feed --list
veil config                # where data lives and what is exposed
veil wipe
```

## How it's built

```
core/        veil-core, the engine (Rust)
  server.rs    loopback API, thumbnail fetcher, Range-forwarding video relay
  net.rs       route validation, per-request Tor circuit isolation
  feed.rs      per-channel RSS with a yt-dlp fallback when RSS is down
  ytdlp.rs     isolated yt-dlp calls: search, channels, stream resolution
  ids.rs       URL → ID, dropping tracking parameters
src/         the window (C++ / Qt Quick)
  backend.*    starts veil-core and talks to it over loopback
  mpvitem.*    libmpv rendered into the scene
  *.qml        the interface
```

YouTube's private API, signature deciphering and PO tokens change
constantly. veil leaves that to yt-dlp, so a fix there is a package update
away. YouTube's RSS endpoint is often down; the feed then falls back to the
channel's Videos tab through yt-dlp, with approximate dates (shown with `~`).
That fallback costs one yt-dlp call per affected channel, so a refresh while
RSS is down is slow, most of all over Tor.

```sh
bin/build    # engine + app into build/
bin/test     # Rust and Qt tests
```

## License

MIT

[deno]: https://deno.com
