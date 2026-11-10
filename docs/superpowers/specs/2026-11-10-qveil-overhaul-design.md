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
- yt-dlp cannot tell "no comments" from "comments turned off", so an empty
  answer is not an error; the window says "No comments here, or they are
  turned off."
- Command line: `qveil comments <video> [--new] [-n N] [--replies] [--json]`.

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
- At most 6 topics. Quoted text is always literal. Single-word rules
  ("long", "new", "today") only count as a clause of their own ("jazz, new"),
  and a date only where it ends its clause, so "new york" and "this week in
  tech" stay whole. Exclusions match whole words. A lens with no topic is an
  error that says so.
- The search box is stricter than a lens: a query is only handled here when
  it carries a filter (a length, a date, `-word`, an order). Several topics
  alone ("rick and morty") or a leading "no" ("no man's sky") are sent to
  YouTube as typed.

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
- `GET api/read?q=` (the search box) and `&for=lens` (a lens): the reading of
  a text, offline. The window calls it while the user types.
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
- Keys: existing ones stay (`P` still switches route), except the digits,
  which follow the rail: `1` home, `2` lenses, `3` channels, `4` history,
  `5` settings. New: `I` picture in picture, `T` theatre, `C` comments,
  `[` `]` chapter, `<` `>` speed.

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

### The look, revised (2026-11-10)

The owner saw the first build, drawn in the language of the Omarchy panels,
and said: "kinda feels like im looking at a large plugin not a video site."
Of three directions built into the real window they chose **lead and
shelves**. Where this section and the description of the look above
disagree, this section is the authority. The layout of the other pages, the
privacy rules and every behaviour above stand.

- **Pictures lead.** Thumbnails are shown as they are by default. Frosted
  (blurred until pointed at) and off remain settings.
- **Type** is the desktop's interface font (the family its settings name, as
  its other apps use), not the monospace of the panels, at sizes to be read
  from a seat: 12/13/14 for text, 15 for a card's title, 18 and 22 for
  headings, 32 for the lead. Emphasis is a weight the face has (demi-bold);
  nothing is bold by synthesis.
- **Still not bubbles.** Corners follow Hyprland, fills are flat, there are no
  pills and no avatar circles. Sections are headings without hairline rules;
  tabs are marked by a line under them; a choice among settings keeps the
  selected fill.
- **A bar across the top** in place of the rail (the owner, of the slim icon
  rail: "not a fan of the sidebar"; chosen from mockups): the wordmark, the
  places as words (Home, Lenses, Channels, History), the search field in the
  middle, and at the far end the route (its dot and its name; the dot alone in
  a narrow window) and the settings. It stays while a video is watched, and
  goes in fullscreen. The route's popup opens under where the route is shown.
  How a search was read is shown under the bar.
- **Settings** (also chosen from mockups) have categories down the side
  (Privacy, Playback, Home, About) and, beside them, the rows of the one that
  is showing: a setting's name and what it means on one side, its control on
  the other. Privacy has the route, history and resume; About has the
  engine's own account in full, and where the settings file is.
- **Home** opens on the video most recently left unfinished, as a lead: its
  picture, its title large, what is left of it, and Resume, Start over and
  Forget. With nothing unfinished there is no lead. Under it every feed is a
  shelf that scrolls sideways: the latest from the channels you follow, what
  else was left unfinished, then one per lens, and at the end the way to a new
  lens. A lens's shelf shows what the engine already has for it and never
  searches; shelves out of view are neither made nor filled. A shelf's heading
  (or "See all") opens that feed in full: the grid with its tabs, as before.
  Escape, the back arrow in the tab row, `1` and the bar return to the
  shelves. Arrows move along a shelf and between shelves; Enter plays.
- **The window adapts to its size** (the owner, trying the build: "i think the
  scaling is broken"; fixed sizes cut a card off in a 941×508 tile and left a
  full screen nearly empty; chosen from mockups). A shelf shows whole cards,
  sized to the width (about 280 px and up: three across in the tile, four at
  1280). The latest take the rows the window has free, so that a window with
  no lead and no other shelves is filled; with no room for more they are one
  row that scrolls sideways. From 1600 px wide the whole window, and what
  opens over it, is drawn half again as large (four large cards across a full
  HD screen), and the video is rendered at the size it covers.
- **Watching**: "Up next" shows pictures and is the sheet's first tab when the
  video was opened from a list with something after it; otherwise the sheet
  opens on About.

### Topic feeds and sorting (added 2026-11-10, at the owner's request)

The owner asked for "a feed to the home page based off the topics of
subscribed channels (multiple if outliers)" and "ways to sort by different
filters", and chose from questions and mockups: topic feeds that search by
themselves, off by default; one sort menu on every shelf and list.

**Topic feeds.** Engine: `topics.rs` groups the followed channels by the
words and word pairs that come back in at least two of a channel's latest 30
titles (stop words and the channel's own name left out). Two channels share a
group when they have at least two such words in common and their terms are
alike enough; a channel unlike the rest is a group of its own; a channel with
nothing recurring has none. A group's searches are up to three terms, pairs
first. Fixed rules, no model, nothing leaves the machine for it. With the
setting `topic_feeds` on (off by default), `GET api/topics?refresh=1`
searches for the groups whose results are missing or a day old (up to four
groups, 20 results a search), ranks them like a lens, and leaves out channels
that are already followed; `GET api/topics` lists what is kept; `POST
api/topics/hide` hides a group for good. Off, none of it reads or asks
anything. The privacy text has a sentence for on and for off, and a point in
the list while it is on. Window: a row in Settings, Home, with the engine's
sentence; while on, each group is a shelf after the lenses, named by its
terms and saying which channels it comes from, with "Keep as lens" (its
searches become a lens) and "Hide". The window asks for a refresh when the
setting is turned on and once when it starts; the engine decides what is due.

**Sorting and filtering.** One control in every shelf's heading, in the tab
row of a feed in full, and on search results and a channel's page. It says
what it is set to and opens a menu: the order (as it came, newest where that
is not the same, most viewed, longest, shortest), the length (any, under 10
minutes, 10 to 30, over 30) and whether what was watched is shown. It
rearranges what is already here and never asks the engine for anything. Each
shelf and list keeps its own setting, in the window's settings. A video whose
length is not known has no place under a length filter. "Watched" is what
has a kept position and, while the history is kept, what is in it. What
comes next after a video follows the order that was showing.

### Fluid, and at home in a tile (added 2026-11-10, at the owner's request)

The owner, using a tiling window manager: "wishing the ui felt more fluid",
"the ui doesnt feel native to it"; all four parts of a live mockup chosen.
Where this differs from "The window adapts to its size" above, this stands.

- **It follows the tile.** The size is continuous: 1 up to 1280 px of width,
  then growing in a straight line to 1.5 at 1920 and staying there (no step
  at 1600). In a short tile (under 560 units of page height) the lead is a
  strip: its title, what is left, its picture; pressing it resumes; a whole
  row of the latest fits under it. In a narrow column the lead drops its
  picture (under 700 units of width), cards go two across, and the search
  field folds into its key and opens over the places while it is used.
  h j k l move like the arrows on the shelves, in grids, in lists and in the
  settings; while a video has the page, J and L are still ten seconds.
- **Things move, briefly.** What the stage shows (a page, the video) fades in
  and settles over 180 ms; what left is gone at once. Menus and popups fade.
  A card's picture comes a little closer under the pointer and the keyboard's
  outline fades. All of it is off when the desktop's animations are
  (`enable-animations`).
- **Loading without pops.** A shelf whose videos are still coming shows their
  places, at their size, breathing; a picture fades into its place when it is
  there; the lead fades in. A shelf is as tall empty as full.
- **Shelves glide.** A row rests on a whole card. Pointed at, a shelf shows an
  arrow at each edge where there is more, which glides it by a row; the
  keyboard glides it too, and the page glides to what the keyboard is on.

### After the review council (2026-11-10)

The owner asked for a council before finishing. Its code seats found no
critical defect; what its chair verified is fixed (see the commits). Its
design seats scored the look below the owner's own view, and of the chair's
list the owner chose, from mockups, the following. Where these differ from
the text above, these stand.

- **The keyboard's cursor** is a 2 px frame in the accent on the picture
  (a card's, an up-next row's), there from the moment a page opens. On home
  it starts on the lead: Enter resumes, left and right choose between Resume,
  Start over and Forget, Down goes to the first shelf. It steps aside when
  the pointer moves and returns with the next key.
- **Watching.** The sheet stands beside the video whenever that costs the
  picture nothing (so also in a quarter tile), and under it only in a narrow
  column. Its tabs are Up next, Comments and, when there are any, Chapters;
  the description is under the video only. C gives the sheet the keyboard:
  left and right change the tab, up and down move in it, Enter acts, Escape
  gives the keys back to the video. N plays the next video.
- **Lenses.** The lenses are a strip along the top with "New lens" at its
  end; under it the open lens's words head the page, then how they read with
  its actions beside, then what a refresh searches for with the first
  sentence of the engine's text and "More" for the rest of it, in place;
  then its videos across the whole width.
- **Channels** are two panes: the channels down one side, each with its
  latest video's picture, and beside them the latest of the one the keyboard
  is on, from what the feed already has. Right goes into that pane; Enter on
  a channel opens its page; u unsubscribes, asked twice.
- **Small corrections.** A card says why it is there only where that differs
  from card to card, and quietly (what is left of an unfinished video keeps
  the accent). The bar marks no place on search results. The sort control is
  a quiet word with a mark. With the history off, its page lists the
  unfinished videos that are kept, each with Forget.
- **Keys for the rest.** S opens the sort menu of what the keyboard is on
  (up, down and Enter in it), O opens a shelf in full, the route's popup
  takes left and right, and in Settings Enter goes from the categories into
  their rows, where left and right change the choice. The help has two
  columns where there is room and never exceeds the window.
- **Topic feeds, corrected.** A day has a budget of twelve searches whatever
  the groups; results are kept by what was searched for; hiding a topic hides
  its channels and phrases; turning the setting off stops a running refresh
  and deletes what was found; what turning it on would send is shown first.

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
