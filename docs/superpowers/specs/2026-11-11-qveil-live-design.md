# qveil live streams: design

Date: 2026-11-11. Branch: `live`. Target release: 0.5.0.

## What was asked for

"can we add support for live streams and vods to qveil"

Decisions the owner made when asked:

| Question | Answer |
|---|---|
| Which sites | YouTube only. Twitch stays parked with the other frontends. |
| Extras in the first version | All four: past broadcasts list, upcoming waiting room, rewind a live stream, read-only live chat. |
| How home learns what is live | One setting, off by default, that checks every followed channel at each feed refresh. |
| How a live stream reaches mpv | Approach B: an engine-built timeline (below), not a relay of YouTube's HLS playlist. |
| Look | L2 H3 C1 W2 R1 T1 from the mockups: <https://claude.ai/artifact/6Bv1dcVJBoRFDwDmb38d6x> |

## What exists today

- The engine refuses a running or announced stream with "Live streams aren't
  supported yet" (`ytdlp.rs`, `LIVE_UNSUPPORTED`), and a video only offered as
  an HLS/DASH manifest with `MANIFEST_ONLY`. Lookups skip the manifests
  (`youtube:skip=hls,dash`).
- The window hides every entry marked `"live": true` (`VideoModel::assign`).
- The channel RSS feed doesn't say what is live, so feed entries are only
  marked live when yt-dlp lists them (searches, the fallback).
- A channel shows only its Videos tab. A finished stream already plays like
  any video when YouTube offers it as a plain download. A stream that ended
  recently, or ran very long, is often manifest-only and fails.

## Goals

1. A running stream plays through the route like any video, opening near the
   live edge, with the whole stream behind it seekable.
2. When the stream ends while it plays, it becomes a finished video in the
   same playback, with no reload.
3. An announced stream or premiere opens to a waiting room and plays once,
   from its start, when it begins.
4. Past broadcasts are listed on each channel. Those that are only offered as
   manifests play.
5. Live chat, read only, beside a running stream.
6. With the live check on, home names what is live or about to start.

Everything goes over the route the video was opened on. The engine passes
media bytes through unchanged: no transcoding, remuxing or resampling.

## Probe already done

On this machine (mpv 0.41.0, FFmpeg 9.0.2), with a synthetic EVENT playlist
of fMP4 segments and no `#EXT-X-ENDLIST`: mpv reports it `seekable=yes`,
`--start=30` lands at 0:30, and `--start=-6` lands 6 s before the end. So
seeking inside a growing playlist works, and rewinding needs no reload.

## Phase 0: measure (first task of the engine plan)

On one real running stream, and one announced one if one is at hand, over
Tor through the engine's own route settings. Keep the request count small
(about ten lookups, and fragments for two minutes of playback). This
machine's IP has been bot-checked by YouTube before.

Questions, with each answer recorded in the appendix of this document:

1. **Formats.** Which formats does yt-dlp list with live-from-start (codec,
   height, fps, bitrate, video-only or audio-only, protocol)? Which does it
   list over HLS, for comparison? Which is best within `max_height`?
2. **Fragments.** The fragment URL shape (the `sq` number). The response
   headers that give the newest segment number, its time and the segment
   length. The container: are fragments self-initialising, or do they need
   an init segment (`#EXT-X-MAP`)?
3. **How far back.** The oldest fragment that can still be fetched, and what
   a stream with rewind turned off answers.
4. **mpv on a growing playlist.** Does mpv reload an EVENT playlist at the
   target duration? Does `duration` grow? Seek far back, then append
   `#EXT-X-ENDLIST`. What does reloading cost in CPU at 10,000 and 40,000
   entries (synthetic)?
5. **Audio in step.** Does a separate audio playlist, given through
   `audio-files` as today, stay in sync over 30 minutes (synthetic), and over
   the two real minutes?
6. **Streams tab.** Do flat entries carry `live_status`, `release_timestamp`
   and a viewer count?
7. **Chat.** On a running stream, does `yt-dlp --skip-download --write-subs
   --sub-langs live_chat` keep writing as chat arrives? What is its line
   format, and how far behind does it run?
8. **Announced streams.** What does a lookup of an announced stream return,
   and what does it say on the way out?

If an answer breaks this design, it goes back to the owner before the rest
of the engine plan runs. The known fallbacks:

- Fragments need an init segment: add `#EXT-X-MAP`.
- Reloading a long playlist costs too much: the engine lists older stretches
  as one entry each, a loopback link that serves several consecutive
  fragments.
- A separate audio playlist drifts: serve an HLS master playlist with an
  `EXT-X-MEDIA` audio group, so FFmpeg's HLS demuxer keeps both in step.

## Engine

### Lookups

The usual lookup is unchanged and stays fast. When its answer, or
`explain_no_format`'s second look, says live, upcoming, post-live or
manifest-only, the engine makes a **live lookup**. That is a yt-dlp call
without `skip=hls,dash` and with live-from-start. It returns YouTube's live
DASH formats, and the engine picks the best video within `max_height` plus a
separate best audio track, with the same preferences as a normal video. An
announced stream answers with its scheduled start, and the play request
gets a `waiting` answer (below) instead of an error. `LIVE_UNSUPPORTED` and
`MANIFEST_ONLY` go away.

### The timeline (`server/timeline.rs`)

A live session is a `Session` like any other: the same route, the same pool
of upstream connections, stall recovery, and a fresh lookup when the
upstream URLs go stale (googlevideo links expire after hours, and segment
numbers stay stable across lookups). In place of a byte range it holds:

- **The newest segment number.** Read from the headers of each fragment it
  fetches. While the player sits at the edge and fetches nothing, the
  timeline asks for it about once per segment length.
- **Two playlists for mpv**, video and audio, at
  `/live/<session>/video.m3u8` and `/live/<session>/audio.m3u8`, guarded
  like the relay's links:
  - `#EXT-X-PLAYLIST-TYPE:EVENT` with the segment length as
    `#EXT-X-TARGETDURATION`;
  - one entry per segment, from the oldest kept up to the newest;
  - `#EXT-X-ENDLIST` once the stream has ended.
- **Segments** at `/live/<session>/{v,a}/<n>`. The engine fetches segment
  `n` over the route and hands its bytes on unchanged.
  - The playlists list only segments the engine knows exist, so mpv never
    asks ahead of the stream. A failed segment would be skipped by FFmpeg,
    which is a gap.
  - A request ahead of the newest known number (a race) waits up to two
    segment lengths for it.
  - A segment that has dropped off the back of the rewind window answers
    410, and the playlist starts later from then on.
- **The end.** The newest segment number stops moving, and a check lookup
  says the stream has ended (`was_live` or `post_live`). The playlist then
  gets `#EXT-X-ENDLIST`, mpv plays to the end, and the usual end of a video
  follows (Replay, Next).

The play answer gains a `live` object:
`{"state": "now", "segment": <seconds>, "started": <ts>, "rewind": true|false}`.
It also carries the two playlist links in place of the stream links. The
window opens a running stream near the edge (mpv `start=` three segments
back from the end) and a waited-for stream at `0`. That is one load either
way.

A past broadcast that is only offered as a manifest plays through the same
timeline, which is already ended at open.

### Statuses in lists

`Video.live: bool` becomes a status the window can draw:
`"live": "now" | "upcoming" | null`, with `"starts": <ts>` for upcoming and
`"watching": <n>` when known. A finished stream is an ordinary video with
`"streamed": true`, so its card can say "Streamed 3 days ago". Searches, the
fallback feed and the Streams tab fill these in from yt-dlp's
`live_status`, `release_timestamp` and viewer count. RSS fills in nothing.

### The live check (setting `live_check`, off by default)

When it is on, each feed refresh also reads every followed channel's Streams
tab: flat, the first five entries, approximate dates. The checks go in
shuffled order with random 2–8 s pauses between them, single-flight, over
the route. This is the same pattern and helper as the topic searches.

- Each channel's result replaces that channel's live and upcoming marks.
- Past broadcasts it finds join Latest. They are deduplicated by ID, since
  the RSS feed carries most of them already.
- The refresh job reports the live results when they are in, after the
  RSS results.
- With the check off, the marks it set are dropped at the next refresh.

What a mark says is only as fresh as the last check. Opening a video always
asks again, so a card that says live but has since ended opens as a
finished video.

### A channel's streams

`api/channel` takes `tab=streams`. It is the same reader as the live check,
with a longer list (30, like Videos). It runs only when the Streams tab is
opened.

### The waiting room (`api/wait`)

When the play answer says `waiting`, the window opens `GET /api/wait?v=<id>`,
an NDJSON job tied to the request in the usual way (it stops when the window
hangs up). It sends `{"state":"upcoming","starts":<ts>}` at once, then asks
YouTube on this schedule:

- once at the scheduled time;
- every minute after that;
- every 5 minutes once the stream is 30 minutes late.

After 6 hours late it stops and sends `{"state":"gave_up"}`. When a lookup
says the stream has started, it sends `{"state":"now"}`, and the window
makes its one play request.

### Chat (`api/chat`)

`GET /api/chat?v=<id>` is an NDJSON job, open only while the Chat tab is on
screen. It runs yt-dlp's live-chat reader (`--skip-download --write-subs
--sub-langs live_chat`) over the same route, with the output file in the
call's private folder. The engine tails that file and sends one line per
message:

```json
{"author": "...", "text": "...", "owner": false, "amount": null}
```

- `owner` marks the channel's own messages.
- `amount` is a Super Chat's amount, as text.
- Text only: custom emoji become `:name:`. Avatars, badges and emoji
  pictures are never fetched.
- Control characters are stripped and each message is capped at 500
  characters.
- The window keeps the last 300 messages.

Chat replay on past broadcasts is not part of this version.

### Settings

| Key | Default | Row |
|---|---|---|
| `live_check` | off | "Check followed channels for live streams": reads each channel's Streams tab at every feed refresh, one more request per channel, in random order with pauses. |
| `live_chat` | on | "Show live chat": loaded only while the Chat tab is open. |

Both are additive with defaults, so an older config still loads.

### Privacy text

The engine's single source (`privacy.rs`) gains three things:

- the live check: what it reads, how often, and the pauses;
- the waiting room's lookups and their schedule;
- chat: one long-running request stream while the tab is open, with no
  pictures fetched.

The README section is regenerated from it, and the existing test keeps them
in step. The route tests that prove nothing leaves outside the route cover
the timeline, the waiting room and chat too.

### Errors

Each gets an error code, and a sentence that says what happened:

- members-only;
- not available in this region;
- the stream ended and YouTube kept no recording;
- the stream hasn't started and qveil gave up waiting after 6 hours.

Rewind turned off by the streamer is not an error. The playlist starts at
the oldest segment kept, and the seek bar covers only that span.

### Interaction with existing features

- **Resume** positions aren't saved while a stream runs. A finished
  broadcast resumes like any video.
- **History** records a live stream when it is opened, as it does any
  video, if history is on.
- **"Get next video ready"** skips live and upcoming entries.
- **Picture in picture** works as today (the mpv core moves). Chat stays in
  the main window.
- **Topic feeds** don't use live entries to group words.

## Window

- **Cards (L2).** No red anywhere; the accent colour carries the marks.
  - Running: `● LIVE` in an accent fill where the duration sits, and
    "live now · 1.2K watching" under the title.
  - Upcoming: its day and time in an outlined corner tag, and "Starts in
    2 h · Fri 18:00".
  - Finished: "Streamed 3 days ago" with its length.
  - `VideoModel` stops dropping live entries. The sort and filter menu
    gains "Hide live and upcoming".
- **Home (H3).** One line under the lead, shown only when the live check
  or a search has found something:
  - it reads `● LIVE NOW`, then "Channel · title" for each running stream,
    then upcoming streams starting within 24 hours ("Fri 18:00 · Signal &
    Noise");
  - what doesn't fit goes under a "+N" that opens the full list;
  - keyboard: j and k reach the line, h and l move along it, and Enter
    opens.
- **Channel page (C1).** Two tab words under the channel's name, Videos and
  Streams. Streams loads on first open (one request). It shows running,
  then upcoming, then past broadcasts.
- **Watching live (W2).**
  - The seek bar spans the whole stream so far, and grows.
  - At the edge (within three segments) the clock reads "2:14:05 · live".
  - When you fall behind it reads "position / length", and a Go live
    button appears. The button, and the End key, seek to the edge. (L
    already moves 10 s forward, so it is not used.)
  - Under the title: "1.2K watching · started 2:14:05 ago".
- **Waiting room (R1).** The thumbnail softened, with a large countdown,
  and "Fri 18:00 · plays from the start when it begins". Once the start
  time has passed it reads "Should have started 12 min ago · checking
  every minute". After 6 hours it gives the gave-up sentence. Nothing is
  loaded into mpv until the stream starts.
- **Chat (T1).** While a stream runs, the side sheet's tabs are Chat, Up
  next, Chapters. Chat is selected when you open the stream, and Comments
  returns once the stream ends.
  - Author names are muted and text is plain. Channel-owner messages show
    the author in accent. Super Chats get a thin warn-coloured rule and
    their amount.
  - The list sticks to the newest message unless you have scrolled up.
  - A footer reads "Read only · text, no pictures fetched".
- **Settings.** The two rows above, in the category that holds topic feeds.

## Testing

**Engine:**

- A fake googlevideo in the testkit that serves numbered segments, sends
  the newest-number header, grows on a clock, can end, and drops old
  segments. It covers playlists, waiting on a segment not there yet, 410 at
  the back, the end, and a stale-URL lookup partway through.
- Fake yt-dlp answers for:
  - a live lookup and an announced stream;
  - a Streams tab (running, upcoming, past);
  - a chat file that grows.
- Waiting-room schedule tests on a fake clock.
- Live-check tests: shuffled order, pauses, single-flight, and marks
  replaced or dropped.
- Privacy text and route tests extended.

**Window:**

- Tests against the stand-in engine (`tests/shots/engine.py` gains live
  fixtures) for each state: cards, the live line, the Streams tab, the
  clock and Go live, the waiting room, and the chat tab.
- `bin/shot` gains views for them.

**mpv:** a test that grows a synthetic EVENT playlist in front of the real
mpv and checks:

- the reload, and `duration` growing;
- a seek back, and Go live;
- the end, after `#EXT-X-ENDLIST` is appended.

**Measured, not assumed, on the finished build** (over Tor):

- time to first picture for a running stream;
- seeking to the start of a 2-hour stream;
- 30 minutes at the edge with no `paused-for-cache` stalls and no dropped
  frames from the source;
- the end of a stream, with no reload;
- the codecs and bitrates actually played.

## Quality

The engine adds no lossy step. YouTube's own live encode is the lossy link,
and phase 0 records its codec and bitrate per tier in the appendix. The
owner is told which tier plays.

## Plans

1. **Engine.** Phase 0 first, then lookups, timeline, statuses, live check,
   streams tab, waiting room, chat, settings, privacy text, errors.
2. **Window.** Built on that API.

Each plan is executed with a fresh agent per task and one review per track
(minors deferred).

## Appendix: phase 0 measurements

To be filled in by the first task of the engine plan.
