# qveil live streams: implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:**
- Play running YouTube streams through the route, with rewind back to the
  start.
- Wait for announced streams.
- List past broadcasts.
- Show live chat.
- Mark live and upcoming streams in the window.

**Architecture:**
- The engine turns YouTube's live DASH fragments (`url&sq=<n>`) into two
  growing EVENT HLS playlists on loopback.
- It fetches each fragment over the route and passes the bytes through
  unchanged.
- mpv plays the video playlist, with the audio playlist as its external
  audio file.
- Waiting room, chat, live check and Streams tab are new engine endpoints
  and yt-dlp calls. The window draws the owner's picks L2 H3 C1 W2 R1 T1.

**Tech stack:**
- Engine: Rust (`core/`): reqwest blocking, serde_json, rand.
- Window: Qt 6 / QML + C++ (`src/`).
- Window tests: the stand-in engine `tests/shots/engine.py`.

**Spec:** `docs/superpowers/specs/2026-11-11-qveil-live-design.md`. Read
the whole spec, including the appendix, before any task. The appendix
holds the measured facts that this plan's values come from.

## Global Constraints

- Every request goes over the route the video was opened on, and Tor gets
  a fresh identity per lookup, as today. Nothing may fall back to a direct
  connection.
- Media bytes pass through unchanged: no transcoding, remuxing or
  re-encoding.
- Segment links end in `.mkv` (WebM video), `.mp4` (H.264 video) or `.m4a`
  (audio). FFmpeg refuses `.webm`.
- The rewind window is 432,000 s. For a stream older than that, the oldest
  listed segment is `head - 432000/target + 60`.
- Playlists list only segments the engine knows exist (`n <= head`).
- Read-ahead: 3 segments per track in flight, at most 4 held.
- Join a running stream at `start = -(3 × target)`, and a waited-for
  stream at `0`. That is one `loadfile` either way.
- Go-live key: **End**. L stays "10 s on".
- Settings:
  - `live_check`, default `false`. Row text: "Check followed channels for
    live streams"; note: "Reads each channel's Streams tab at every feed
    refresh: one more request per channel, in random order with pauses."
  - `live_chat`, default `true`. Row text: "Show live chat"; note:
    "Loaded only while the Chat tab is open."
- Waiting schedule:
  - once at `starts`;
  - then every 60 s;
  - every 300 s once 30 min late;
  - give up at 6 h late.
- Chat: text only, control characters stripped, 500 characters per
  message, the window keeps 300. Footer copy: "Read only · text, no
  pictures fetched".
- Copy follows the spec's Window section word for word: "live now",
  "Starts in 2 h · Fri 18:00", "Streamed 3 days ago", "● LIVE NOW", "Go
  live", "Fri 18:00 · plays from the start when it begins", "Should have
  started 12 min ago · checking every minute".
- Commits use the repo's noreply email and end with
  `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Verification: `bin/test` (cargo test, then the Qt suite offscreen). Each
  task runs at least its own tests, plus `cargo test` for engine tasks.
- Never run the real yt-dlp or touch YouTube from tests (under test,
  `DEFAULT_YTDLP` is `/nonexistent`).
- Work only in `~/Work/qveil-wt/live`, with its own cargo target. Never
  share `CARGO_TARGET_DIR` with another worktree.

## Review Focus

1. **Tor stalls on one fragment at the edge.** The stalled fetch is redone
   on a fresh client within `FIRST_WITHIN`. The player sees a slow
   segment, never a skipped one. Pinned in Task E5 by
   `a_frozen_fragment_is_fetched_again_not_skipped`.
2. **The user leaves a live video.** Within two segment lengths the
   upstream sees no more requests: head polling, read-ahead and chat's
   yt-dlp all stop. Pinned in Task E5 by
   `nothing_is_fetched_after_the_session_is_cancelled` and in Task E8 by
   `chat_stops_when_the_window_hangs_up`.
3. **The route changes while a live stream plays.** The session ends and is
   not looked up again on the old route. Pinned in Task E5 by
   `a_live_session_is_not_looked_up_again_on_a_route_the_user_left`.
4. **The stream ends while the user has rewound.** `#EXT-X-ENDLIST` is
   added, and playback goes on to the real end with no reload. Pinned in
   Task E5 by `an_ended_stream_gets_endlist_and_keeps_every_segment`.
5. **A refresh is pressed again during a live check.** It joins the
   running check, and no channel is asked twice. Pinned in Task E7 by
   `a_second_refresh_joins_the_running_live_check`.

---

## Engine track

### Task E1: Stream status on every video

**Files:**
- Modify: `core/src/video.rs`
- Modify: `core/src/ytdlp.rs` (`entry`, `is_live*`)
- Modify: `core/src/feed.rs` (`parse_rss`, `announces_live`)
- Modify: every `.live` bool user (`rg -n '\.live\b' core/src`)

**Interfaces:**
- Produces:
  - `pub enum LiveState { Now, Upcoming }`, serialised as `"now"` and
    `"upcoming"`.
  - `Video` gains:
    - `live: Option<LiveState>` (skipped when `None`);
    - `starts: Option<i64>` (from `release_timestamp`, upcoming only);
    - `watching: Option<u64>` (from `concurrent_view_count`);
    - `streamed: bool` (from `live_status == "was_live" | "post_live"`,
      skipped when false).
  - `pub fn live_state(v: &Value) -> Option<LiveState>` in `ytdlp.rs`.

- [ ] **Step 1: Write the failing tests**
  - `ytdlp::tests::entries_carry_live_state_start_and_watchers`: from the
    existing 8-entry fixture plus `{"id":"hhhhhhhhhhh","live_status":"is_upcoming","release_timestamp":1800000000}`
    and `{"id":"iiiiiiiiiii","live_status":"is_live","concurrent_view_count":863}`,
    assert:
    - `live` = `[Now, Upcoming, Now, None, None, None, None, None, Upcoming, Now]`;
    - `starts` of `h` is `Some(1800000000)`;
    - `watching` of `i` is `Some(863)`;
    - `streamed` is true for `d` and `e` only.
  - `video::tests::live_state_serialises_as_words`: `serde_json::to_value`
    of a `Video` with `Now` has `"live":"now"`. With `None` and
    `streamed=false`, neither key is present.
- [ ] **Step 2:** `cargo test --manifest-path core/Cargo.toml entries_carry_live live_state_serialises`. Expect a FAIL (does not compile).
- [ ] **Step 3:** Implement. RSS marks from `announces_live` map to `Now`
  or `Upcoming` by the element's text. Callers that used `!v.live` now use
  `v.live.is_none()`. Topic grouping keeps skipping live entries.
- [ ] **Step 4:** Run `cargo test --manifest-path core/Cargo.toml`. Expect
  all to pass.
- [ ] **Step 5:** Commit: "Videos say whether they are live, upcoming or a
  past stream".

### Task E2: Live lookup

**Files:**
- Create: `core/src/live.rs` (types and format choice; `mod live;` in
  `lib.rs`)
- Modify: `core/src/ytdlp.rs` (`resolve_until`, `explain_no_format`,
  `streams_from`; remove `LIVE_UNSUPPORTED` and `MANIFEST_ONLY`)
- Modify: `core/src/errors.rs` (codes)

**Interfaces:**
- Consumes: `live_state` (E1).
- Produces, in `live.rs`:
  - `pub struct Track { pub base: String, pub ext: &'static str, pub format_id: String }`.
    `ext` is `"mkv"` for a WebM video, `"mp4"` for an MP4 video and
    `"m4a"` for audio.
  - `pub enum Fragments { Numbered, Listed(Vec<(String, f64)>) }`.
    `Numbered` means `base&sq=n`. `Listed` is a past broadcast's DASH
    `fragment_base_url` + `fragments[].path` with each duration.
  - `pub struct LiveStreams { pub title: String, pub channel: Option<String>, pub channel_id: Option<String>, pub video: Track, pub audio: Track, pub fragments: Fragments, pub target: f64, pub started: Option<i64>, pub watching: Option<u64>, pub ended: bool, pub rewind: bool, pub headers: Vec<(String,String)> }`.
    `ended` is true for `was_live`/`post_live`. `rewind` is false when no
    fragment older than head is offered. Fill `rewind = true` here; E5
    corrects it at run time.
  - `pub enum Opening { Plain(Streams), Live(LiveStreams), Upcoming { title: String, channel: Option<String>, starts: Option<i64> } }`.
  - `pub fn choose(v: &Value, max_height: u32) -> Result<LiveStreams>`.
    It takes, from yt-dlp's formats array (ordered worst to best), the
    **last** video-only format with `height <= max_height` and the last
    audio-only format. Protocol must be `http_dash_segments_generator`
    (`Numbered`) or `http_dash_segments` with `fragments` (`Listed`).
- Produces, in `Ytdlp`:
  `pub fn open_until(&self, url: &str, format: &str, max_height: u32, abort: &dyn Fn() -> bool) -> Result<Opening>`.
  - It runs the usual resolve first.
  - On no-format, or when the answer is live, upcoming or manifest-only,
    it makes one live lookup: `-J --no-playlist --live-from-start
    --ignore-no-formats-error`, with no `skip=hls,dash` in the extractor
    args.
  - `resolve_until` stays for the plain path and the relay's
    `resolve_again`.
- New `errors::Code` variants: `MembersOnly`, `Region`, `NoRecording`,
  `GaveUp`.
  - `NoRecording` matches "This live stream recording is not available"
    and yt-dlp's "Post-Live Manifestless".
  - `Code::Live` stays for a stream that has no fragments at all.

- [ ] **Step 1: Write the failing tests** (in `live.rs` and `ytdlp.rs`;
  the fake yt-dlp scripts branch on `$conf`, as in
  `manifests_are_skipped_when_resolving_but_not_when_explaining_a_failure`):
  - `choose_takes_the_best_video_within_the_cap_and_the_best_audio`:
    fixture = the 16 formats listed in the spec appendix (1). With cap
    1440 it picks video `271`, `ext=="mkv"`, and audio `140` with
    `ext=="m4a"`. With cap 1080 it picks `248`. `target == 5.0`.
  - `choose_reads_a_past_broadcasts_fragment_list` gives `Listed` with
    paths and durations in order, and `ended == true`.
  - `a_running_stream_opens_as_live`: the fake yt-dlp answers
    no-format to the plain resolve, then live JSON when `--live-from-start`
    is in `$conf`. Expect `Opening::Live` with `!ended`.
  - `an_announced_stream_opens_as_upcoming_with_its_start` gives
    `starts == Some(1800000000)`.
  - `a_plain_video_still_opens_plain_with_one_call`: `runs(dir) == 1`.
  - `the_live_lookup_never_skips_manifests_and_asks_from_the_start`:
    asserts the second `$conf` has `--live-from-start` and no
    `skip=hls,dash`.
  - `errors::tests`:
    - `no_recording_is_explained` for "This live stream recording is not
      available.";
    - `members_only_is_explained` for "Join this channel to get access to
      members-only content";
    - `region_is_explained` for "The uploader has not made this video
      available in your country".
- [ ] **Step 2:** Run them. Expect a FAIL.
- [ ] **Step 3:** Implement as in the Interfaces block. Headers follow
  `streams_from` (cookie headers dropped).
- [ ] **Step 4:** Run `cargo test --manifest-path core/Cargo.toml`. Expect
  all to pass, with the old live-refusal tests rewritten to the new
  outcomes.
- [ ] **Step 5:** Commit: "A live, announced or manifest-only video is
  looked up as live instead of refused".

### Task E3: Playlists

**Files:**
- Create: `core/src/server/timeline.rs` (pure part only in this task;
  `mod timeline;` in `server.rs`)

**Interfaces:**
- Consumes: `LiveStreams`, `Fragments`, `Track` (E2).
- Produces:
  - `pub(super) struct Span { pub oldest: u64, pub head: u64, pub ended: bool }`.
    `head` is the newest segment that exists (inclusive).
  - `pub(super) const WINDOW_SECS: f64 = 432_000.0;`
    `pub(super) const BACK_MARGIN: u64 = 60;`
  - `pub(super) fn oldest_for(head: u64, target: f64) -> u64`, which is
    `head.saturating_sub((WINDOW_SECS / target) as u64) + BACK_MARGIN`,
    capped at `head`.
  - `pub(super) fn playlist(span: &Span, target: f64, prefix: &str, ext: &str) -> String`
    for `Numbered`, and
    `pub(super) fn listed_playlist(durations: &[f64], prefix: &str, ext: &str) -> String`
    for `Listed` (always ended).

- [ ] **Step 1: Write the failing tests:**
  - `a_running_playlist_is_an_event_list_of_known_segments`: for
    `Span{oldest:10, head:12, ended:false}`, target 5, prefix `"v/"`, ext
    `"mkv"`, the output equals exactly:
    ```
    #EXTM3U
    #EXT-X-VERSION:7
    #EXT-X-TARGETDURATION:5
    #EXT-X-PLAYLIST-TYPE:EVENT
    #EXT-X-MEDIA-SEQUENCE:10
    #EXTINF:5.000,
    v/10.mkv
    #EXTINF:5.000,
    v/11.mkv
    #EXTINF:5.000,
    v/12.mkv
    ```
  - `an_ended_playlist_ends_with_endlist`: the same output plus
    `#EXT-X-ENDLIST\n`.
  - `the_oldest_segment_is_five_days_back_plus_a_margin`:
    `oldest_for(476821, 5.0) == 476821-86400+60`, and `oldest_for(100, 5.0) == 60`.
  - `a_fragment_list_plays_as_a_finished_video`: three durations
    `[5.0, 5.0, 2.5]` give the right `#EXTINF` lines and
    `#EXT-X-TARGETDURATION:5`, then `#EXT-X-ENDLIST`.
- [ ] **Step 2:** Run them. Expect a FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** Run them. Expect a PASS.
- [ ] **Step 5:** Commit: "The engine writes a live stream's playlists".

### Task E4: Fake googlevideo for live

**Files:**
- Modify: `core/src/server/testkit.rs`

**Interfaces:**
- Produces:
  `pub struct LiveUpstream { pub base: String, .. }` with:
  - `start(target_ms: u64, first: u64, head: u64) -> LiveUpstream`. Head
    grows by one every `target_ms`.
  - `end(&self)`: stops growth; a later lookup reports `was_live`.
  - `drop_before(&self, n: u64)`: segments below `n` answer 404.
  - `freeze(&self, n: u64)`: the first request for `n` hangs until
    dropped.
  - `forbid_until_relooked(&self)`: answers 403 until a new base URL
    generation is used.
  - `requests(&self) -> Vec<String>`.
  - Paths: `/live/{gen}/{track}` (HEAD answers 302 to
    `/live2/{gen}/{track}`, which carries `X-Head-Seqnum`). With `&sq=n`
    the response body is `format!("{track}-{n}")` repeated to 64 KiB,
    with headers `X-Head-Seqnum` and `X-Sequence-Num`.
  - `fake_live_ytdlp(dir, &LiveUpstream) -> PathBuf`: plain resolve says
    no format; the live lookup prints a two-format JSON (`271`, `140`)
    whose `url` is `{base}/live/{gen}/v` or `/a`, with `target_duration`
    `target_ms/1000`.

- [ ] **Step 1:** Write `testkit::tests::the_live_fake_grows_redirects_and_numbers_segments`.
  HEAD the base, follow the 302, and read `X-Head-Seqnum == head`. Wait
  one tick and it is `head+1`. `&sq=5` gives a body starting with `v-5`.
- [ ] **Step 2:** Run it. Expect a FAIL. **Step 3:** Implement.
  **Step 4:** Run it. Expect a PASS.
- [ ] **Step 5:** Commit: "Test kit: a live googlevideo stand-in".

### Task E5: Serving a live session

**Files:**
- Modify: `core/src/server/timeline.rs` (session runtime)
- Modify: `core/src/server.rs` (routes, register, `open_until`, `PlayInfo`)
- Modify: `core/src/server/api.rs` (`play`)

**Interfaces:**
- Consumes: E2 `Ytdlp::open_until` and `Opening`; E3 playlists; E4 fake;
  existing `Net::relay_client`, `register`, `MAX_SESSIONS`, `http::Watch`,
  and `FIRST_WITHIN`.
- Produces:
  - `pub(super) struct LiveSession`, registered beside `Session` under the
    same sid space and cap. It holds:
    - the route (`Net`);
    - a `Mutex<Upstream { generation, video: Track, audio: Track, headers, client }>`;
    - a `Mutex<Span>`;
    - per-track read-ahead `Mutex<VecDeque<(u64, Arc<OnceLock<Result<Vec<u8>>>>)>>`;
    - `cancelled: AtomicBool`;
    - `Fragments`.
  - Routes in `Server::handle` (after the token):
    - `GET ["live", sid, "video.m3u8" | "audio.m3u8"]`: the playlist with
      prefix `"v/"`/`"a/"`.
    - `GET ["live", sid, "v"|"a", "<n>.<ext>"]`: the segment bytes, with
      `Content-Type` `video/webm`/`video/mp4`/`audio/mp4`.
  - Behaviour:
    - The head comes from the **final** response's `X-Head-Seqnum`:
      after each fragment, and from a HEAD of the base (following
      redirects) every `target` seconds while no fragment was fetched in
      the last `target`.
    - `n > head`: wait for the head up to `2 × target`, else 404.
    - `n < oldest`: 410, and `oldest` moves up to `n + 1`.
    - A request for `n` starts read-ahead of `n+1..=n+3`, held at most 4
      per track. A request outside the held range clears the read-ahead
      (a seek).
    - Stall: no first byte within `FIRST_WITHIN` means the fetch is
      redone once on a fresh client (Tor: a fresh lookup, as in
      `Server::recover`), never skipped.
    - 403: one fresh live lookup (generation + 1), shared by
      simultaneous failures, then retry.
    - End: the head is unchanged for `3 × target`, then one check
      lookup. If that says ended, `span.ended = true`.
    - Cancel (evicted, route changed, or `api/play` superseded): every
      thread stops before its next request.
  - `PlayInfo` gains:
    - `live: Option<LiveInfo>` with
      `LiveInfo { state: "now"|"ended", segment: f64, started: Option<i64>, rewind: bool, watching: Option<u64> }`;
    - `waiting: Option<Waiting { starts: Option<i64> }>`.
  - For a live opening, `video` = `{base}/live/{sid}/video.m3u8` and
    `audio` = `.../audio.m3u8`. `start` is `-(3×target)` for `now` and
    absent for `ended`. Upcoming answers 200 with `waiting` set and no
    `video`; history is not written for a waiting answer.

- [ ] **Step 1: Write the failing tests** (server tests using `LiveUpstream`
  and `fake_live_ytdlp`):
  - `playing_a_running_stream_answers_two_playlists_and_a_join_point`:
    the answer has `live.state=="now"`, `start == -15.0` for target 5, and
    `video` ending `/video.m3u8`.
  - `the_playlist_lists_only_segments_that_exist_and_grows`: fetch it,
    count entries, wait 1.2 × target, and there is one more.
  - `a_segment_is_the_upstreams_bytes_unchanged`.
  - `reading_forward_fetches_three_ahead_in_parallel`: after asking for
    `n`, `requests()` contains `sq=n+1..n+3` before they are asked for.
  - `a_segment_ahead_of_the_head_waits_for_it`.
  - `a_segment_behind_the_window_is_gone_and_the_playlist_moves_on`
    (`drop_before`): 410, then the playlist starts later.
  - `a_frozen_fragment_is_fetched_again_not_skipped` (Review Focus 1).
  - `a_403_looks_the_stream_up_again_once`.
  - `an_ended_stream_gets_endlist_and_keeps_every_segment` (Review Focus
    4).
  - `nothing_is_fetched_after_the_session_is_cancelled` (Review Focus 2):
    zero new requests within `2 × target` after cancel.
  - `a_live_session_is_not_looked_up_again_on_a_route_the_user_left`
    (Review Focus 3).
  - `an_announced_stream_answers_waiting_and_writes_no_history`.
  - `preparing_next_skips_live_and_upcoming`: the opt-in "get next video
    ready" (`prepare`) never looks up a live or upcoming entry.
- [ ] **Step 2:** Run them. Expect a FAIL. **Step 3:** Implement.
- [ ] **Step 4:** Run `cargo test --manifest-path core/Cargo.toml`. Expect
  all to pass.
- [ ] **Step 5:** Commit: "Live streams play through an engine-built
  timeline".

### Task E6: Waiting room

**Files:**
- Create: `core/src/server/wait.rs`
- Modify: `core/src/server.rs` (route `GET api/wait`)

**Interfaces:**
- Consumes: `Ytdlp::open_until` (E2) and `answer_as_it_goes`-style NDJSON
  (`server/lenses.rs:68`, `http::Watch`).
- Produces:
  - `pub(super) fn next_ask(now: i64, starts: Option<i64>, asked_at: Option<i64>) -> Option<i64>`
    returns the time of the next lookup, or `None` meaning give up.
    - First ask at `starts`, or immediately when `starts` is unknown or
      past.
    - Then `asked_at + 60` while `now - starts < 1800`, else
      `asked_at + 300`.
    - `None` once `now - starts >= 21600`.
  - `GET api/wait?v=<id>` writes NDJSON lines
    `{"state":"upcoming","starts":<ts|null>}`, then later
    `{"state":"now"}` or `{"state":"gave_up"}`, or an error line. It ends
    when the window hangs up (checked at least every 100 ms while
    sleeping).

- [ ] **Step 1: Write the failing tests:**
  - `next_ask_follows_the_schedule`: table rows from the Global
    Constraints, with `starts = 1000`:
    `(now 500, asked None) → 1000`, `(1000, Some(1000)) → 1060`,
    `(2900, Some(2860)) → 3160`, `(22600, Some(22500)) → None`.
  - `waiting_says_now_once_the_stream_starts`: the fake yt-dlp answers
    upcoming twice, then live. Use a test hook to shrink the minute to
    50 ms.
  - `waiting_stops_asking_when_the_window_hangs_up`: `runs(dir)` does not
    grow after the hang-up.
- [ ] **Step 2:** Run them. Expect a FAIL. **Step 3:** Implement.
  **Step 4:** Run them. Expect a PASS.
- [ ] **Step 5:** Commit: "Announced streams are waited for, then played
  once".

### Task E7: Live check, Streams tab, settings

**Files:**
- Modify: `core/src/ytdlp.rs` (`channel_streams`)
- Modify: `core/src/app/feed.rs` (refresh + `channel`)
- Modify: `core/src/feed.rs` (`FeedCache` marks)
- Modify: `core/src/config.rs` and `core/config.example.toml`
- Modify: `core/src/server/api.rs` (`ConfigPatch`, `config_json`,
  `api/channel`)

**Interfaces:**
- Consumes: E1 statuses; the existing `Pacing::of(rt).before_search(wanted)`
  (2–8 s, none under test); `jobs::join_or_start`.
- Produces:
  - `Ytdlp::channel_streams(&self, channel_id: &str, limit: usize) -> Result<Vec<Video>>`.
    It reads `/channel/{id}/streams`, flat, with
    `youtubetab:approximate_date` and `-I 1:limit`.
  - `Config.live_check: bool` (default false) and `Config.live_chat: bool`
    (default true). `ConfigPatch` gains `liveCheck` and `liveChat`, and
    `config_json` emits them.
  - In a feed refresh with `live_check` on:
    - after the RSS results, each subscribed channel in shuffled order
      gets `channel_streams(id, 5)` with `before_search` pauses;
    - progress is reported as extra steps;
    - each channel's live and upcoming marks are replaced in `FeedCache`,
      and its `was_live` entries are merged into Latest (deduplicated by
      ID).
  - With it off, the marks are cleared at the next refresh.
  - `api/channel?id=&tab=streams` answers the same `ChannelPage` shape
    from `channel_streams(id, 30)`.

- [ ] **Step 1: Write the failing tests:**
  - `the_live_check_marks_running_and_upcoming_streams`.
  - `the_live_check_asks_every_channel_once_in_shuffled_order` (seeded
    RNG or order-independent set check, plus count).
  - `a_second_refresh_joins_the_running_live_check` (Review Focus 5).
  - `past_broadcasts_join_latest_once`.
  - `turning_the_check_off_clears_its_marks`.
  - `the_streams_tab_is_read_only_when_asked_for`.
  - The existing `the_example_config_names_every_key_and_only_those`
    passes with the two new keys.
- [ ] **Step 2:** Run them. Expect a FAIL. **Step 3:** Implement.
  **Step 4:** `cargo test`. Expect a PASS.
- [ ] **Step 5:** Commit: "Followed channels can be checked for live
  streams, and a channel lists its streams".

### Task E8: Live chat

**Files:**
- Create: `core/src/chat.rs`
- Modify: `core/src/server.rs` (route `GET api/chat`)
- Modify: `core/src/ytdlp.rs` (a spawning call that keeps its `Call`
  private dir alive)

**Interfaces:**
- Consumes: the `Call` private-dir mechanism, the kill-on-abort process
  group, and `http::Watch`.
- Produces:
  - `pub struct ChatMessage { pub author: String, pub text: String, pub owner: bool, pub amount: Option<String> }`.
  - `pub fn parse_chat_line(line: &str) -> Vec<ChatMessage>`. It reads
    `replayChatItemAction.actions[].addChatItemAction.item` as either:
    - `liveChatTextMessageRenderer`: `authorName.simpleText`; `message.runs[]`,
      where `text`, or `emoji.shortcuts[0]` (else `emoji.emojiId`), is
      joined; `owner` comes from an `authorBadges[].liveChatAuthorBadgeRenderer.icon.iconType == "OWNER"`;
    - `liveChatPaidMessageRenderer`: the same, plus
      `purchaseAmountText.simpleText` as `amount`.

    Control characters are stripped and text is capped at 500 characters.
  - `GET api/chat?v=<id>` refuses with 409 when `live_chat` is off.
    Otherwise it runs `--skip-download --write-subs --sub-langs live_chat
    -o <privdir>/c.%(ext)s <url>` over the route. It tails
    `c.live_chat.json.part` (then `.json`) and writes one NDJSON line per
    message. On hang-up it kills yt-dlp.

- [ ] **Step 1: Write the failing tests:**
  - `a_text_message_with_emoji_reads_as_text` uses the appendix (7) line
    shape and gives `text == ":cloud_with_lightning_and_rain:"` and
    `author == "@JacksunStone"`.
  - `a_super_chat_carries_its_amount`.
  - `the_owners_messages_are_marked`.
  - `control_characters_go_and_long_text_is_cut_at_500`.
  - `chat_streams_lines_as_the_file_grows`: the fake yt-dlp appends 3
    lines with sleeps.
  - `chat_stops_when_the_window_hangs_up` (Review Focus 2): the fake's
    pid is gone within 1 s.
  - `chat_is_refused_when_the_setting_is_off`.
- [ ] **Step 2:** Run them. Expect a FAIL. **Step 3:** Implement.
  **Step 4:** Run them. Expect a PASS.
- [ ] **Step 5:** Commit: "Live chat, read only, text only".

### Task E9: Privacy text and README

**Files:**
- Modify: `core/src/privacy.rs`
- Modify: `README.md` (regenerated with `qveil-core privacy --markdown`)

**Interfaces:**
- Consumes: the E6–E8 behaviour.
- Produces:
  - Three new points.
    - The live check (setting-dependent, like `TOPICS_POINT`): "With the
      live check on, each feed refresh also reads every followed
      channel's Streams tab: one more request per channel, in random
      order with 2–8 s pauses."
    - The waiting room: "Waiting for an announced stream asks YouTube
      once at its start time, then every minute (every 5 minutes after
      half an hour late), and stops after 6 hours or when you leave."
    - Chat: "Live chat is read only while its tab is open: one
      long-running request stream over your route. No pictures (avatars,
      badges, emoji) are fetched."
  - Live playback adds to the playback point: "Live streams are fetched
    segment by segment over your route, a few seconds ahead of what
    plays."

- [ ] **Step 1:** Extend the tests in `privacy.rs` that list points per
  setting, so `live_check` adds its point. Run them, expect a FAIL.
  Implement, then regenerate the README.
- [ ] **Step 2:** Run `cargo test` (including
  `the_readme_has_exactly_what_the_engine_prints`). Expect a PASS.
- [ ] **Step 3:** Commit: "The privacy text covers live streams".

---

## Window track

The engine API is final after E9. Each window task also extends
`tests/shots/engine.py` with the fixtures it needs. Variants are chosen by
`QVEIL_STANDIN_*` environment variables, as today. `QVEIL_STANDIN_LIVE`
already exists, and its entries change to `live:"now"` and `"upcoming"`
with `starts` and `watching`.

### Task W1: Cards and lists (L2)

**Files:**
- Modify: `src/videomodel.cpp/.h`: drop the live filter at cpp:80; add
  the roles `live`, `starts`, `watching` and `streamed`.
- Modify: `src/ShelvesHome.qml:160`: drop `filter(v => !v.live)`.
- Modify: `src/VideoCard.qml` and the meta line in `src/Format.js`.
- Modify: `src/Arrange.js` and `src/SortMenu.qml`: filter "Hide live and
  upcoming".
- Modify: `tests/shots/engine.py`, `tests/window_tests.cpp`.

**Interfaces:**
- Produces:
  - `Format.meta(video, date, brief)`:
    - live: `"live now · <n> watching"`, or `"live now"` without a count;
    - upcoming: `"Starts in <rel> · <Ddd HH:MM>"`;
    - streamed: `"Streamed <ago>"`.
  - `Format.startsIn(ts)` and `Format.dayTime(ts)`.
  - `Arrange` setting gains `live: "show"|"hide"`.
  - Card corner tag, drawn in the existing tag spot:
    - live: Rectangle `Style.accent` with Text `"● LIVE"` in
      `Style.accentText`;
    - upcoming: transparent with a 1 px `Style.dim` border, Text = day and
      time;
    - other cards: unchanged.

- [ ] **Step 1: Write the failing tests:**
  - Rename `liveStreamsAreNotOnShelves` to
    `liveAndUpcomingStreamsAreOnShelvesWithTheirMarks`. Checks: the
    tag texts `"● LIVE"`, `"Fri 18:00"`-style, the meta `"live now · 863
    watching"`, and `"Streamed 3 days ago"`.
  - `hidingLiveAndUpcomingLeavesOnlyVideos`.
  - A Format.js test for `startsIn` ("in 2 h", "in 1 day", "in 5 min").
- [ ] **Step 2:** `bin/test`. Expect a FAIL. **Step 3:** Implement.
  **Step 4:** `bin/test`. Expect a PASS.
- [ ] **Step 5:** Commit: "Cards mark live, upcoming and past streams".

### Task W2: Home live line (H3), channel Streams tab (C1), settings

**Files:**
- Create: `src/LiveLine.qml`
- Modify:
  - `src/ShelvesHome.qml`: the line sits under the lead and is a
    navigable row.
  - `src/backend.cpp/.h`: `openChannel(id, tab)`; `liveNow` derived from
    the feed.
  - `src/Main.qml`: the `channelPage` `extras`, which hold TabWords
    Videos / Streams.
  - `src/SettingsPage.qml`: two rows in category `home`, beside
    `topicsRow`.

**Interfaces:**
- Produces:
  - `Backend.liveNow`: a `QVariantList`, rebuilt on every feed assign.
    It holds entries with `live=="now"`, then `"upcoming"` with
    `starts - now < 86400`, sorted by `starts`.
  - `Backend::openChannel(const QString &id, const QString &tab = "videos")`.
  - `LiveLine { property var entries; property int current; signal picked(var video) }`.
    It shows `"● LIVE NOW"` in `Style.accent`, then items: `"Channel ·
    title"` for live, `"Ddd HH:MM · Channel"` for upcoming. When the items
    overflow, a `"+N"` opens a `Choices` popup with the rest. It is hidden
    when `entries` is empty.
  - ShelvesHome keyboard: the line is row −1 between the lead and the
    first shelf. j and k reach it, h and l move along it, and Enter opens
    the entry.

- [ ] **Step 1: Write the failing tests:**
  - `theLiveLineNamesWhatIsLiveAndNext`.
  - `theLiveLineIsAbsentWithNothingLive`.
  - `theLiveLineIsReachableWithJAndOpensWithEnter`.
  - `aChannelsStreamsTabAsksOnlyWhenOpened` (`asked()` shows
    `api/channel?...tab=streams` only after the click).
  - `theLiveSettingsSaveTheirKeys` (POST `liveCheck`/`liveChat`; extend
    the whitelist at engine.py:526).
- [ ] **Step 2:** Run them. Expect a FAIL. **Step 3:** Implement.
  **Step 4:** `bin/test`. Expect a PASS.
- [ ] **Step 5:** Commit: "Home names live streams; channels list their
  streams; live settings".

### Task W3: Watching live (W2)

**Files:**
- Modify:
  - `src/PlayerSurface.qml`: clock, Go live button, join start.
  - `src/SeekBar.qml`: `live` property.
  - `src/WatchPage.qml`: the stats line.
  - `src/Main.qml`: a `Shortcut { sequence: "End" }`.
  - `src/HelpOverlay.qml`: the End row.
  - `src/mpvitem.cpp`: silence the Matroska resync warnings.
  - Resume saving, wherever positions are posted: skipped when
    `info.live.state === "now"`.

**Interfaces:**
- Consumes: `PlayInfo.live`, `start` (E5).
- Produces:
  - `PlayerSurface.atEdge: bool`, which is
    `live && length - position <= 3 * info.live.segment`.
  - `PlayerSurface.goLive()`, which is `seekTo(length - 3 * info.live.segment)`.
  - Clock text: at the edge, `Format.clock(length) + " · live"`;
    otherwise `Format.clock(position) + " / " + Format.clock(length)`.
    The Go live button (objectName `"go-live"`, text `"Go live"`, key hint
    `End`) is visible only when `live && !atEdge`.
  - Stats line under the title: `"<n> watching · started <clock> ago"`.
  - mpv: `msg-level` with `ffmpeg/demuxer=error`.

- [ ] **Step 1: Write the failing tests** (the stand-in serves a local
  growing EVENT playlist built from `makeClip()` segments under
  `QVEIL_STANDIN_LIVECLIP`):
  - `aLiveStreamOpensNearTheEdgeAndSaysLive`.
  - `fallingBehindShowsGoLiveAndEndReturns`.
  - `aLiveStreamSavesNoResumePosition`.
  - `whenTheStreamEndsItPlaysOnAsAFinishedVideo` (the stand-in appends
    `#EXT-X-ENDLIST`; no second `api/play` in `asked()`).
- [ ] **Step 2:** Run them. Expect a FAIL. **Step 3:** Implement.
  **Step 4:** `bin/test`. Expect a PASS.
- [ ] **Step 5:** Commit: "Live playback: the edge, Go live, and End".

### Task W4: Waiting room (R1)

**Files:**
- Create: `src/WaitingRoom.qml`
- Modify:
  - `src/WatchPage.qml` / `src/PlayerSurface.qml`: show the waiting room
    in place of the player when `info.waiting`.
  - `src/backend.cpp/.h`: `wait(videoId)` over `stream("api/wait", …,
    "wait", …)`, cancelled on leaving.

**Interfaces:**
- Produces:
  - `Backend::wait(const QString &video)`, and the signal
    `waitState(QString state, qint64 starts)`.
  - `WaitingRoom { property var info; property real starts; property string state }`.
    - Softened thumbnail (`Frost`).
    - The eyebrow `"STARTS IN"`, then a countdown at `Style.lead` size.
    - The line `"Fri 18:00 · plays from the start when it begins"`.
    - Past the start: `"Should have started <n> min ago · checking every
      minute"`.
    - `gave_up`: the GaveUp error sentence.
  - On `"now"`, exactly one `backend.play(id)`, and the answer opens at
    `0`.

- [ ] **Step 1: Write the failing tests:**
  - `anAnnouncedStreamOpensTheWaitingRoomAndLoadsNothing` (mpv `loaded`
    stays false).
  - `whenItStartsItPlaysOnceFromTheStart` (`asked()` has exactly one
    `api/play` after `api/wait`).
  - `leavingTheWaitingRoomStopsTheWait`.
  - `aLateStreamSaysHowLate`.
- [ ] **Step 2:** Run them. Expect a FAIL. **Step 3:** Implement.
  **Step 4:** `bin/test`. Expect a PASS.
- [ ] **Step 5:** Commit: "The waiting room".

### Task W5: Chat tab (T1)

**Files:**
- Create: `src/ChatTab.qml`
- Modify:
  - `src/SideSheet.qml`: tabs `chat, next, chapters` while live, with
    `chat` selected on open; Comments returns once ended.
  - `src/WatchPage.qml`: slot wiring.
  - `src/backend.cpp/.h`: `chat(videoId)` over `stream("api/chat", …,
    "chat", …)`, plus a `ChatModel` or a capped `QVariantList`.

**Interfaces:**
- Produces:
  - `Backend::chat(const QString &video)` and `Backend::stopChat()`.
  - Signal `chatLines(QVariantList)`.
  - `ChatTab` implements the tab contract: `step(by)`, `activate()`, and
    `keyed`.
    - It keeps 300 messages.
    - It sticks to the newest unless scrolled up.
    - Author in `Style.faint`. `owner` authors in `Style.accent`. A Super
      Chat gets a 2 px `Style.warn` rule on its left and the amount in
      `Style.warn` DemiBold.
    - The footer is `"Read only · text, no pictures fetched"`.
  - The chat stream runs only while the Chat tab is current and the
    stream is live: it starts on select and stops on unselect or leaving.

- [ ] **Step 1: Write the failing tests:**
  - `aLiveStreamOpensOnTheChatTab`.
  - `chatShowsMessagesSuperChatsAndOwner`.
  - `chatOnlyRunsWhileItsTabIsShown` (`asked()`).
  - `chatKeepsTheNewest300`.
  - `withChatOffThereIsNoChatTab`.
- [ ] **Step 2:** Run them. Expect a FAIL. **Step 3:** Implement.
  **Step 4:** `bin/test`. Expect a PASS.
- [ ] **Step 5:** Commit: "Live chat in the side sheet".

### Task W6: Stills and the measured check

**Files:**
- Modify: `tests/shot.cpp` (views `home-live`, `channel-streams`,
  `watch-live`, `watch-live-behind`, `waiting`, `watch-chat`) and the
  `bin/shot` header comment.

- [ ] **Step 1:** Add the views and take each still at 941×508 and
  1920×1080. Compare against the mockups' picks.
- [ ] **Step 2 (controller, by hand, over Tor):** on the built app, do
  the spec's "Measured, not assumed" list:
  - time to the first picture;
  - a seek to the start of a 2 h stream;
  - 30 min at the edge, with `paused-for-cache` events and dropped frames
    counted;
  - the end of a stream with no reload;
  - the codec and bitrate played (`ffprobe` / mpv `track-list`).

  Record the numbers in the spec appendix.
- [ ] **Step 3:** Commit: "Stills for live views; measured live playback".
