# qveil

A private, account-free YouTube app for Arch Linux and Omarchy. Subscribe,
search and watch in a native window with no Google account, no cookies and no
browser, and optionally route every byte through Tor.

![qveil's feed, thumbnails veiled until hovered](docs/screenshot.png)

> **Prototype.** It works day to day, but expect rough edges. Not affiliated
> with YouTube or Google.

## Privacy model

qveil can't make you invisible to YouTube: it still has to ask YouTube for
videos. The goal is to give YouTube as little as possible to link together,
and to keep everything else on your machine.

| | What qveil does |
|---|---|
| **One way out** | The window and its video player only ever talk to qveil's engine on `127.0.0.1`, behind a random token. Every request that leaves your machine (search, feeds, thumbnails and the video bytes themselves) is made by the engine, through the route you pick. |
| **Route** | *Direct*, *Tor*, or your own *Proxy*, always shown in the top bar: amber while YouTube can see your IP. *Direct* means direct: qveil ignores `HTTP_PROXY`, `HTTPS_PROXY` and `ALL_PROXY` from the environment, so the amber bar is the truth. With Tor, every feed, thumbnail and video gets its own circuit (always: `isolate_circuits = false` is refused with Tor), and seeking still works because the engine relays Range requests. A video that outlives its circuit is looked up again on a fresh one. Switching routes ends the streams playing on the old one, and a feed refresh that is running starts no further channels. `socks5://` is refused (it leaks DNS); use `socks5h://`. |
| **Account and cookies** | Never signed in, and no cookies stored or sent. yt-dlp runs with `--ignore-config --no-cookies --no-cookies-from-browser --no-plugin-dirs` and its own cache, so neither your config nor a plugin can add cookies or a route of its own. Neither the embedded player nor `qveil play` loads your mpv config or scripts. |
| **Command lines** | Anything on a command line can be read by every local user through `/proc`. So yt-dlp gets its proxy (and password), your searches and the video you open from a private file (mode `0600`, in a fresh `0700` directory that is removed afterwards), and `qveil play` hands mpv its loopback URLs the same way. |
| **Subscription list** | Fetched one channel per request, in random order. With Tor each request has its own circuit, so they can't be linked by IP, but a refresh is still a burst of requests for exactly your channels, so timing and the set of IDs can still correlate them. A refresh skips channels fetched in the last 15 minutes (`qveil feed --force` fetches them all), which keeps pressing refresh from repeating that burst. |
| **Tracking parameters** | Links are reduced to the video ID; `si=`, `pp=`, `feature=` and similar are dropped. |
| **Player state** | No resume files, no watch history in mpv. |
| **Local data** | Subscriptions and the feed cache live in `~/.local/share/qveil` and `~/.cache/qveil`, settings in `~/.config/qveil`. The directories are mode `0700` and the files `0600`, and qveil tightens them at startup, because the window creates them first with looser defaults. Watch history is **off** by default. `qveil wipe` deletes the data and caches, and anything left over from veil's directories. |
| **Telemetry** | None. |

**What YouTube can still see:** the IP you connect from (on Direct), each
search and video you play, and the timing of your requests. Even over Tor, a
long session of related videos can be linked by content. The engine's feeds
and thumbnails send a common Firefox User-Agent over a rustls TLS stack, video
bytes carry the Chrome User-Agent and headers yt-dlp resolved the stream with,
and yt-dlp makes its own requests with its own client. None of that is a real
browser's TLS handshake, so a careful observer could fingerprint it as not a
browser. qveil reduces linkability; it doesn't eliminate it.

## The app

- **Feed**: latest uploads from your subscriptions. **Search** and **channel**
  pages, **Subscriptions**, and an optional **History**.
- Video plays inside the window (libmpv), with fullscreen.
- **Veiled thumbnails**: thumbnails stay blurred and muted until you hover or
  select them, so a wall of clickbait doesn't shout at you. You can switch
  this off, or not load thumbnails at all.
- **Live streams aren't supported yet.** qveil relays one plain download per
  video, and a live stream (or a video offered only as HLS/DASH) is a playlist
  whose segments the player would fetch itself, around your route. qveil says
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
git clone https://github.com/quinprobably-netizen/qveil
cd qveil
bin/install          # builds and installs the package; qveil appears in the launcher
```

qveil is only packaged for Arch and Omarchy: `pkgbuild/` is a PKGBUILD that
builds from your checkout with makepkg. It pulls in Qt 6, mpv (libmpv), yt-dlp
(keep it updated: it follows YouTube's changes) and [deno] (yt-dlp needs a
JavaScript runtime for YouTube), and Rust is needed to build. For the Tor
route, install it with `sudo pacman -S tor` and run
`sudo systemctl enable --now tor`.

**Upgrading from veil.** qveil is veil under a new name. pacman asks whether to
replace `veil` (the two conflict): answer `y`. Your subscriptions, history and
settings move to the new locations on first launch.

Settings are in `~/.config/qveil/config.toml`; a commented example is installed
as `/usr/share/doc/qveil/config.example.toml`.

## Command line

`qveil` with a command runs the engine directly, so scripts and agents can use
it without a display (`qveil` alone, or `qveil open`, opens the window):

```
qveil feed --json           # refreshes channels not fetched in 15 minutes
qveil feed --force --json   # refreshes every channel
qveil search rust in 100 seconds --json
qveil sub add @handle
qveil sub import subscriptions.csv
qveil sub export > newpipe.json
qveil play <url>            # plays in a separate mpv window
qveil --tor feed --list
qveil config                # where data lives and what is exposed
qveil wipe
```

## How it's built

```
core/        qveil-core, the engine (Rust)
  server.rs    loopback API, thumbnail fetcher, Range-forwarding video relay
  net.rs       route validation, per-request Tor circuit isolation
  feed.rs      per-channel RSS with a yt-dlp fallback when RSS is down
  ytdlp.rs     isolated yt-dlp calls: search, channels, stream resolution
  private.rs   private scratch directories, so secrets stay off command lines
  ids.rs       URL → ID, dropping tracking parameters
src/         the window (C++ / Qt Quick)
  backend.*    starts qveil-core and talks to it over loopback
  mpvitem.*    libmpv rendered into the scene
  *.qml        the interface
```

YouTube's private API, signature deciphering and PO tokens change
constantly. qveil leaves that to yt-dlp, so a fix there is a package update
away. YouTube's RSS endpoint is often down; the feed then falls back to the
channel's Videos tab through yt-dlp, with approximate dates (shown with `~`).
That fallback costs one yt-dlp call per affected channel, so a refresh while
RSS is down is slow, most of all over Tor.

```sh
bin/build    # engine + app into build/
bin/test     # Rust and Qt tests
```

## License

MIT. The icons in `src/icons` are Material Design icon paths (Copyright Google
LLC), used under the [Apache License 2.0](https://www.apache.org/licenses/LICENSE-2.0).

[deno]: https://deno.com
