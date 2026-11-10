# qveil 0.4.0 Engine Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give `qveil-core` video details, comments, resume, lenses and smart search, each with its API, command line and privacy text.

**Architecture:** Each feature is a small module with pure logic (`resume.rs`, `comments.rs`, `lens/{read,rank,sp}.rs`) plus thin wiring in `app.rs`, `server/api.rs` and `main.rs`. All network access stays inside the existing `Ytdlp` calls, so routes, isolation and cancellation behave as they do today.

**Tech Stack:** Rust 2024 (existing crate `core/`), serde, jiff, clap, the blocking loopback server already in `core/src/server/`, yt-dlp as a child process. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-11-10-qveil-overhaul-design.md` (sections "Engine" and "Principles that stay").

## Global Constraints

- Work on branch `overhaul`. Never push, tag or touch `main`.
- Only `Ytdlp` and `Runtime::web_client` may touch the network. No new HTTP client, no new host.
- Every new file under the data or cache directory is written with `store::save`, `store::save_compact` or `store::write_atomic` (mode 0600) and changed under `store::lock`.
- Search text, video targets and lens text never go on a command line of a child process: they go through `Call`'s private config file, as today.
- Tests never contact YouTube. They use shell scripts standing in for yt-dlp (`store::testing::script`, `testkit::fake_ytdlp`, `tests/fake-ytdlp.sh`).
- Errors are plain sentences a user can act on. Comments in code follow the file's existing voice: why, not what.
- `cargo test --locked --manifest-path core/Cargo.toml` and `cargo clippy --manifest-path core/Cargo.toml -- -D warnings` pass at the end of every task.
- Commit after each task with a subject in the repository's style (imperative sentence, no prefix) and the trailer `Co-Authored-By: Claude Fable 5.2 <noreply@anthropic.com>`.
- Limits from the spec, verbatim: description cut at 20 000 characters; at most 200 chapters; comments `n` 20..200, default 40, up to 3 replies each, limit 120 s, 16 answers in memory; resume at most 500 entries, `resume_days` 0..365 default 7, first 30 s not kept, last 30 s or past 95 % is finished; at most 12 lenses, 6 topics, 30 results per topic, stale after 6 hours.

## Review Focus

1. **A position reported before the duration is known** (`dur` 0, or `pos` greater than `dur`): nothing is saved and nothing panics. Test in Task 2.
2. **A damaged `resume.json`**: the video still plays and the next save replaces the file. A damaged `lenses.json` is the user's writing: it is an error that names the file, and nothing overwrites it. Tests in Tasks 2 and 7.
3. **A lens that is only rules** ("no music, over 10 min") or longer than 500 characters: refused with a sentence that says what to add, not saved. Test in Task 7.
4. **The route changes while comments or a lens are being fetched**: the request ends with "The route changed; try again" and nothing fetched on the old route is cached. Tests in Tasks 4 and 7.
5. **A search that only looks like it has rules** (`"no country for old men"`, a quoted phrase, a title containing "over 9000"): quoted text is searched literally, and a plain query reaches YouTube unchanged, byte for byte. Tests in Tasks 5 and 7.

---

### Task 1: Video details in the play answer

**Files:**
- Modify: `core/src/video.rs` (add `Chapter`)
- Modify: `core/src/ytdlp.rs` (`Streams`, `streams_from`)
- Modify: `core/src/server.rs` (`PlayInfo`, `open_until`)
- Modify: `core/src/server/testkit.rs` (`fake_ytdlp` title line gains the fields below)

**Interfaces:**
- Produces: `video::Chapter { pub start: u64, pub title: String }` (Serialize, Clone, Debug, PartialEq).
- Produces: `Streams` fields `description: Option<String>`, `views: Option<u64>`, `likes: Option<u64>`, `published: Option<i64>`, `chapters: Vec<Chapter>`.
- Produces: `PlayInfo` JSON keys `description`, `views`, `likes`, `published`, `chapters` (each omitted when absent or empty).

- [ ] **Step 1: Write the failing tests** in `ytdlp.rs` tests:

```rust
#[test]
fn details_come_with_the_streams() {
    let s = resolved(r#"{"title":"T","url":"https://gv.test/c","protocol":"https","vcodec":"avc1",
        "description":"Line one\nLine two","view_count":59000,"like_count":640,"timestamp":1762700000,
        "chapters":[{"start_time":0.0,"title":"Intro"},{"start_time":252.4,"title":"Ownership"},{"start_time":300,"title":""}]}"#).unwrap();
    assert_eq!(s.description.as_deref(), Some("Line one\nLine two"));
    assert_eq!((s.views, s.likes, s.published), (Some(59000), Some(640), Some(1762700000)));
    assert_eq!(s.chapters, [Chapter { start: 0, title: "Intro".into() }, Chapter { start: 252, title: "Ownership".into() }]);
}

#[test]
fn a_long_description_is_cut_and_chapters_are_capped() {
    let text = "é".repeat(30_000);
    let chapters: Vec<String> = (0..300).map(|i| format!(r#"{{"start_time":{i},"title":"c{i}"}}"#)).collect();
    let s = resolved(&format!(r#"{{"url":"https://gv.test/c","protocol":"https","vcodec":"avc1","description":"{text}","chapters":[{}]}}"#, chapters.join(","))).unwrap();
    assert_eq!(s.description.unwrap().chars().count(), 20_000);
    assert_eq!(s.chapters.len(), 200);
}
```

And in `server.rs` tests, `the_play_answer_carries_the_details`: start a server whose yt-dlp script prints a video with `"description":"D","view_count":7,"chapters":[{"start_time":5,"title":"One"}]`, call `s.open(&test_video())`, assert `serde_json::to_value(&info)` has `description == "D"`, `views == 7`, `chapters == [{"start":5,"title":"One"}]`, and that a video without them serializes with none of the five keys.

- [ ] **Step 2: Run** `cargo test --manifest-path core/Cargo.toml details_ a_long_description the_play_answer` and see them fail to compile.
- [ ] **Step 3: Implement.** `published` is `timestamp`, else `release_timestamp`. A chapter without a title is skipped. Cutting counts `char`s, not bytes.
- [ ] **Step 4: Run** the full suite; expect PASS.
- [ ] **Step 5: Commit** "Carry a video's description, counts and chapters in the play answer".

---

### Task 2: The resume store

**Files:**
- Create: `core/src/resume.rs`
- Modify: `core/src/main.rs` (`mod resume;`), `core/src/store.rs` (`Paths::resume`), `core/src/config.rs` (`resume_days`), `core/config.example.toml`

**Interfaces:**
- Produces:

```rust
pub const MAX: usize = 500;
pub const EDGE: u64 = 30; // seconds at either end that are not a position worth keeping

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Position { pub id: String, pub pos: u64, pub dur: u64, pub at: i64, pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub channel: Option<String> }

#[derive(Debug, PartialEq)]
pub enum Noted { Kept, Finished, TooEarly, Off, Unknown }

pub fn note(paths: &Paths, days: u32, p: Position, now: i64) -> Result<Noted>;
pub fn list(paths: &Paths, days: u32, now: i64) -> Result<Vec<Position>>; // live ones, newest first
pub fn get(paths: &Paths, days: u32, id: &str, now: i64) -> Result<Option<Position>>;
pub fn forget(paths: &Paths, id: &str) -> Result<()>;
pub fn clear(paths: &Paths) -> Result<()>; // removes the file; fine if absent
pub fn purge(paths: &Paths, days: u32, now: i64) -> Result<()>; // drops expired; days == 0 clears
pub fn expires(p: &Position, days: u32) -> i64; // p.at + days * 86_400
```

- Produces: `Paths::resume() -> PathBuf` = `data/resume.json`; `Config::resume_days: u32`, default 7.

Rules `note` applies, in this order: `days == 0` → `Off`, nothing written. `dur == 0 || pos > dur` → `Unknown`, nothing written. `pos < EDGE` → `TooEarly`, and an existing entry for the video is removed (the user started it over). `pos + EDGE >= dur || pos * 100 >= dur * 95` → `Finished`, entry removed. Otherwise the entry replaces any older one for the same id → `Kept`. Every write also drops expired entries and, beyond `MAX`, the oldest. An entry is expired when `expires(p, days) <= now`. The file is read with `store::load_cache` semantics (a file that doesn't parse counts as empty).

- [ ] **Step 1: Write the failing tests** in `resume.rs`:

```rust
fn pos(id: &str, pos: u64, dur: u64, at: i64) -> Position {
    Position { id: id.into(), pos, dur, at, title: "T".into(), channel: None }
}

#[test]
fn a_position_is_kept_listed_and_expires() {
    let d = TempDir::new(); let p = d.paths();
    assert_eq!(note(&p, 7, pos("aaaaaaaaaaa", 845, 2463, 1000), 1000).unwrap(), Noted::Kept);
    assert_eq!(get(&p, 7, "aaaaaaaaaaa", 1000).unwrap().unwrap().pos, 845);
    let last = 1000 + 7 * 86_400;
    assert!(get(&p, 7, "aaaaaaaaaaa", last - 1).unwrap().is_some());
    assert!(get(&p, 7, "aaaaaaaaaaa", last).unwrap().is_none());
    assert!(list(&p, 7, last).unwrap().is_empty());
}

#[test]
fn the_ends_of_a_video_are_not_positions() {
    let d = TempDir::new(); let p = d.paths();
    assert_eq!(note(&p, 7, pos("aaaaaaaaaaa", 29, 2463, 0), 0).unwrap(), Noted::TooEarly);
    assert_eq!(note(&p, 7, pos("aaaaaaaaaaa", 845, 2463, 0), 0).unwrap(), Noted::Kept);
    assert_eq!(note(&p, 7, pos("aaaaaaaaaaa", 2433, 2463, 0), 0).unwrap(), Noted::Finished); // last 30 s
    assert!(list(&p, 7, 0).unwrap().is_empty());
    assert_eq!(note(&p, 7, pos("bbbbbbbbbbb", 9500, 10_000, 0), 0).unwrap(), Noted::Finished); // 95 %
    assert_eq!(note(&p, 7, pos("aaaaaaaaaaa", 845, 2463, 0), 0).unwrap(), Noted::Kept);
    assert_eq!(note(&p, 7, pos("aaaaaaaaaaa", 5, 2463, 1), 1).unwrap(), Noted::TooEarly);
    assert!(get(&p, 7, "aaaaaaaaaaa", 1).unwrap().is_none(), "starting over forgets the old position");
}

#[test]
fn an_unknown_duration_or_resume_off_saves_nothing() {
    let d = TempDir::new(); let p = d.paths();
    assert_eq!(note(&p, 7, pos("aaaaaaaaaaa", 100, 0, 0), 0).unwrap(), Noted::Unknown);
    assert_eq!(note(&p, 7, pos("aaaaaaaaaaa", 500, 400, 0), 0).unwrap(), Noted::Unknown);
    assert_eq!(note(&p, 0, pos("aaaaaaaaaaa", 100, 400, 0), 0).unwrap(), Noted::Off);
    assert!(!p.resume().exists());
}

#[test]
fn the_newest_five_hundred_are_kept_newest_first() {
    let d = TempDir::new(); let p = d.paths();
    for i in 0..(MAX as i64 + 3) { note(&p, 365, pos(&format!("v{i:010}"), 100, 400, i), i).unwrap(); }
    let all = list(&p, 365, 600).unwrap();
    assert_eq!(all.len(), MAX);
    assert_eq!(all[0].id, format!("v{:010}", MAX as i64 + 2));
}

#[test]
fn a_damaged_file_is_replaced_and_the_file_is_private() {
    let d = TempDir::new(); let p = d.paths();
    std::fs::create_dir_all(&p.data).unwrap();
    std::fs::write(p.resume(), b"{ not json").unwrap();
    assert!(list(&p, 7, 0).unwrap().is_empty());
    assert_eq!(note(&p, 7, pos("aaaaaaaaaaa", 100, 400, 0), 0).unwrap(), Noted::Kept);
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(std::fs::metadata(p.resume()).unwrap().permissions().mode() & 0o777, 0o600);
}

#[test]
fn purging_drops_the_expired_and_zero_days_removes_the_file() {
    let d = TempDir::new(); let p = d.paths();
    note(&p, 7, pos("aaaaaaaaaaa", 100, 400, 0), 0).unwrap();
    note(&p, 7, pos("bbbbbbbbbbb", 100, 400, 86_400 * 5), 86_400 * 5).unwrap();
    purge(&p, 7, 86_400 * 8).unwrap();
    assert_eq!(list(&p, 7, 86_400 * 8).unwrap().len(), 1);
    purge(&p, 0, 0).unwrap();
    assert!(!p.resume().exists());
}
```

Add to `config.rs` tests: `Config::default().resume_days == 7`, and `toml::from_str::<Config>("resume_days = 3")` loads.

- [ ] **Step 2: Run** `cargo test --manifest-path core/Cargo.toml resume` and see it fail to compile.
- [ ] **Step 3: Implement** `resume.rs`, `Paths::resume`, `Config::resume_days`, and document the key in `config.example.toml` ("How many days qveil remembers where you stopped in an unfinished video. 0 turns it off.").
- [ ] **Step 4: Run** the full suite; expect PASS.
- [ ] **Step 5: Commit** "Remember where unfinished videos were stopped, for a set number of days".

---

### Task 3: Resume in the API, the play answer and the command line

**Files:**
- Modify: `core/src/server/api.rs` (routes, `ConfigPatch`, `config_json`, `play`), `core/src/server.rs` (`PlayInfo.resume`, purge in `Server::new`), `core/src/main.rs` (`Cmd::Resume`, `show_config`)
- Test: `core/src/server/api.rs` tests, `core/tests/cli.rs`

**Interfaces:**
- Consumes: Task 2's `resume::*`, `Config::resume_days`.
- Produces HTTP:
  - `GET api/resume` → `[{ "id", "pos", "dur", "at", "title", "channel"?, "expires" }]`, newest first.
  - `POST api/resume` with body `{"v": "<id or url>", "pos": u64, "dur": u64, "title": string, "channel"?: string}` → `{"noted": "kept"|"finished"|"too_early"|"off"|"unknown"}`.
  - `POST api/resume/rm?v=<id>` → `{}`; `POST api/resume/clear` → `{}`.
  - `api/config` gains `resumeDays` (read and patch; a patch is clamped to 0..=365; patching to 0 calls `resume::clear`).
  - `api/play` answer gains `resume: u64` when the target carried no start time and a live position exists.
- Produces CLI: `qveil resume [--json] [--clear]`; `qveil config` prints a `resume` line with the file and the days.

- [ ] **Step 1: Write the failing tests.** In `api.rs` tests:

```rust
#[test]
fn positions_are_noted_listed_forgotten_and_offered_when_playing() {
    let up = Upstream::start();
    let bin = TempDir::new();
    let (s, base, _d) = server_with(Config { ytdlp: fake_ytdlp(&bin.0, &up.url("/video?gen=")), ..Config::default() });
    let id = test_video().id;
    let body = format!(r#"{{"v":"{id}","pos":845,"dur":2463,"title":"T","channel":"C"}}"#);
    assert_eq!(http(&base, "POST", &path(&base, "api/resume"), &body), (200, r#"{"noted":"kept"}"#.into()));
    let (_, list) = http(&base, "GET", &path(&base, "api/resume"), "");
    let v: Value = serde_json::from_str(&list).unwrap();
    assert_eq!((v[0]["id"].as_str(), v[0]["pos"].as_u64(), v[0]["title"].as_str()), (Some(id.as_str()), Some(845), Some("T")));
    assert_eq!(v[0]["expires"].as_i64().unwrap() - v[0]["at"].as_i64().unwrap(), 7 * 86_400);
    let (_, play) = http(&base, "POST", &path(&base, &format!("api/play?v={id}")), "");
    assert_eq!(serde_json::from_str::<Value>(&play).unwrap()["resume"], 845);
    // A link with its own start time wins.
    let (_, play) = http(&base, "POST", &path(&base, &format!("api/play?v=https://youtu.be/{id}?t=60")), "");
    assert!(serde_json::from_str::<Value>(&play).unwrap().get("resume").is_none());
    assert_eq!(http(&base, "POST", &path(&base, &format!("api/resume/rm?v={id}")), "").0, 200);
    assert_eq!(http(&base, "GET", &path(&base, "api/resume"), "").1, "[]");
    drop(s);
}

#[test]
fn turning_resume_off_deletes_what_was_kept() {
    let (s, base, _d) = server();
    let body = r#"{"v":"dQw4w9WgXcQ","pos":100,"dur":400,"title":"T"}"#;
    http(&base, "POST", &path(&base, "api/resume"), body);
    assert!(s.paths.resume().exists());
    let (status, cfg) = http(&base, "POST", &path(&base, "api/config"), r#"{"resumeDays":0}"#);
    assert_eq!(status, 200);
    assert_eq!(serde_json::from_str::<Value>(&cfg).unwrap()["resumeDays"], 0);
    assert!(!s.paths.resume().exists());
    assert_eq!(http(&base, "POST", &path(&base, "api/resume"), body).1, r#"{"noted":"off"}"#);
    let (_, cfg) = http(&base, "POST", &path(&base, "api/config"), r#"{"resumeDays":9999}"#);
    assert_eq!(serde_json::from_str::<Value>(&cfg).unwrap()["resumeDays"], 365);
}
```

In `core/tests/cli.rs`, `resume_lists_and_clears`: write a `resume.json` with one live entry into a temp data dir, run `qveil-core resume --json` and assert the id is printed, run `resume --clear`, assert the file is gone and `resume` prints "nothing kept".

- [ ] **Step 2: Run** those tests; expect FAIL.
- [ ] **Step 3: Implement.** `Server::new` calls `resume::purge` once and only notes a failure. The `v` of `POST api/resume` goes through `ids::parse_video`. `qveil resume` lines are `<id>  <title>  ·  <channel>  ·  <m:ss> of <m:ss>  ·  <N> days left`.
- [ ] **Step 4: Run** the full suite; expect PASS.
- [ ] **Step 5: Commit** "Offer the kept position when a video is opened, and let it be listed and cleared".

---

### Task 4: Comments

**Files:**
- Create: `core/src/comments.rs`, `core/src/server/comments.rs`
- Modify: `core/src/ytdlp.rs` (`Ytdlp::comments`), `core/src/server.rs` (route, cache field, `mod comments`), `core/src/main.rs` (`mod comments;`, `Cmd::Comments`)
- Test: in each new file, plus `core/tests/cli.rs`

**Interfaces:**
- Produces:

```rust
// comments.rs
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)] #[serde(rename_all = "lowercase")]
pub enum Sort { Top, New }
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Comment { pub id: String, pub author: String, pub text: String,
    pub likes: Option<u64>, pub time: Option<i64>, #[serde(rename = "timeText")] pub time_text: Option<String>,
    pub pinned: bool, #[serde(rename = "byUploader")] pub by_uploader: bool, pub replies: Vec<Comment> }
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Comments { pub sort: Sort, pub count: Option<u64>, pub comments: Vec<Comment> }
pub const TEXT_MAX: usize = 10_000; // characters of one comment
pub fn parse(v: &serde_json::Value, sort: Sort) -> Comments;

// ytdlp.rs
pub fn comments(&self, url: &str, sort: Sort, n: usize, replies: bool, abort: &dyn Fn() -> bool) -> Result<Comments>;
```

- Produces HTTP: `GET api/comments?v=&sort=top|new&n=&replies=0|1` → `Comments` as JSON (`None` fields omitted is not required; `null` is fine).
- Produces CLI: `qveil comments <target> [--new] [-n N] [--replies] [--json]` with the `Route` flags and `IN_PROC` help.

Decisions: `parse` reads yt-dlp's flat `comments` array, in order. An entry whose `parent` is `"root"` is a thread; any other entry is appended to the `replies` of the thread whose `id` equals its `parent` (a reply whose parent isn't in the list is dropped). `count` is `comment_count`. yt-dlp is asked with args `-J --no-playlist --skip-download --write-comments` and one extractor-args line: `youtube:skip=hls,dash;comment_sort=<top|new>;max_comments=<n>,<n>,0,0`, or with replies `max_comments=<4n>,<n>,<3n>,3`. The limit is a new `const COMMENTS: Duration = Duration::from_secs(120)`. yt-dlp can't tell "no comments" from "comments turned off", so an empty answer is not an error. The server handler lives next to `play` in `handle()`'s match because it needs `Watch` to end the lookup when the window hangs up; it clamps `n` to 20..=200 (default 40), uses `rt.pinned_ytdlp()`, wraps the result in `guarded`, and keeps the 16 most recent answers in `Server.comments: Mutex<VecDeque<((String, Sort, usize, bool), Arc<Comments>)>>`. A guarded failure is not cached.

- [ ] **Step 1: Write the failing tests.** In `comments.rs`:

```rust
#[test]
fn a_flat_list_becomes_threads() {
    let v: Value = serde_json::from_str(r#"{"comment_count":1204,"comments":[
      {"id":"a","parent":"root","author":"@chan","text":"Fixed","like_count":640,"timestamp":1762700000,"_time_text":"9 hours ago","is_pinned":true,"author_is_uploader":true},
      {"id":"b","parent":"root","author":"@mara","text":"Nice"},
      {"id":"b.1","parent":"b","author":"@x","text":"Agreed","like_count":3},
      {"id":"z.1","parent":"z","author":"@lost","text":"orphan"}]}"#).unwrap();
    let c = parse(&v, Sort::Top);
    assert_eq!((c.count, c.comments.len()), (Some(1204), 2));
    assert!(c.comments[0].pinned && c.comments[0].by_uploader);
    assert_eq!((c.comments[0].likes, c.comments[0].time_text.as_deref()), (Some(640), Some("9 hours ago")));
    assert_eq!(c.comments[1].replies.len(), 1);
    assert_eq!(c.comments[1].replies[0].text, "Agreed");
}

#[test]
fn nothing_there_is_an_empty_answer_and_long_text_is_cut() {
    assert_eq!(parse(&json!({}), Sort::New), Comments { sort: Sort::New, count: None, comments: vec![] });
    let v = json!({"comments":[{"id":"a","parent":"root","author":"@a","text":"é".repeat(TEXT_MAX + 5)}]});
    assert_eq!(parse(&v, Sort::Top).comments[0].text.chars().count(), TEXT_MAX);
}
```

In `server/comments.rs` tests (a yt-dlp script that copies its config file to `asked`, counts runs in `runs`, and prints the fixture above):

```rust
#[test]
fn comments_ask_for_top_level_only_and_are_answered_from_memory_the_second_time() {
    // GET api/comments?v=<id> → 200, two threads.
    // The config file has the line: --extractor-args 'youtube:skip=hls,dash;comment_sort=top;max_comments=40,40,0,0'
    // and its last line is the watch URL. A second identical GET leaves `runs` at 1.
    // GET ...&sort=new&n=5000&replies=1 → runs is 2 and the line ends max_comments=800,200,600,3 with comment_sort=new.
}

#[test]
fn hanging_up_stops_the_comments_lookup() {
    // With testkit::hanging_ytdlp: open the request, wait for hanging_started, drop the socket,
    // then assert_stopped_and_cleaned_up(pid, private).
}

#[test]
fn comments_fetched_while_the_route_changes_are_an_error_and_are_not_kept() {
    // A yt-dlp that sleeps 1 s then answers; start the GET, switch_to_tor(&base), read the answer:
    // status 500, body contains "The route changed". A later GET runs yt-dlp again (not served from memory).
}
```

In `core/tests/cli.rs`, `comments_print_as_text_and_json` with a fixture script: text output has `@chan` and `Fixed`; `--json` parses and has `comments[1].replies[0].text == "Agreed"`.

- [ ] **Step 2: Run** `cargo test --manifest-path core/Cargo.toml comments`; expect FAIL.
- [ ] **Step 3: Implement.** Text output of the CLI: one block per thread, `author · timeText · ♥ likes` then the text, replies indented two spaces.
- [ ] **Step 4: Run** the full suite; expect PASS.
- [ ] **Step 5: Commit** "Fetch a video's comments on request".

---

### Task 5: The lens reader

**Files:**
- Create: `core/src/lens.rs` (module root: `pub mod read; pub mod rank; pub mod sp;` added as later tasks create them), `core/src/lens/read.rs`
- Modify: `core/src/main.rs` (`mod lens;`)

**Interfaces:**
- Produces:

```rust
pub const TOPICS_MAX: usize = 6;
pub const TEXT_MAX: usize = 500; // characters of a lens

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)] pub enum Subs { #[default] Any, Only, Exclude }

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Reading {
    pub topics: Vec<String>, pub exclude: Vec<String>,
    pub min_len: Option<u64>, pub max_len: Option<u64>, // seconds
    pub since: Option<i64>,                             // Unix seconds
    pub shorts: bool, pub subs: Subs, pub newest_first: bool,
    pub dropped: usize,                                 // topics beyond TOPICS_MAX
}
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Tag { pub label: String, pub value: String } // "about" / "systems talks"

pub fn read(text: &str, now: i64) -> Reading;
impl Reading {
    pub fn has_rules(&self) -> bool;   // anything besides exactly one topic
    pub fn tags(&self) -> Vec<Tag>;
    pub fn check(&self) -> Result<()>; // Err("Say what it should be about, for example \"woodworking\".") when topics is empty
    /// The reasons a video is kept, or None when a rule turns it away.
    pub fn keeps(&self, v: &Video, subscribed: bool) -> Option<Vec<String>>;
}
```

The grammar, which is the whole decision (all matching is case-insensitive on ASCII; times are UTC):

1. A double-quoted span is one literal topic and is never read for rules.
2. Outside quotes, split into clauses on `,` `;` newline and the word ` and `.
3. In each clause, in this order:
   - A leading `no `, `not `, `without `, `except `: if the rest is `shorts` → `shorts = false`; if the rest starts with `from my subscriptions` → `Subs::Exclude`; otherwise the rest is one excluded phrase. A token `-word` anywhere is the same for that word, and is removed.
   - `shorts`, `with shorts`, `including shorts` as the whole clause → `shorts = true`.
   - `from my subscriptions`, `only my subscriptions`, `channels i follow` → `Subs::Only`; `newest first` → `newest_first`.
   - Length, lifted out wherever it stands: `(longer than|over|more than|at least) N U` → `min_len`; `(shorter than|under|less than|at most) N U` → `max_len`; `N-M U` → both; `long` or `long-form` → `min_len = 1200`; `short` or `quick` → `max_len = 300`. These single words, and `new`, `recent` and `today` below, only count as the first or last word of a clause, so "how long to boil an egg" stays a plain topic. `U` is `s|sec|second(s)|m|min|minute(s)|h|hr|hour(s)`; `N` may be decimal (`1.5 h` = 5400).
   - Date, lifted out: `today` → `now - 86_400`; `this week` → `now - 7 d`; `this month` → the 1st of `now`'s month, 00:00; `this year` → 1 January of `now`'s year; `(past|last) N (day|week|month|year)(s)` → `now - N × (1|7|30|365) d`; `since YYYY` (1990..=2100) → 1 January of it; `new` or `recent` as a word → `now - 30 d`. A leading `from` left dangling before a lifted date is dropped.
   - What is left, minus a leading `show me`, `i want`, `videos about`, `videos on`, `about`, and a trailing `videos`, trimmed and with inner spaces collapsed, is a topic if non-empty.
4. Topics beyond `TOPICS_MAX` are counted in `dropped`.
5. `has_rules()` is false exactly when the reading is one topic and every other field is default (`shorts` defaults to false).
6. Tags, in order: one `about` per topic, one `not` per excluded phrase, `longer than` / `shorter than` with `m:ss`, `since` with `D Mon YYYY`, `shorts` = `included` when true, `only` / `not` = `channels you follow`, `order` = `newest first`, and `ignored` = `N more topics` when `dropped > 0`.
7. `keeps`: `None` if the video is a short and `!shorts`; is live; has a duration outside the bounds (an unknown duration fails a set bound); has a `published` before `since` (an unknown date fails a set `since`); has an excluded phrase in its title or channel name; or fails the subscription rule. Reasons name each rule that was set and met: `over 15:00`, `under 5:00`, `since 1 Jan 2026`, `a channel you follow`.

- [ ] **Step 1: Write the failing tests** in `read.rs` (`NOW` is 2026-11-10 12:00:00 UTC = 1794312000; `JAN1` is 2026-01-01 00:00:00 UTC = 1767225600):

```rust
#[test]
fn the_examples_from_the_design_read_as_drawn() {
    let r = read("slow woodworking with hand tools, no music, longer than 15 minutes, from this year", NOW);
    assert_eq!(r.topics, ["slow woodworking with hand tools"]);
    assert_eq!(r.exclude, ["music"]);
    assert_eq!((r.min_len, r.max_len, r.since), (Some(900), None, Some(JAN1)));
    let r = read("systems talks over 30 min this year -shorts", NOW);
    assert_eq!((r.topics.as_slice(), r.min_len, r.since, r.shorts), (&["systems talks".to_string()][..], Some(1800), Some(JAN1), false));
    assert!(r.has_rules());
}

#[test]
fn a_plain_search_has_no_rules_and_quotes_are_literal() {
    for q in ["rust", "Rust in 100 Seconds", "how to tie a bowline"] {
        let r = read(q, NOW);
        assert_eq!(r.topics, [q]);
        assert!(!r.has_rules(), "{q}");
    }
    let r = read(r#""no country for old men, over 9000""#, NOW);
    assert_eq!(r.topics, ["no country for old men, over 9000"]);
    assert!(!r.has_rules());
}

#[test]
fn lengths_dates_and_the_other_rules() {
    assert_eq!(read("10-20 min synth repair", NOW).min_len, Some(600));
    assert_eq!(read("10-20 min synth repair", NOW).max_len, Some(1200));
    assert_eq!(read("knots, under 5 minutes", NOW).max_len, Some(300));
    assert_eq!(read("lectures at least 1.5 h", NOW).min_len, Some(5400));
    let r = read("long-form interviews", NOW);
    assert_eq!((r.topics.as_slice(), r.min_len), (&["interviews".to_string()][..], Some(1200)));
    assert_eq!(read("cats, past 2 weeks", NOW).since, Some(NOW - 14 * 86_400));
    assert_eq!(read("cats since 2024", NOW).since, Some(1704067200));
    assert_eq!(read("cats today", NOW).since, Some(NOW - 86_400));
    assert_eq!(read("new jazz", NOW).since, Some(NOW - 30 * 86_400));
    assert_eq!(read("jazz, from my subscriptions", NOW).subs, Subs::Only);
    assert_eq!(read("jazz, not from my subscriptions", NOW).subs, Subs::Exclude);
    assert!(read("jazz, newest first", NOW).newest_first);
    assert!(read("jazz, with shorts", NOW).shorts);
    let egg = read("how long to boil an egg", NOW);
    assert_eq!((egg.topics.as_slice(), egg.min_len), (&["how long to boil an egg".to_string()][..], None));
    assert!(!read("what's new in rust", NOW).has_rules());
    assert_eq!(read("show me videos about bread and sourdough videos", NOW).topics, ["bread", "sourdough"]);
}

#[test]
fn only_six_topics_are_read_and_rules_alone_are_refused() {
    let r = read("a1, b2, c3, d4, e5, f6, g7, h8", NOW);
    assert_eq!((r.topics.len(), r.dropped), (6, 2));
    assert_eq!(r.tags().last().unwrap(), &Tag { label: "ignored".into(), value: "2 more topics".into() });
    let r = read("no music, over 10 min", NOW);
    assert!(r.topics.is_empty());
    assert!(r.check().unwrap_err().to_string().starts_with("Say what it should be about"));
}

#[test]
fn the_tags_say_what_was_understood() {
    let tags = read("systems talks over 30 min this year, no drama", NOW).tags();
    let said: Vec<String> = tags.iter().map(|t| format!("{} {}", t.label, t.value)).collect();
    assert_eq!(said, ["about systems talks", "not drama", "longer than 30:00", "since 1 Jan 2026"]);
}

#[test]
fn a_video_is_kept_or_turned_away_by_each_rule() {
    let r = read("talks over 30 min this year, no drama", NOW);
    let v = |title: &str, dur: Option<u64>, published: Option<i64>| Video { id: "aaaaaaaaaaa".into(), title: title.into(),
        channel: Some("Chan".into()), channel_id: None, published, duration: dur, views: None, short: false, live: false };
    assert_eq!(r.keeps(&v("A talk", Some(1900), Some(NOW - 5)), false).unwrap(), ["over 30:00", "since 1 Jan 2026"]);
    assert!(r.keeps(&v("A talk", Some(1700), Some(NOW - 5)), false).is_none());
    assert!(r.keeps(&v("A talk", None, Some(NOW - 5)), false).is_none());
    assert!(r.keeps(&v("A talk", Some(1900), Some(JAN1 - 1)), false).is_none());
    assert!(r.keeps(&v("Conference DRAMA", Some(1900), Some(NOW - 5)), false).is_none());
    let only = read("jazz, from my subscriptions", NOW);
    assert!(only.keeps(&v("Jazz", None, None), false).is_none());
    assert_eq!(only.keeps(&v("Jazz", None, None), true).unwrap(), ["a channel you follow"]);
}
```

- [ ] **Step 2: Run** `cargo test --manifest-path core/Cargo.toml lens::read`; expect FAIL to compile.
- [ ] **Step 3: Implement** `read.rs` as a hand-written tokenizer over the clause's words (no regex crate: none is a dependency). Dates use `jiff` in UTC.
- [ ] **Step 4: Run** the full suite; expect PASS.
- [ ] **Step 5: Commit** "Read a lens: turn a description of a feed into topics and rules".

---

### Task 6: Ranking what the searches found

**Files:**
- Create: `core/src/lens/rank.rs`
- Modify: `core/src/lens.rs`

**Interfaces:**
- Consumes: `Reading`, `Reading::keeps`.
- Produces:

```rust
pub struct Found { pub video: Video, pub topic: usize, pub rank: usize } // rank: its place in that topic's results, from 0
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Ranked { #[serde(flatten)] pub video: Video, pub why: Vec<String> }
pub struct Context<'a> { pub subscribed: &'a HashSet<String>, pub watched: &'a HashSet<String>, pub now: i64 }
pub fn rank(reading: &Reading, found: Vec<Found>, ctx: &Context) -> Vec<Ranked>;
```

The algorithm, which the tests pin:

1. Drop a video that is in `watched`, or that `reading.keeps` turns away (`subscribed` = its `channel_id` is in `ctx.subscribed`). `why` = the topic's text, then the reasons from `keeps`.
2. A video found under several topics is kept once, under the topic where its `rank` is lowest (ties: the lower topic index).
3. Score = `1 / (1 + rank)` + `0.5 × 0.5^(age_days / 30)` (0 without a date) + `0.25` if subscribed. Within a topic, order by score descending, then by id ascending.
4. With `newest_first`: one list ordered by `published` descending (undated last, then id). Skip steps 5 and 6.
5. Interleave: take the best remaining from topic 0, then topic 1, and so on, round and round, skipping exhausted topics.
6. Channel spread, applied to that list in order: a video that would be the third in a row from one channel, or the fifth from its channel among the first 40 placed, is held back and tried again after each later placement; what is still held at the end is appended in its original order. A video without `channel_id` uses its channel name; without either it is never held back.

- [ ] **Step 1: Write the failing tests** in `rank.rs`: a helper `f(id, channel_id, topic, rank, published)`; then

```rust
#[test]
fn topics_take_turns_and_a_repeat_is_kept_once() { /* topic 0: a,b,c; topic 1: b,d → ids a,b,c,d with b under the topic of lower rank: [a, d, b, c] when b is rank 1 in topic 0 and rank 0 in topic 1 */ }
#[test]
fn newer_and_subscribed_videos_move_up_within_a_topic() { /* same topic: x rank 0 dated 400 days ago; y rank 1 dated today from a subscribed channel → y before x (1/2 + 0.5 + 0.25 > 1 + ~0) */ }
#[test]
fn watched_videos_and_ones_a_rule_turns_away_are_left_out() { /* watched id absent; a short absent when !shorts */ }
#[test]
fn one_channel_never_gets_three_in_a_row_or_five_in_the_first_forty() { /* 8 videos of channel A at ranks 0..7 and 8 of other channels at ranks 8..15 in one topic: no three consecutive A, and exactly 4 A among the first 12 */ }
#[test]
fn newest_first_ignores_the_rest_of_the_ordering() { /* published 3,1,2 → 3,2,1 */ }
#[test]
fn why_names_the_topic_and_the_rules_met() { /* reading "talks over 30 min" → why == ["talks", "over 30:00"] */ }
```

Each test asserts the exact id order with `assert_eq!` on a `Vec<&str>`.

- [ ] **Step 2: Run** `cargo test --manifest-path core/Cargo.toml lens::rank`; expect FAIL.
- [ ] **Step 3: Implement** `rank`.
- [ ] **Step 4: Run** the full suite; expect PASS.
- [ ] **Step 5: Commit** "Rank a lens's results: topics in turn, channels spread out, a reason on each".

---

### Task 7: Lenses stored, fetched and served; search reads its query

**Files:**
- Modify: `core/src/lens.rs` (store and types), `core/src/store.rs` (`Paths::lenses`, `Paths::lens_cache`), `core/src/app.rs` (`lens`, `smart_search`), `core/src/server/api.rs` (routes, search), `core/src/server.rs` (route for the streamed lens), `core/src/main.rs` (`Cmd::Read`, `Cmd::Lens`)
- Test: `core/src/lens.rs`, `core/src/server/api.rs`, `core/tests/cli.rs`

**Interfaces:**
- Consumes: Tasks 5 and 6; `Ytdlp::search_each`; `app::load_subs`, `app::load_history`.
- Produces:

```rust
// lens.rs
pub const MAX: usize = 12;
pub const PER_TOPIC: usize = 30;
pub const STALE: i64 = 6 * 3600;
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Lens { pub id: String, pub text: String, pub created: i64 } // id: 8 hex characters
pub fn load(paths: &Paths) -> Result<Vec<Lens>>;                       // store::load: a damaged file is an error
pub fn add(paths: &Paths, text: &str, now: i64) -> Result<Lens>;
pub fn edit(paths: &Paths, id: &str, text: &str, now: i64) -> Result<Lens>; // also removes its cache
pub fn remove(paths: &Paths, id: &str) -> Result<()>;                   // also removes its cache
#[derive(Serialize, Deserialize, Default)]
pub struct Cached { pub fetched_at: i64, pub videos: Vec<Ranked> }

// app.rs
#[derive(Clone, Copy, PartialEq)] pub enum LensRefresh { Off, Stale, Force }
#[derive(Serialize)]
pub struct LensResult { pub lens: Lens, pub tags: Vec<Tag>, pub searches: Vec<String>,
    #[serde(rename = "fetchedAt")] pub fetched_at: Option<i64>, pub videos: Vec<Ranked> }
pub fn lens(rt: &Runtime, paths: &Paths, id: &str, refresh: LensRefresh,
    progress: &dyn Fn(usize, usize), wanted: &dyn Fn() -> bool) -> Result<LensResult>;
pub fn smart_search(rt: &Runtime, paths: &Paths, reading: &Reading, limit: usize, wanted: &dyn Fn() -> bool) -> Result<Vec<Ranked>>;
```

- Produces HTTP:
  - `GET api/read?q=` → `{ "tags": [Tag], "searches": [string], "rules": bool, "error"?: string }` (never a 500 for a readable request; `error` is `check()`'s sentence).
  - `GET api/lenses` → `[{ "id", "text", "created", "tags", "searches", "fetchedAt"?, "count" }]`.
  - `POST api/lenses` body `{"text": …}` → the new entry in that shape; `POST api/lenses/<id>` body `{"text": …}`; `POST api/lenses/<id>/rm` → `{}`.
  - `GET api/lens?id=&refresh=0|1&force=0|1` → `LensResult`; with `&stream=1`, lines `{"progress":{"done":N,"total":M}}` then `{"result": LensResult}` or `{"error","code"}`, in the feed's line format (`NDJSON_HEAD` before the first line).
  - `GET api/search` (streamed or not): `let reading = lens::read::read(q, now)`. Without rules: exactly today's behaviour, the original `q` passed on untouched. A reading with no topic at all ("no country for old men") is also passed on untouched: in the search box a query is never refused for looking like rules. With rules and a topic: `smart_search`; each result is a `Ranked` (so it carries `why`), and the streamed form sends them when all topics are in.
- Produces CLI: `qveil read <text…>` (offline; one tag per line as `label  value`, then `searches:` and each query); `qveil lens add <text…>`, `qveil lens ls [--json]`, `qveil lens show <number or id> [--refresh] [--json|--list]` (with `Route`), `qveil lens rm <number or id>`. Numbers are the 1-based position `lens ls` prints.

Decisions: `add` and `edit` refuse text over `TEXT_MAX` characters ("A lens can be up to 500 characters."), text whose reading fails `check()`, and a 13th lens ("12 lenses is the most; remove one first."). The text is stored trimmed, as typed. `searches` are the reading's topics, in order. `app::lens` with `Off` answers from the cache only; `Stale` fetches when there is no cache or `now - fetched_at >= STALE`; `Force` always fetches. Fetching runs the topics one after another with `rt.yt.search_each(topic, PER_TOPIC, …)` whose callback returns `wanted()`, calls `progress(done, total)` after each, ranks with the subscribed channel IDs and (only when `cfg.history`) the watched IDs, then saves `Cached` with `store::save_compact`. If `!wanted()` after a topic it stops with `ytdlp::CANCELLED` and saves nothing. A topic that fails fails the refresh; the old cache stays. The server wraps the result in `guarded` before answering, and `app::lens` re-checks `still_on` through a `&dyn Fn() -> bool` passed in `wanted` so a refresh from a route that was left is not saved. `smart_search` uses `limit` per topic and nothing is cached.

- [ ] **Step 1: Write the failing tests.** In `lens.rs`:

```rust
#[test]
fn lenses_are_added_edited_removed_and_private() {
    // add → file exists with mode 0600, id is 8 hex chars, load() returns it;
    // edit changes the text and removes paths.lens_cache(id); remove deletes both.
}
#[test]
fn a_lens_needs_a_topic_a_sane_length_and_there_are_at_most_twelve() {
    // add("no music, over 10 min") → Err starting "Say what it should be about"; nothing saved.
    // add(&"x".repeat(501)) → Err "A lens can be up to 500 characters."
    // 12 adds succeed, the 13th → Err "12 lenses is the most; remove one first."
}
#[test]
fn a_damaged_lenses_file_is_an_error_and_is_left_alone() {
    // write "{ nope" to paths.lenses(); load() is Err naming the file; add() is Err; the bytes are unchanged.
}
```

In `api.rs` tests, with a yt-dlp script that appends its target line to `asked` and prints two flat results per call, ids derived from a counter (`{"id":"aaaaaaaaaa1","title":"t","channel":"c","duration":1900.0,"timestamp":<now>}` one per line, as `-j` prints):

```rust
#[test]
fn a_lens_sends_one_search_per_topic_then_answers_from_its_cache() {
    // POST api/lenses {"text":"bread, sourdough over 30 min"} → 200 with tags and searches == ["bread","sourdough"].
    // GET api/lens?id=…&refresh=1 → 200; `asked` has exactly 'ytsearch30:bread' and 'ytsearch30:sourdough';
    // videos non-empty, each with a non-empty `why` whose first entry is its topic.
    // The same GET again leaves `asked` at two lines (fresh cache), and fetchedAt is unchanged.
    // GET …&force=1 → four lines.
    // GET api/lenses → count equals the videos' number.
}
#[test]
fn a_streamed_lens_reports_each_topic_then_the_result() {
    // lines: {"progress":{"done":1,"total":2}}, {"progress":{"done":2,"total":2}}, {"result":{…}}
}
#[test]
fn reading_is_offline_and_says_when_a_topic_is_missing() {
    // GET api/read?q=talks%20over%2030%20min → rules true, tags about/longer than, searches ["talks"]; yt-dlp never ran.
    // GET api/read?q=no%20music → error starts "Say what it should be about".
}
#[test]
fn a_plain_search_reaches_youtube_unchanged_and_one_with_rules_is_filtered_here() {
    // GET api/search?q=rust%20in%20100%20seconds → asked: 'ytsearch40:rust in 100 seconds'; results have no `why`.
    // GET api/search?q=%22over%209000%22 → asked ends 'ytsearch40:"over 9000"' (the original text).
    // GET api/search?q=no%20country%20for%20old%20men → asked ends 'ytsearch40:no country for old men'; no `why`.
    // GET api/search?q=talks%20over%2030%20min → asked: 'ytsearch40:talks'; every result has why == ["talks","over 30:00"].
    // The script's 1900-second results pass; change the fixture to 100 seconds and the answer is [].
}
#[test]
fn a_lens_refreshed_while_the_route_changes_is_an_error_and_is_not_saved() {
    // sleeping yt-dlp; start GET api/lens?…&refresh=1, switch_to_tor, answer is 500 "The route changed";
    // paths.lens_cache(id) does not exist.
}
```

In `core/tests/cli.rs`, `read_and_lens_commands`: `read talks over 30 min` prints `about  talks` and `longer than  30:00`; `lens add bread` then `lens ls` shows `1  bread`; `lens rm 1` empties it.

- [ ] **Step 2: Run** `cargo test --manifest-path core/Cargo.toml lens`; expect FAIL.
- [ ] **Step 3: Implement.** Keep `api.rs` readable: the lens routes go in a new `core/src/server/lenses.rs` (`impl Server`), with only the match arms in `api.rs` and `server.rs`.
- [ ] **Step 4: Run** the full suite; expect PASS.
- [ ] **Step 5: Commit** "Add lenses: feeds described in words, and a search box that reads the same rules".

---

### Task 8: YouTube's own filters, where they match a rule

**Files:**
- Create: `core/src/lens/sp.rs`
- Modify: `core/src/lens.rs`, `core/src/ytdlp.rs` (`search_each` takes the filter), `core/src/app.rs` (pass it)

**Interfaces:**
- Produces: `pub fn sp(reading: &Reading, now: i64) -> Option<String>` (the value of YouTube's `sp` parameter, base64, not URL-encoded); `Ytdlp::search_each(&self, query: &str, limit: usize, sp: Option<&str>, each: …)`.

Decisions: the parameter is a protobuf message: field 2 (length-delimited) holds field 1 = upload date (2 today, 3 this week, 4 this month, 5 this year), field 2 = type (1, videos), field 3 = duration (2, over 20 minutes). Top-level field 1 = 2 means "by upload date". A filter is only used when YouTube's bucket contains the rule: `min_len >= 1200` → duration 2; `since >= now - 86_400` → 2, `>= now - 7 d` → 3, `>= now - 30 d` → 4, `>= now - 365 d` → 5; `newest_first` → sort 2. Type 1 is set whenever anything else is. No rule that maps → `None`, and the search is the plain `ytsearchN:` as before. With `Some(sp)` the target is `https://www.youtube.com/results?search_query=<percent-encoded query>&sp=<percent-encoded sp>` with `-I 1:<limit>` added to the arguments. If a filtered search fails or finds nothing, the same topic is searched once more without the filter. The local rules always run afterwards.

- [ ] **Step 1: Write the failing tests** in `sp.rs`:

```rust
#[test]
fn known_values() {
    assert_eq!(sp(&read("talks over 30 min", NOW), NOW).as_deref(), Some("EgQQARgC"));   // videos, over 20 minutes
    assert_eq!(sp(&read("talks past 2 weeks", NOW), NOW).as_deref(), Some("EgQIBBAB"));  // this month, videos
    assert_eq!(sp(&read("talks", NOW), NOW), None);
    assert_eq!(sp(&read("talks over 10 min", NOW), NOW), None, "YouTube has no bucket that contains it");
    assert_eq!(sp(&read("talks since 2019", NOW), NOW), None);
}
#[test]
fn newest_first_sorts_by_date() {
    assert_eq!(sp(&read("talks, newest first", NOW), NOW).as_deref(), Some("CAISAhAB"));
}
```

And in `api.rs` tests, `a_filtered_topic_asks_youtube_with_the_filter_and_falls_back_without_it`: a lens "talks over 30 min" asks for a target containing `results?search_query=talks&sp=EgQQARgC`; with a script that prints nothing for a `results?` target, the next line asked is `ytsearch30:talks` and the answer has the plain results.

- [ ] **Step 2: Run** `cargo test --manifest-path core/Cargo.toml sp`; expect FAIL.
- [ ] **Step 3: Implement** (base64 by hand: a 20-line encoder; there is no base64 dependency).
- [ ] **Step 4: Check against the real site, once.** Run `./core/target/debug/qveil-core search "conference talk over 30 min" --list -n 10` and confirm every line shows a length over 30:00; run it for `"jazz today"` and confirm the ages. If YouTube ignores a value, delete that mapping and its test line rather than guessing another. This is the only step in the plan that contacts YouTube; make at most four calls.
- [ ] **Step 5: Run** the full suite; expect PASS. **Commit** "Let YouTube filter a search where it has a filter that contains the rule".

---

### Task 9: Privacy text for what was added

**Files:**
- Modify: `core/src/privacy.rs`, `core/src/server/api.rs` (`config_json` passes the settings), `core/src/main.rs` (`privacy` and `config` commands), `README.md` (the generated section only)

**Interfaces:**
- Produces: `privacy::describe(net: &Net, cfg: &Config) -> Privacy`; `Pages` gains `resume: String`, `comments: &'static str`, `lenses: &'static str`.

Exact copy:

- `PAGES.history_off`: `Off: qveil keeps no list of what you watch.`
- `pages.resume`, with resume on (N = `resume_days`): `On: qveil remembers where you stopped in an unfinished video for N days, in a file only you can read, then deletes it. A finished video is forgotten at once.` (`1 day` when N is 1). With 0: `Off: qveil doesn't remember where you stopped.`
- `pages.comments`: `Fetched only when you open them, as one more lookup of that video. Commenters' pictures are never fetched.`
- `pages.lenses`: `A refresh sends one search per topic, each a request of its own. YouTube sees those searches; your wording, the filters and the ranking stay on this computer.`
- `EVERYWHERE` gains: `A lens is refreshed as one search per topic: on one address their timing links them, and even over Tor a set of related searches can be linked by content.`
- `DETAILS` "Player state" becomes: `mpv keeps no resume files or history. qveil itself remembers where you stopped in an unfinished video for a set number of days (7 unless you change it; 0 turns it off) in a file only you can read, then deletes it; a finished video is forgotten at once.`
- `DETAILS` "Local data": add `resume positions and lenses` to what lives in `~/.local/share/qveil`, and lens results to the cache.
- `markdown()` "On the pages" gains `Resume, on` (worded for 7 days), `Resume, off`, `Comments`, `Lenses`.

- [ ] **Step 1: Update the tests first:** `the_json_has_the_shape_the_window_expects` expects pages keys `["comments","history_off","history_on","lenses","resume","search","subscriptions"]`; add `resume_says_the_live_number_of_days` (7 → contains "for 7 days"; 1 → "for 1 day,"; 0 → starts "Off:"); extend `the_words_that_were_wrong…` so "doesn't remember what you watch" appears nowhere.
- [ ] **Step 2: Run** `cargo test --manifest-path core/Cargo.toml privacy`; expect FAIL, including the README comparison.
- [ ] **Step 3: Implement,** then regenerate: `cargo run --manifest-path core/Cargo.toml -- privacy --markdown` and paste between the README's `privacy:begin` and `privacy:end` markers.
- [ ] **Step 4: Run** the full suite and clippy; expect PASS.
- [ ] **Step 5: Commit** "Say what resume, comments and lenses do to your privacy".
