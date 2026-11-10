# qveil overhaul: design

Date: 2026-11-10. Branch: `overhaul`. Target release: 0.4.0 (desktop).

## What was asked for

The owner's list, in their words: picture in picture; remembers video
position ("cookies delete after set amount of time"); comments; video
descriptions; an algorithmic feed "based on user prompts, clean room rip the yt
smart search feature"; "other social media frontends?"; a native mobile app.
Later: "give the ui a full redo as well".

Decisions the owner made when asked:

| Question | Answer |
|---|---|
| What reads the prompts for the feed | Fixed rules. No language model. |
| What "remembers position" means | Saved on disk, deleted after a set time. |
| Mobile | Android, after the desktop release. |
| Other frontends | Parked. |
| Interface direction | Direction A's layout from the stills, without the dye, drawn in the language of the recent panels with the system font. |

Stills of the chosen direction: <https://claude.ai/artifact/Xpv4ujGSAWYU7LRE4HGT1z>.

## Scope and order

1. **Desktop 0.4.0 (this document).** Engine first, then the window, as two
   implementation plans:
   - *Engine:* video details, comments, resume, lenses and smart search, with
     their API, command line and privacy text.
   - *Window:* the full redo, built on that API: new shell, home, watch page,
     lenses page, picture in picture, settings.
2. **Android app.** Its own design document once 0.4.0 is out. yt-dlp needs
   Python on the phone, so the extraction layer is the first question there.
3. **Parked: other frontends.** For the record: sites yt-dlp already handles
   (PeerTube, Odysee, Twitch VODs, Vimeo) could become extra sources in this
   app, at the cost of loosening the engine's YouTube-only IDs. Text and photo
   networks fight scraping and need a different interface; that is a second
   app, not a feature.

Success for 0.4.0: every item above works without an account on Direct and on
Tor, every privacy sentence is still literally true, the tests pass, and the
owner rates the interface 8 or better.

## Principles that stay

- One way out: only the engine (and the yt-dlp it runs) talks to the network,
  through the chosen route. The window talks to the engine on loopback.
- `privacy.rs` words every privacy claim once; the README section is generated
  from it and a test compares them. Every new behaviour below adds or changes a
  sentence there.
- yt-dlp does the fragile work. qveil adds no scraper of its own.
- Nothing fetches in the background that the user didn't ask for by looking.

## Engine

### 1. Video details

The lookup made to play a video already returns its description, counts, date
and chapters; today they are thrown away. `Streams` and `PlayInfo` gain
optional fields:

- `description` (plain text, cut at 20 000 characters),
- `views`, `likes`, `published` (Unix seconds),
- `chapters`: up to 200 of `{ start, title }`, in order.

No new request is made for any of it.

### 2. Comments

`GET api/comments?v=<id>&sort=top|new&n=<20..200, default 40>&replies=0|1`

- Runs yt-dlp on the video with `--write-comments`, `comment_sort` and
  `max_comments` (top-level count, and with `replies=1` up to 3 replies under
  each). The plan picks the lightest invocation that works and checks it once
  against the real site.
- Answer: `{ sort, count, comments: [{ id, author, text, likes, time,
  timeText, pinned, byUploader, replies: [...] }] }`. `time` is approximate
  (YouTube says "3 days ago"); `count` is the video's total when known.
- yt-dlp has no paging: "more" asks again with a larger `n`. Replies are a
  second, slower call, made only when the user turns them on for that video.
- Limit 120 s. The call ends when the window hangs up, like a play lookup.
- Answers are kept in memory for the engine's life (the 16 most recent
  video-and-sort pairs), never on disk.
- Commenters' pictures are never fetched. The window draws an initial.
- New error code for "comments are turned off for this video".

Privacy sentence (every route): *Comments are fetched only when you open them,
as one more lookup of that video; commenters' pictures are never fetched.*

### 3. Resume

A record of where unfinished videos were stopped, on disk, with an expiry.

- File: `~/.local/share/qveil/resume.json`, mode 0600, at most 500 entries:
  `{ id, pos, dur, at, title, channel }`.
- Setting: `resume_days` (config and `resumeDays` in `api/config`), 0 to 365,
  **default 7**. 0 turns resume off and deletes the file at once.
- The window reports the position every 15 s while playing and on pause, seek
  and close: `POST api/resume?v=&pos=&dur=`. The engine writes atomically
  under the store lock.
- Not kept: a position in the first 30 s. Removed: a video stopped in its last
  30 s or past 95 % (it is finished). Expired entries (`at` older than
  `resume_days`) are deleted at engine start and on every write, and are never
  returned.
- `api/play` answers with `resume: <seconds>` when a live entry exists and the
  link carried no `t=`. `GET api/resume` lists entries, newest first, for the
  home page. `POST api/resume/rm?v=` forgets one; `POST api/resume/clear`
  forgets all.
- Command line: `qveil resume [--json] [--clear]`. `qveil wipe` and
  `qveil config` cover the file.

Privacy text: the "Player state" row becomes *mpv keeps no resume files or
history. qveil itself remembers where you stopped in an unfinished video for
N days (7 unless you change it; 0 turns it off) in a file only you can read,
then deletes it; a finished video is forgotten at once.* The history-off line
changes from "qveil doesn't remember what you watch" to "qveil keeps no list
of what you watch", which stays true with resume on. `privacy::describe` takes the settings as well as the route so the
window states the live value.

This is on by default because the owner asked for it; the first time a
position is saved the window says so once and links to the setting.

### 4. Lenses and smart search

A lens is a feed described in words. Modelled, from public behaviour only, on
YouTube's "Your custom feed" and "Ask YouTube": none of their code, no API
beyond the searches yt-dlp already makes, and no model, so no summaries and no
conversation.

**The reader** (`lens.rs`, pure functions, no I/O). Text in, a `Reading` out:
topics, excluded words, length bounds, earliest date, shorts or not,
subscriptions only / excluded, order. It also returns the tags the window
shows ("about *hand tool woodworking*", "longer than *15:00*"), so what was
understood is always visible. English only in this release.

- Clauses split on commas, semicolons, new lines and a top-level "and".
- Negation: a clause starting `no`, `not`, `without`, `except`, or a `-word`
  becomes excluded words. "no shorts" is the shorts rule (shorts are excluded
  unless asked for).
- Length: "longer than / over / at least N min|h", "shorter than / under /
  at most N", "N-M min", "long" or "long-form" (20 min and up), "short" or
  "quick" (5 min and under).
- Date: "today", "this week / month / year", "past N days / weeks / months /
  years", "since 2024", "new" or "recent" (30 days), "newest first" (order).
- "from my subscriptions" and "not from my subscriptions".
- What is left of a clause after removing those and filler ("videos",
  "about", "show me") is a topic. Rules inside a topic clause are lifted out:
  "systems talks over 30 min this year" is one topic and two rules.
- At most 6 topics. Quoted text is always literal. No topic at all is an
  error that says so.

**Fetching.** One YouTube search per topic through the existing yt-dlp search
path, 30 results each. Where YouTube has a matching filter of its own (its
length and upload-date buckets), the search carries it; the exact rule is
always applied again locally.

**Ranking** (deterministic): YouTube's own order within a topic, a recency
term (30-day half-life), a bonus for subscribed channels, minus videos already
in the history when it is on. Topics are interleaved so one cannot drown the
rest, and no channel gets more than two in a row or four in the first forty.
Every result carries a `why`: its topic and the rules it met.

**Storage.** `lenses.json` in the data directory (0600, at most 12 lenses:
`{ id, text, created }`), results in `~/.cache/qveil/lens-<id>.json`.

**Refresh.** Only when the lens is on screen: on the Refresh action, or when
opened with results older than 6 hours.

**API.**
- `GET api/read?q=`: the reading of a text, offline. The window calls it while
  the user types.
- `GET api/lenses`, `POST api/lenses` (create), `POST api/lenses/<id>`
  (edit), `POST api/lenses/<id>/rm`.
- `GET api/lens?id=&refresh=0|1`: progress lines, then the ranked videos, in
  the feed job's line format.
- `GET api/search` reads its query the same way: a query with no recognised
  rule is passed on untouched, as today; one with rules is searched by topic
  and filtered here.

**Command line.** `qveil read "<text>"`, `qveil lens add|ls|show|rm`.

Privacy sentence: *A lens is refreshed as one search per topic, each a request
of its own. YouTube sees those searches; your wording, the filters and the
ranking stay on this computer. On one address, their timing links them.*

### Engine housekeeping

- README privacy section regenerated; the wording tests extended to the new
  sentences.
- Tests: table-driven reader tests, ranker tests, resume expiry with an
  injected clock, comment parsing against a fixture, API tests through
  `testkit`, CLI tests with the fake yt-dlp.
- Real YouTube traffic stays minimal (the address was bot-checked on
  2026-11-07): a handful of calls to confirm comments and search filters.

## Window

The stills are the reference. What they commit to:

**Look.** The language of the owner's recent Omarchy panels (qnet, oc, Sound
Hub), at window size. The owner on the first drawing: the pills "feel too
apple and not like our recent design directions", and asked for system fonts.
- **System typeface.** One family for everything: the `monospace` alias the
  Omarchy bar uses, so `omarchy font set` changes qveil live. No bundled
  fonts. Hierarchy comes from size and weight, as in the panels. Tabular
  figures for times.
- **Square, flat surfaces.** Corners follow Hyprland's rounding (0 today), as
  the app already does. Surfaces are the foreground colour at the panels'
  alphas: rest 0.04, hover 0.08, selected 0.18. Hairlines divide; a section
  is a small dim label with a rule running to the edge. No pills, no shadows,
  no avatar circles, no glow behind the video.
- **Buttons are words** (optionally with a glyph) that take the hover fill;
  tabs are words that take the selected fill.
- **No dye.** The dye shader, the dye wash and the dyed wordmark go. The veil
  becomes frost: the thumbnail blurred and muted, clearing on hover or
  keyboard selection. Setting: Frosted (default), Clear, or no thumbnails.
- Accent is for the wordmark, progress, links and timestamps, and "why"
  lines. Green still means a protected route and amber means Direct.

**Shell.** A slim rail on the left: wordmark, Home, Lenses, Channels, History,
then the route indicator and Settings at the bottom (the indicator opens the
route popup). The top tab bar goes. A search field sits at the top of the
content; the reading's tags appear under it while typing.

**Home.** "Pick up where you stopped" (unfinished videos with time left, only
when there are any), then a row of tabs: Latest, one per lens, New lens.
Below it the grid. Shorts and Refresh sit at the end of that row.

**Lenses.** List on the left, editor on the right: the prompt, its tags, the
exact searches a refresh will send, Refresh and Delete, then the results with
their "why" lines. A search can be kept as a lens.

**Channels, channel page, history, search results.** Same parts as today in
the new shell.

**Watch page.** Replaces the full-window player as the default view.
- Video, title, channel row with Subscribe, counts and date, the description
  (collapsed to three lines).
- A side sheet with three tabs. *About*: full description and the chapter
  list. *Comments*: fetched when the tab is first opened; top or newest;
  more; replies on request. *Up next*: the rest of the list the video was
  opened from, so it costs no request.
- Links in descriptions and comments: a timestamp seeks; a YouTube video or
  channel link opens inside qveil, reduced to its ID; any other link can only
  be copied. qveil never opens a browser.
- Controls: seek bar cut into chapters with the chapter named beside the
  time, play, volume, speed, picture in picture, theatre, fullscreen. A
  "Resumed at 14:05 · Start over" note when a position was restored.
- Keys: existing ones stay (`P` still switches route). New: `I` picture in
  picture, `T` theatre, `C` comments, `[` `]` chapter, `<` `>` speed.

**Picture in picture.** A second window titled "Picture-in-Picture". Omarchy's
stock rule already floats, pins and places windows with that title; the README
gives the rule for other Hyprland setups, and elsewhere it is a small ordinary
window. The video moves there and the main window goes back to browsing, with
a "playing in picture in picture" bar (title, pause, bring back, close).
Hovering the small window shows pause, the seek bar, bring back and close.
Opening another video plays it there. Closing it stops the video.

Technically: one mpv player. The plan starts with a short spike on moving the
video item between windows; if libmpv's render context does not survive that,
the small window gets its own item that loads the same streams at the current
position (a brief rebuffer).

**Settings.** One page: route, history, resume days, thumbnails, quality,
shorts. The route popup stays as the quick way from the seal.

**Structure.** `Style` singleton for tokens. One file per part: `Rail`,
`SearchPill`, `ReadingChips`, `HomePage`, `ResumeRow`, `FeedSwitcher`,
`VideoGrid`, `VideoCard`, `Frost`, `LensesPage`, `LensEditor`, `ChannelsPage`,
`ChannelPage`, `HistoryPage`, `SearchPage`, `WatchPage`, `PlayerSurface`
(shared by the watch page and the small window), `SeekBar`, `SideSheet`,
`AboutTab`, `CommentsTab`, `CommentItem`, `UpNextTab`, `PipWindow`,
`NowPlayingBar`, `SettingsPage`, `RoutePopup`, and `Linkify.js`. `Backend`
gains the calls and properties for details, comments, resume, lenses and
readings, with a list model for comments.

**Kept behaviour.** Right-to-left mirroring, translated strings, keyboard
movement through grids, the error bar with "Change route", toasts,
drop-to-import, engine restart handling.

## Verification

- `bin/test`: Rust and Qt suites. The Qt suite is tied to today's object
  names and is rewritten with the window.
- Stills from real renders in cage; no synthetic input on the desktop and no
  headless outputs.
- A review council on live stills each round: fresh blind seats, one seat for
  identity, scores kept from seats and implementers. The owner's eye decides.

## Release

Version 0.4.0 in `Cargo.toml`, `Cargo.lock` and `pkgver`; README features,
keys, screenshot and generated privacy section. Nothing is pushed or tagged
until the owner says so.

## Not in 0.4.0

- Android, other frontends (see Scope).
- Summaries or chat-style answers (they need a model).
- Posting, liking or anything that needs an account.
- Live streams (unchanged: still refused, with the reason).
- Other languages for the lens reader.

## Risks

- **Picture in picture:** moving a libmpv item between windows. Spike first;
  the fallback is specified.
- **Comments cost:** a comments call repeats the video lookup. Acceptable on
  demand; the lightest invocation is chosen by measurement.
- **Search filters:** YouTube's filter parameters are undocumented. Local
  filtering is the guarantee; theirs is only an optimisation.
- **Qt tests:** about 1 300 lines follow the old layout and need rewriting.
