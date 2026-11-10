# qveil 0.4.0 Window Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rebuild qveil's window in the language of the owner's Omarchy panels, with a watch page (description, chapters, comments), resume, lenses, and picture in picture.

**Architecture:** `Backend` (C++) gains the calls and properties for the engine's new endpoints; QML is rebuilt from a `Style` singleton and a handful of small parts (`Act`, `TabWord`, `Field`, `SectionHead`, `Rule`, `Glyph`, `Bar`). One `PlayerSurface` owns the mpv item and its controls and is shown either in `WatchPage` or in `PipWindow`. The old top-bar window, the dye shader and the Material style are removed.

**Tech Stack:** C++17, Qt 6.12 (Quick, Controls "Basic" style, Effects for `MultiEffect`), libmpv render API, qmake. Tests are Qt Test on `QT_QPA_PLATFORM=offscreen`; stills are real renders inside a headless `cage`.

**Spec:** `docs/superpowers/specs/2026-11-10-qveil-overhaul-design.md` (section "Window"). Stills: <https://claude.ai/artifact/Xpv4ujGSAWYU7LRE4HGT1z>. **Requires** the engine plan (`2026-11-10-overhaul-engine.md`) to be finished: this plan uses its endpoints as specified in that plan's Interfaces blocks.

## Global Constraints

- Work on branch `overhaul`. Never push, tag or touch `main`.
- **One typeface:** every `font.family` binds to `Style.font` (the system `monospace` alias, resolved). No second family, no bundled fonts.
- **Shapes:** every corner radius is `Style.radius` (Hyprland's rounding) or 0. No pills, no drop shadows, no avatar circles. The only round things are the route dot and a busy spinner.
- **Surfaces:** fills are the foreground colour at `Style.rest` 0.04, `Style.hover` 0.08, `Style.sel` 0.18; dividers are `Style.line` 0.13. No other greys are invented.
- **Buttons are words** (`Act`), tabs are words (`TabWord`). No Material controls; the app style is "Basic".
- **No dye:** `DyeWash.qml`, `Wordmark.qml`, `src/shaders/` and the shader build in `src/resources.pri` are deleted by the end of Task 5.
- The window never words a privacy claim: such text comes from `backend.privacy` only (the existing test `theWindowHasNoPrivacyClaimsOfItsOwn` stays and must pass).
- The window never opens a browser and never makes a request except to the engine on loopback.
- All user-visible strings go through `qsTr`. Right-to-left mirroring stays on the root item.
- No synthetic input on the desktop and no headless Hyprland outputs. Interaction is tested offscreen with `QTest` events; stills come from `bin/shot` (Task 4).
- `bin/test` passes at the end of every task. Commit after each task in the repository's style, with the trailer `Co-Authored-By: Claude Fable 5.2 <noreply@anthropic.com>`.
- Sizes (pixels at scale 1): caption 11, small 12, body 13, title 16, head 18. Rail width 76. Content gutter 24. Control height 28.
- Keys: existing ones keep their meaning (`P` still switches route), except the digits, which now follow the rail: `1` home, `2` lenses, `3` channels, `4` history, `5` settings. New: `I` picture in picture, `T` theatre, `C` comments, `[` `]` chapter, `<` `>` speed.

## Review Focus

1. **A video with no chapters, no description and no dates** (most Shorts, old uploads): the seek bar is one plain segment, the About tab says nothing rather than "undefined", counts are simply absent. Test in Task 6.
2. **A description or comment full of markup and odd links** (`<script>`, `javascript:` links, a 20 000-character line, right-to-left text): shown as plain text; only timestamps and YouTube links become actions. Test in Task 6 (`Linkify`).
3. **The engine dies while a video plays in picture in picture**: the small window closes, the main window shows the engine error, nothing is left playing. Test in Task 9.
4. **The window is narrow or short** (a tiled half or quarter of a 1080p screen, about 950 × 520): the side sheet moves under the video, the grid drops columns, nothing overlaps or clips. Test in Tasks 5 and 6.
5. **Typing in a field while shortcuts are active**: letters typed into search, the lens editor or settings never trigger `I`, `T`, `C`, `Q`, digits or brackets. Test in Task 4.

---

### Task 1: Spike: can the video move between windows?

A spike. Its code is thrown away; its output is the decision recorded at the end of this task.

**Files:** scratch only (`build/spike-pip/`, never committed), then this file (the decision line below).

- [ ] **Step 1:** Build a 60-line program: two `QQuickWindow`s from one QML file, one `MpvItem` playing a generated clip (`ffmpeg -f lavfi -i testsrc2=d=30:s=640x360:r=30 -f lavfi -i sine=d=30 clip.mkv`), a timer that re-parents the item to the other window's `contentItem` every 2 s, 20 times.
- [ ] **Step 2:** Run it inside `env -u WAYLAND_DISPLAY -u DISPLAY -u HYPRLAND_INSTANCE_SIGNATURE WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 cage -- ./spike`. After each move, 500 ms later, save `grabWindow()` of the window that holds the item.
- [ ] **Step 3:** It passes if, for all 20 moves: the grabbed frame is not a single colour, `time-pos` kept advancing across the move (no restart from 0), there was no crash and no mpv error line.
- [ ] **Step 4:** If it fails because the render context cannot be created while the old one exists, try once: free the old context from `MpvRenderer`'s destructor path before the new renderer's first `createFramebufferObject` (the existing `MpvShared` already frees on the render thread). Re-run.
- [ ] **Step 5: Record the decision** by replacing this line: **Decision: (B) a second item that reloads at the current position.** The spike (12 moves, headless cage) showed mpv keeps playing across a re-parent with no crash, but it could not confirm frames: the harness drew black even before the first move. (B) is chosen on design grounds: with (A) the old window's renderer, which owns mpv's one render context, is only freed when that window next renders, and a main window on another workspace may not render for minutes, so the small window could not create its context. (B) costs a short rebuffer (the relay seeks in about 0.1 s) and depends on nothing but `load` at a position. Task 9 implements whichever is written here. Commit only this file: "Record how picture in picture moves the video".

---

### Task 2: Theme, Style and the parts everything is built from

**Files:**
- Modify: `src/theme.h`, `src/theme.cpp` (colours and font), `src/main.cpp` (style "Basic", register `Style`), `src/resources.qrc`, `tests/qveil_tests.cpp`
- Create: `src/Style.qml` (singleton), `src/Act.qml`, `src/TabWord.qml`, `src/Field.qml`, `src/SectionHead.qml`, `src/Rule.qml`, `src/Glyph.qml`, `src/Bar.qml`, `src/Tip.qml`, `tests/parts_tests.cpp`
- Create icons: `src/icons/{home,lens,channels,history,settings,plus,pip,theatre,speed}.svg` (Material paths, as the existing ones)

**Interfaces:**
- Produces `Theme` properties (all `NOTIFY changed`): `background` (colors.toml `background`, fallback `#111115`), `foreground` (`foreground`, fallback `#e9e4f0`), `fontFamily` (the output of `fc-match -f %{family[0]} monospace`, fallback `monospace`; re-read when the theme directory changes or `~/.config/fontconfig` changes). Constructor gains `const QString &fcMatch = {}` for tests, like `hyprctl`.
- Produces `Style` (registered as `qmlRegisterSingletonType(QUrl("qrc:/Style.qml"), "Qveil", 1, 0, "Style")`; it reads the `theme` context property):
  - colours `bg`, `fg`, `dim` (fg at 0.62), `faint` (fg at 0.40), `rest`, `hover`, `sel`, `line`, `accent`, `safe` (`theme.protectedColor`), `warn` (`#e0a93b`), `scrim` (`#cc08070b`);
  - `font` (family), `caption` 11, `small` 12, `body` 13, `title` 16, `head` 18; `radius`; `u` 4; `rail` 76; `gutter` 24; `control` 28;
  - `function tint(alpha)` → fg at that alpha.
- Produces parts (each sets `objectName` from a `name` property where tests need it):
  - `Act { text; glyph: url; filled: bool; quiet: bool; tip: string; enabled; signal clicked() }` height `Style.control`; hover or key focus → `Style.hover`; `filled` rests on `Style.rest`; pressed scales to 0.97; `Enter`/`Space` click it.
  - `TabWord { text; glyph; current: bool; signal clicked() }` current → `Style.sel` and bold.
  - `Field { text; placeholder; hint: string; signal submitted(string); signal edited(string) }` rests on `Style.rest` with a `Style.line` border; `Esc` clears then drops focus.
  - `SectionHead { text; default property alias trailing }` small dim label, a rule to the right edge, trailing items at the far right.
  - `Rule {}` 1 px of `Style.line`.
  - `Glyph { source; color; size: 18 }` an SVG tinted with `MultiEffect` colorization.
  - `Bar { value; from; to; marks: var /* fractions 0..1 where a segment starts */; buffered: real; signal moved(real); readonly pressed }` segments separated by 2 px gaps; filled part `Style.accent`.
  - `Tip { text }` a small `Style.bg` box with a `Style.line` border, shown after 500 ms hover.

- [ ] **Step 1: Write the failing tests.** In `qveil_tests.cpp`:

```cpp
void themeReadsTheRoomAndTheFont()
{
    QTemporaryDir dir;
    QVERIFY(QDir().mkpath(dir.filePath("theme")));
    write(dir.filePath("theme/colors.toml"), "accent = \"#a08dff\"\nbackground = \"#151326\"\nforeground = \"#dad6ec\"\n");
    const QString fc = script(dir.filePath("fc-match"), "echo 'Test Mono'");
    Theme theme(dir.path(), nullptr, {}, {}, fc);
    QTRY_COMPARE(theme.fontFamily(), QString("Test Mono"));
    QCOMPARE(theme.background(), QString("#151326"));
    QCOMPARE(theme.foreground(), QString("#dad6ec"));
    QTemporaryDir empty;
    Theme bare(empty.path(), nullptr, {}, {}, "/nonexistent/fc-match");
    QCOMPARE(bare.background(), QString("#111115"));
    QCOMPARE(bare.foreground(), QString("#e9e4f0"));
    QCOMPARE(bare.fontFamily(), QString("monospace"));
}
```

In `parts_tests.cpp` (a new suite; helper `make(qml)` builds an item from a QML string with `theme` set and `Style` registered, in a 400 × 300 `QQuickWindow`):

```cpp
void anActIsAWordThatClicksByMouseAndKeyboard();   // click → 1 signal; forceActiveFocus + Key_Return → 2; disabled → still 2
void anActTakesTheHoverFillAndNothingRounder();    // hovered: color == Style.hover; radius == Style.radius for radius 0 and 6
void aFieldSubmitsOnEnterAndEscapeClearsThenLeaves(); // type "abc", Enter → submitted("abc"); Esc → text ""; Esc → !activeFocus
void aBarReportsWhereItWasPressedAndDrawsOneSegmentPerMark(); // marks [0, 0.25, 0.5] → 3 segments; press at 75 % → moved(≈ 0.75 * to)
void everyPartUsesTheOneTypeface();                // each part's Text children have font.family == Style.font
```

- [ ] **Step 2: Run** `bin/test`; expect the new tests to fail (or not compile).
- [ ] **Step 3: Implement.** `main.cpp`: `QQuickStyle::setStyle("Basic")`. Leave the old pages working for now: they keep their Material imports until the task that replaces them, so add `QT_QUICK_CONTROLS_MATERIAL_THEME`-free fallbacks only if a page fails to load.
- [ ] **Step 4: Run** `bin/test`; expect PASS.
- [ ] **Step 5: Commit** "Add the theme's room and typeface, and the parts the new window is built from".

---

### Task 3: Backend: details, comments, resume, lenses, readings

**Files:**
- Modify: `src/backend.h`, `src/backend.cpp`, `src/videomodel.h`, `src/videomodel.cpp`, `tests/engine_tests.cpp`, `tests/fake-ytdlp.sh`, `tests/videomodel_tests.cpp`

**Interfaces:**
- Consumes the engine's `api/comments`, `api/resume*`, `api/lenses*`, `api/lens`, `api/read`, and the new keys of `api/play` and `api/config`.
- Produces on `Backend`:

```cpp
Q_PROPERTY(QVariantList resume READ resume NOTIFY resumeChanged)          // [{id,pos,dur,at,title,channel,expires}]
Q_PROPERTY(QVariantMap comments READ comments NOTIFY commentsChanged)     // {video, sort, n, replies, count, items: [...]} or {}
Q_PROPERTY(QVariantList lenses READ lenses NOTIFY lensesChanged)          // api/lenses as it is
Q_PROPERTY(VideoModel *lensVideos READ lensVideos CONSTANT)
Q_PROPERTY(QVariantMap lensInfo READ lensInfo NOTIFY lensChanged)         // {id, text, tags, searches, fetchedAt}
Q_PROPERTY(int lensDone READ lensDone NOTIFY lensProgressChanged)
Q_PROPERTY(int lensTotal READ lensTotal NOTIFY lensProgressChanged)
Q_PROPERTY(QVariantMap reading READ reading NOTIFY readingChanged)        // {text, tags, searches, rules, error}

Q_INVOKABLE void loadResume();
Q_INVOKABLE void noteResume(const QVariantMap &video, double position, double duration); // video: id, title, channel
Q_INVOKABLE void forgetResume(const QString &id);
Q_INVOKABLE void clearResume();
Q_INVOKABLE void loadComments(const QString &video, const QString &sort, int n, bool replies);
Q_INVOKABLE void closeComments();          // hangs up on a running lookup and empties `comments`
Q_INVOKABLE void loadLenses();
Q_INVOKABLE void addLens(const QString &text);      // emits lensAdded(id) on success
Q_INVOKABLE void editLens(const QString &id, const QString &text);
Q_INVOKABLE void removeLens(const QString &id);
Q_INVOKABLE void openLens(const QString &id, int refresh); // 0 cache only, 1 if stale, 2 always
Q_INVOKABLE void read(const QString &text);        // empty text empties `reading` without a request
signals: void lensAdded(const QString &id);
```

- `VideoModel` gains the role `why` (`QStringList`, empty when absent).
- `privacyOf` passes `pages.resume`, `pages.comments`, `pages.lenses` through (empty string when missing).
- Busy task names: `comments`, `lens`, `lenses`, `read`, `resume`. `newestOnly` is true for `comments`, `lens` and `read`.
- `tests/fake-ytdlp.sh` learns: an argument `--write-comments` → kind `comments`, answering three threads (one pinned by the uploader, one with a reply when `max_comments` allows replies); `resolve` answers gain `"description":"About this video.\n0:00 Intro","view_count":59000,"timestamp":1762700000,"chapters":[{"start_time":0,"title":"Intro"},{"start_time":60,"title":"Middle"}]`.

- [ ] **Step 1: Write the failing tests** in `engine_tests.cpp` (each uses `Rig`):

```cpp
void thePlayAnswerCarriesTheDetails();
// play a video → playReady's map has description starting "About this video.", views 59000, chapters.size() == 2, chapters[1].title == "Middle".

void commentsLoadOnRequestAndClosingHangsUp();
// loadComments(id, "top", 40, false) → comments.items.size() == 3, items[0].pinned == true, count present;
// calls log has one "comments" line. With comments-delay = 5: loadComments then closeComments within 200 ms →
// comments is {} and busy no longer has "comments" within 1 s.

void aPositionIsNotedOfferedAndForgotten();
// noteResume({id,title:"T",channel:"C"}, 845, 2463) → resume.size() == 1, resume[0].pos == 845;
// play(id) → playReady map has resume == 845; forgetResume(id) → resume empty.

void lensesAreAddedOpenedAndRemoved();
// addLens("bread, sourdough") → lensAdded fires, lenses.size() == 1, lenses[0].searches == ["bread","sourdough"];
// openLens(id, 1) → lensTotal reaches 2, lensVideos->count() > 0, lensVideos->get(0).why[0] is "bread" or "sourdough";
// addLens("no music") → error is set and starts "Say what it should be about"; lenses.size() still 1;
// removeLens(id) → lenses empty.

void readingFollowsTheNewestTextOnly();
// read("talks over 30 min") → reading.rules == true, reading.tags.size() == 2;
// read("a"); read("ab"); read("abc") in one go → reading.text ends as "abc" and never shows a stale answer after it.
// read("") → reading is {} and no request was made.

void theEnginesNewPrivacyLinesReachTheWindow();
// backend.privacy()["pages"] has non-empty resume, comments and lenses.
```

In `videomodel_tests.cpp`: `whyIsARole` (an entry with `"why":["a","b"]` → role value `QStringList{"a","b"}`; without it → empty list).

- [ ] **Step 2: Run** `bin/test`; expect FAIL.
- [ ] **Step 3: Implement.** `openLens` reuses `stream()` with `api/lens?stream=1`: a `progress` line sets `lensDone/lensTotal`, the `result` line fills `lensVideos` (a new thumbnail page via `newPage`) and `lensInfo`. A change to lenses (`add`, `edit`, `remove`) reloads `lenses` from its answer path (`everyAnswer = true`, as subscriptions do).
- [ ] **Step 4: Run** `bin/test`; expect PASS.
- [ ] **Step 5: Commit** "Teach the window's backend about details, comments, resume and lenses".

---

### Task 4: The shell: rail, search, pages, keys, and a way to take stills

**Files:**
- Rewrite: `src/Main.qml`
- Create: `src/Rail.qml`, `src/SearchField.qml` (replaces `SearchBox.qml`), `src/ReadingTags.qml`, `src/ErrorBar.qml`, `src/Toast.qml`, `tests/shot.cpp`, `tests/shot.pro`, `tests/shots/engine.py`, `bin/shot`
- Modify: `src/resources.qrc`, `tests/qveil_tests.cpp`, `bin/test` (build `shot` too)
- Delete: `src/SearchBox.qml`

**Interfaces:**
- Produces in `Main.qml` (ids and object names later tasks and tests rely on): `win.page` one of `"home" | "lenses" | "channels" | "channel" | "history" | "search" | "settings"`; `win.watching: bool`; `win.go(page)`, `win.openChannel(id)`, `win.play(video, list /* VideoModel or null */, index)`, `win.goBack()`; `win.typing: bool`. Page hosts are `Loader`s named `page-<name>`; until their task lands they load the old page files unchanged, so the app stays usable after every task.
- `Rail { current: string; watching: bool; signal picked(string); signal routeClicked() }`: wordmark text "qveil" in `Style.accent`, bold; entries Home, Lenses, Channels, History (object names `rail-home` …); at the bottom the route indicator (`rail-route`: a dot and the word Direct / Tor / Proxy, amber on Direct, green otherwise, tooltip `backend.privacy.summary`) and Settings (`rail-settings`).
- `SearchField` (object name `search`): placeholder `qsTr("Search, or describe what you're after")`, hint `/`; while its text changes it calls `backend.read(text)` after 150 ms; `ReadingTags { tags: backend.reading.tags; shown: backend.reading.rules === true }` sits directly under it (object name `reading-tags`); Enter → `win.go("search")` and `backend.search(text)`.
- `bin/shot <view> <out.png> [width height]`: builds and runs `build/tests/qveil_shot` inside headless `cage` (`env -u WAYLAND_DISPLAY -u DISPLAY -u HYPRLAND_INSTANCE_SIGNATURE WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 cage -- …`, as in Task 1). `qveil_shot` starts `Backend` against `tests/shots/engine.py` (a stand-in engine that serves fixed JSON for every endpoint and a generated PNG for every thumbnail), loads `qrc:/Main.qml`, puts the window in the named view (`home`, `watch`, `watch-comments`, `lenses`, `search`, `channels`, `history`, `settings`, `pip`, `narrow`) by setting properties and calling the functions above, waits 1.5 s, saves `grabWindow()`, exits 0. Default size 1280 × 720; `narrow` is 950 × 520.

- [ ] **Step 1: Write the failing tests** in `qveil_tests.cpp` (replace `theMainWindowLoadsCleanAndSpeaksOnlyForItself` with the first two; keep its warning filter and its privacy and error-bar assertions, re-pointed at the new object names):

```cpp
void theWindowLoadsCleanWithARailAndNoTabBar();
// loads with no QML warning; finds rail-home, rail-lenses, rail-channels, rail-history, rail-route, rail-settings, search;
// clicking rail-channels → win.page == "channels"; rail-route's tooltip text == backend.privacy()["summary"].

void theRouteIndicatorSaysWhereYouStand();
// config route "direct" → its label is "Direct" and its colour is Style.warn; "tor" → "Tor", Style.safe.

void typingNeverTriggersAShortcut();
// focus `search`, QTest::keyClicks(window, "itc[q4 ") → text == "itc[q4 ", win.page unchanged, the app did not quit,
// win.watching unchanged. Then Esc twice → focus leaves the field; key "2" → win.page == "lenses".

void theSearchFieldShowsHowItWasReadBeforeEnter();
// with the contract engine answering api/read with two tags and rules true: type "talks over 30 min" →
// reading-tags becomes visible with 2 children; a plain "rust" (rules false) → hidden.
```

- [ ] **Step 2: Run** `bin/test`; expect FAIL.
- [ ] **Step 3: Implement.** Every `Shortcut` has `enabled: !win.typing` except `Esc`, `/` and `Ctrl+L`. The drop-to-import area, the error bar (with "Change route") and the toast move into their own files unchanged in behaviour.
- [ ] **Step 4: Run** `bin/test`, then `bin/shot home build/shots/home.png`; open the PNG and check it against the home still: rail on the left, search on top, no tab bar.
- [ ] **Step 5: Commit** "Rebuild the window around a rail, and add a way to take stills of it".

---

### Task 5: Cards, the frosted veil, and the home page

**Files:**
- Rewrite: `src/VideoCard.qml`, `src/VideoGrid.qml`
- Create: `src/Frost.qml`, `src/HomePage.qml`, `src/ResumeRow.qml`, `src/FeedTabs.qml`
- Delete: `src/DyeWash.qml`, `src/Wordmark.qml`, `src/shaders/dye.frag`, the shader block of `src/resources.pri`, `src/VideoPage.qml` once nothing loads it
- Modify: `src/Main.qml` (home loader), `src/resources.qrc`, `tests/qveil_tests.cpp`

**Interfaces:**
- `Frost { source: url; clear: bool; mode: "frosted" | "clear" }`: the image through `MultiEffect` with `blur: 1.0`, `blurMax: 48`, `saturation: -0.35`, `brightness: -0.08`, animated to none over 200 ms when `clear`. With `mode: "clear"` no effect is created.
- `VideoCard` roles as today plus `why`; shows thumbnail (`Frost`), length tag at the thumbnail's bottom right on `Style.scrim`, a 2 px `Style.accent` bar along the thumbnail's bottom when `progress > 0` (from `backend.resume`), title (bold, 2 lines), channel (`Style.dim`, clickable), date and views (`Style.faint`), and the `why` line in `Style.accent` when present. Hovered or keyboard-selected: thumbnail cleared and a 1 px `Style.fg` outline 2 px outside it.
- `VideoGrid { model; veil: string; signal activated(var video, int index); signal channelRequested(string) }`: columns = `max(1, floor(width / 280))`.
- `ResumeRow { entries: backend.resume }` (object name `resume-row`): hidden when empty; a `SectionHead` "Pick up where you stopped" with trailing `qsTr("kept %n days, then deleted", "", backend.config.resumeDays)`; up to 3 entries, each a `Style.rest` surface with thumbnail, title, channel, time left, an accent bar; click → `win.play`; a "Forget" `Act` on hover → `backend.forgetResume(id)`.
- `FeedTabs { lenses: backend.lenses; current: string /* "latest" or a lens id */; signal picked(string); signal newLens() }` (object name `feed-tabs`) plus trailing `Act`s "Shorts on/off" and "Refresh".
- `HomePage`: `ResumeRow`, `FeedTabs`, a `Rule`, then one `VideoGrid` showing `backend.feed` or `backend.lensVideos`. Picking a lens tab calls `backend.openLens(id, 1)`.
- Setting `prefs.thumbnails`: `"frosted"` (default; migrates from `veilThumbnails: true`), `"clear"`, `"off"` (maps to the engine's `thumbnails: false`).

- [ ] **Step 1: Write the failing tests:**

```cpp
void unfinishedVideosSitOnTopAndVanishWhenThereAreNone();
// engine with one resume entry → resume-row visible with 1 entry whose text contains "left"; after forgetResume → hidden.

void aLensTabShowsThatLensInTheGrid();
// two lenses → feed-tabs has Latest + 2 + New lens; click the first lens tab → backend.lensInfo.id == that id,
// the grid's model is backend.lensVideos, a card shows its why line.

void aCardClearsUnderThePointerAndUnderTheKeyboard();
// hover a card → its Frost.clear == true; move away → false; Right arrow in the grid → the selected card's Frost.clear == true.
// with prefs.thumbnails == "clear" → no MultiEffect child exists in any card.

void theGridDropsColumnsInANarrowWindow();
// window 1280 wide → 4 columns; 950 → 3; 520 → 1; no card's right edge passes the grid's width at any of them.

void nothingOfTheDyeIsLeft();
// QFile::exists is false for qrc:/DyeWash.qml, qrc:/Wordmark.qml and qrc:/shaders/dye.frag.qsb.
```

- [ ] **Step 2: Run** `bin/test`; expect FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `bin/test`; `bin/shot home build/shots/home.png` and `bin/shot narrow build/shots/narrow.png`; compare with the home still.
- [ ] **Step 5: Commit** "Draw the home page: unfinished videos, feed tabs and frosted cards".

---

### Task 6: The watch page and the player surface

**Files:**
- Create: `src/WatchPage.qml`, `src/PlayerSurface.qml` (replaces `PlayerView.qml`), `src/SeekBar.qml`, `src/SideSheet.qml`, `src/AboutTab.qml`, `src/UpNextTab.qml`, `src/Linkify.js`, `tests/linkify_tests.cpp`
- Modify: `src/mpvitem.h`, `src/mpvitem.cpp` (`speed`), `src/Main.qml`, `src/Format.js`, `tests/qveil_tests.cpp`
- Delete: `src/PlayerView.qml`

**Interfaces:**
- `MpvItem` gains `Q_PROPERTY(double speed READ speed WRITE setSpeed NOTIFY speedChanged)` (clamped 0.25..3).
- `Linkify.js` (`.pragma library`): `function parts(text)` → an array of `{ kind: "text" | "time" | "video" | "channel" | "link", text, seconds?, id?, url? }`. `time` is `m:ss` or `h:mm:ss` standing alone as a word; `video` is a `youtube.com/watch?v=`, `youtu.be/`, `youtube.com/shorts/` or `/live/` link (id = the 11 characters; a `t=` is ignored); `channel` is `youtube.com/channel/UC…` or `youtube.com/@handle`; `link` is any other `http(s)://` URL. `function html(text, accent)` → the same as escaped HTML for a `Text` with `textFormat: Text.StyledText`: `time`, `video` and `channel` become `<a href="qveil:time/<s>">`, `qveil:video/<id>`, `qveil:channel/<id or @handle>` in `accent`; `link` becomes `<a href="qveil:copy/<percent-encoded url>">`; everything else is escaped text.
- `PlayerSurface { info; pending; chrome; compact: bool /* true in the small window */; mpv (alias); function prepare(video); function open(info); function stop(); signal closed(); signal pipRequested(); signal theatreToggled(); signal fullscreenToggled(); signal channelRequested(string); function seekTo(seconds); readonly property real startedAt /* the start it last gave mpv.load, 0 if none */; readonly property real lastSeek /* the last seekTo target, -1 before any */ }`. Every seek that comes from a link, a chapter or "Start over" goes through `seekTo`, so tests can read what was asked without a video. It reports `backend.noteResume(info, mpv.position, mpv.duration)` every 15 s while playing and on pause, seek end and stop. When `info.resume > 0` it starts there and shows a tag "Resumed at m:ss" with an `Act` "Start over" (object name `resumed`) for 8 s.
- `SeekBar { mpv; chapters: var }` (object name `seek`): a `Bar` whose marks are the chapters' starts over the duration; beside the time it names the chapter as `n/N Title`.
- `WatchPage { info; list: var; index: int }` (object name `watch`): `PlayerSurface` (16:9, as wide as fits), title (`Style.head`, bold), channel row with a filled `Act` Subscribe / Subscribed and `views · date` in `Style.dim`, `SectionHead` "About" and the description collapsed to 3 lines (click toggles), and a `SideSheet` 312 wide on the right with tabs About, Comments, Up next (object names `tab-about`, `tab-comments`, `tab-next`). Below 1000 px of width the sheet moves under the description. `theatre: bool` hides the sheet and lets the video take the width.
- `AboutTab`: the whole description through `Linkify.html`, then a chapter list (time in `Style.faint`, title; the current one bold with its time in `Style.accent`; click seeks).
- `UpNextTab`: the entries of `list` after `index` (title, channel, length); click plays; empty text when there is no list.
- Link clicks: `qveil:time/…` seeks; `qveil:video/…` → `win.play({id})`; `qveil:channel/…` → `win.openChannel`; `qveil:copy/…` copies the URL and toasts `qsTr("Link copied. qveil doesn't open browsers.")`.

- [ ] **Step 1: Write the failing tests.** `linkify_tests.cpp` (evaluates `Linkify.js` in a `QJSEngine`):

```cpp
void timestampsAndYouTubeLinksBecomeActionsAndNothingElseDoes();
// "Intro 0:00, deep dive 1:02:03 https://youtu.be/dQw4w9WgXcQ?t=5 https://example.com/a?b=1 UCx"
// → kinds: text, time(0), text, time(3723), text, video(dQw4w9WgXcQ), text, link(https://example.com/a?b=1), text.
void markupIsShownNotRun();
// html("<script>alert(1)</script> <a href='javascript:x'>y</a> & 5 < 6", "#fff") contains "&lt;script&gt;" and "&amp;",
// and contains no "<script", no "javascript:" inside an href, and no <a> at all.
void aNumberThatOnlyLooksLikeATimeIsText();
// "ratio 16:9, score 3:2, at 12:40." → exactly one time (760); "1:99" is text.
void aVeryLongLineAndRightToLeftTextSurvive();
// 20 000 characters without a space → one text part of that length; a Hebrew sentence comes back unchanged.
```

In `qveil_tests.cpp`:

```cpp
void openingAVideoShowsTheWatchPageWithItsDetails();
// play a fixture video → watch visible; title text; the description's first line; seek has 2 segments; views text present.
void aVideoWithNothingButStreamsStillLooksWhole();
// playReady with only id/title/video → seek has 1 segment, no text anywhere contains "undefined" or "NaN",
// the About tab's chapter list is not visible.
void aKeptPositionIsResumedAndCanBeStartedOver();
// info.resume == 845 → the surface's startedAt == 845 and `resumed` is visible;
// click Start over → lastSeek == 0.
void theSheetMovesUnderTheVideoInANarrowWindow();
// 1280 wide → the sheet's x > the player's right edge; 950 wide → the sheet's y > the description's bottom; nothing overlaps.
void thePlayerClosesWhenTheEngineIsGone();   // the existing test, re-pointed at `watch`
```

- [ ] **Step 2: Run** `bin/test`; expect FAIL.
- [ ] **Step 3: Implement.** Controls row, left to right: play/pause, time, chapter, spacer, speed (`1x`, click cycles 1 → 1.25 → 1.5 → 2 → 0.75 → 1), volume, picture in picture, theatre, fullscreen. `Esc` leaves fullscreen, then theatre, then the page.
- [ ] **Step 4: Run** `bin/test`; `bin/shot watch build/shots/watch.png`; compare with the watch still.
- [ ] **Step 5: Commit** "Add the watch page: description, chapters on the seek bar, resume and up next".

---

### Task 7: Comments

**Files:**
- Create: `src/CommentsTab.qml`, `src/CommentItem.qml`
- Modify: `src/SideSheet.qml`, `tests/qveil_tests.cpp`

**Interfaces:**
- `CommentsTab { video: string; active: bool }` (object name `comments`): when it first becomes active for a video it calls `backend.loadComments(video, "top", 40, false)`; leaving the watch page calls `backend.closeComments()`. Header: `backend.privacy.pages.comments` in `Style.faint` (the engine's words), then `TabWord`s Top / Newest. Footer `Act`s: "More" (same call with `n + 40`, hidden at 200) and "Show replies" (same call with `replies: true`).
- `CommentItem`: `author · timeText` (`Style.faint`; the uploader's name in `Style.accent`, "pinned" when pinned), the text through `Linkify.html`, then `♥ likes` and replies indented by `Style.gutter` with a `Style.line` on their left. Items are divided by `Rule`s. No avatar.
- Empty answer: `qsTr("No comments here, or they are turned off.")`.

- [ ] **Step 1: Write the failing tests:**

```cpp
void commentsAreOnlyFetchedWhenTheTabIsOpened();
// open a video → the engine saw no comments request; click tab-comments → exactly one, 3 items shown,
// the first says "pinned"; switching to About and back makes no second request.
void moreAndRepliesAskAgain();
// More → a request with n=80; Show replies → replies=1 and an indented reply appears.
void aTimestampInACommentSeeksAndLeavingStopsTheLookup();
// click the link of "12:40" in a comment → the surface's lastSeek == 760; with a slow engine, Esc during loading →
// backend.busy has no "comments" within 1 s.
void theCommentsNoteIsTheEnginesOwnSentence();
// the header text == backend.privacy()["pages"]["comments"]; with that key empty the header is hidden.
```

- [ ] **Step 2: Run** `bin/test`; expect FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `bin/test`; `bin/shot watch-comments build/shots/comments.png`.
- [ ] **Step 5: Commit** "Show a video's comments in the side sheet, fetched when asked for".

---

### Task 8: Lenses and the search page

**Files:**
- Create: `src/LensesPage.qml`, `src/LensEditor.qml`, `src/SearchPage.qml`
- Modify: `src/Main.qml`, `src/HomePage.qml` ("New lens" opens the page with an empty editor), `tests/qveil_tests.cpp`

**Interfaces:**
- `LensesPage` (object name `lenses`): heading "Lenses", one line of explanation, then two columns. Left (300 wide): the lenses as `Style.rest` rows (`Style.sel` when current) showing the text and `N searches · N videos · time`, and an `Act` "New lens". Right: `LensEditor`, a `Rule`, the results grid (`backend.lensVideos`).
- `LensEditor { lens: var /* {} for a new one */ }`: a multi-line `Field` (object name `lens-text`, limit 500 characters, counter shown from 400); under it `ReadingTags` fed by `backend.read(text)` and "Read as"; a line from the engine (`backend.privacy.pages.lenses`) followed by the exact searches (`backend.reading.searches`, each quoted); `Act`s "Save" (new or changed text; `addLens` / `editLens`), "Refresh" (`openLens(id, 2)`), "Delete" (asks once in place: "Delete?" / "Cancel"). `backend.reading.error` shows in `Style.warn` and disables Save. During a refresh: `Refreshing %1 of %2` from `lensDone/lensTotal`.
- `SearchPage` (object name `search-page`): heading = the query, `ReadingTags` when the reading had rules, an `Act` "Keep as a lens" (object name `keep-lens`; `backend.addLens(query)`, then on `lensAdded` → `win.go("lenses")` with it current), then the grid of `backend.results`.

- [ ] **Step 1: Write the failing tests:**

```cpp
void aLensIsWrittenReadSavedAndShown();
// go("lenses"), New lens, type "bread, sourdough over 30 min" → tags appear (3), the searches line names "bread" and "sourdough";
// Save → the list has 1 row, it is current, the grid fills, each card has a why line.
void aLensWithoutATopicCannotBeSaved();
// type "no music" → the engine's sentence shows in Style.warn and Save is disabled.
void deletingAsksOnceInPlace();
// Delete → the lens is still there and the button reads "Delete?"; Cancel restores; Delete, Delete? → gone.
void aSearchCanBeKeptAsALens();
// search "talks over 30 min" → search-page shows 2 tags; click keep-lens → win.page == "lenses" and that lens is current.
void theLensNoteIsTheEnginesOwnSentence();
// the editor's note starts with backend.privacy()["pages"]["lenses"].
```

- [ ] **Step 2: Run** `bin/test`; expect FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `bin/test`; `bin/shot lenses build/shots/lenses.png` and `bin/shot search build/shots/search.png`; compare with the lens still.
- [ ] **Step 5: Commit** "Add the lenses page, and let a search be kept as a lens".

---

### Task 9: Picture in picture

**Files:**
- Create: `src/PipWindow.qml`, `src/NowPlayingBar.qml`
- Modify: `src/Main.qml`, `src/WatchPage.qml`, `src/PlayerSurface.qml`, `README.md` (the Hyprland rule), `tests/qveil_tests.cpp`

**Interfaces:**
- Consumes Task 1's decision. With (A) the one `PlayerSurface` is re-parented between `WatchPage` and `PipWindow.contentItem`. With (B) `PipWindow` holds a second `PlayerSurface` that is opened with the same `info` and `start = position`, after which the first is stopped; coming back does the reverse. Either way exactly one surface is playing at any time.
- `PipWindow` (object name `pip`): a `Window` with `title: "Picture-in-Picture"` (Omarchy's stock rule floats, pins and sizes it), 600 × 338, minimum 320 × 180, `flags: Qt.Window | Qt.WindowStaysOnTopHint`, colour black. Hovering shows play/pause, the seek bar, "Back to qveil" and "Close" as `Act`s on `Style.scrim`. Closing the window stops the video.
- `win.pip: bool`; `win.togglePip()`. Entering: the main window leaves the watch page for the page it came from and shows `NowPlayingBar` (object name `now-playing`) at the bottom of the content: title, channel, play/pause, "Back to qveil", "Close". `win.play(...)` while `pip` plays in the small window. `I` toggles; in the small window `Space`, arrows, `M`, `I` and `Esc` (back) work.
- README, under "The app": the two-line Hyprland rule for setups without Omarchy's `apps/pip.lua` (float, pin, size 600 338, for title `Picture-in-Picture`).

- [ ] **Step 1: Write the failing tests:**

```cpp
void pictureInPictureMovesTheVideoOutAndBack();
// play; press I → pip window visible with title "Picture-in-Picture", win.pip, the watch page is gone, now-playing visible
// with the video's title, and exactly one surface reports !paused. Press I again (or "Back to qveil") →
// pip hidden, watch visible, position within 2 s of where it was, still exactly one surface playing.
void anotherVideoOpenedMeanwhilePlaysInTheSmallWindow();
// in pip, activate a card → the pip surface's info.id is the new one; the main window stays on its page.
void closingTheSmallWindowStopsTheVideoAndSavesWhereItWas();
// close() on pip → no surface is loaded, now-playing hidden, backend saw a resume note for the video.
void theSmallWindowGoesWhenTheEngineDoes();
// in pip, kill the stand-in engine → pip hidden within 2 s, the error bar shows, nothing is loaded.
```

- [ ] **Step 2: Run** `bin/test`; expect FAIL.
- [ ] **Step 3: Implement** per the recorded decision.
- [ ] **Step 4: Run** `bin/test`; `bin/shot pip build/shots/pip.png` (grabs the small window) and check the video frame is not black. Then, in the user's session and only by asking them to press `I` on a playing video, confirm it floats and stays on top; do not send keys yourself.
- [ ] **Step 5: Commit** "Add picture in picture: the video in a small floating window while you browse".

---

### Task 10: The remaining pages, settings, and the old window's leftovers

**Files:**
- Create: `src/ChannelsPage.qml` (replaces `SubsPage.qml`), `src/ChannelPage.qml`, `src/SettingsPage.qml`
- Rewrite: `src/HistoryPage.qml`, `src/RoutePopup.qml`, `src/HelpOverlay.qml`, `src/EmptyState.qml`
- Delete: `src/SubsPage.qml`, `src/QuietList.qml`, `src/QuietRow.qml` if unused, every `import QtQuick.Controls.Material`
- Modify: `src/Main.qml`, `src/i18n/en.json`, `tests/qveil_tests.cpp`

**Interfaces:**
- `ChannelsPage`: a `Field` "Add a channel: @handle, link or ID", the channels as rows (name, an `Act` "Unsubscribe" that asks once in place), the import hint, and the engine's line `backend.privacy.pages.subscriptions`.
- `ChannelPage`: name, Subscribe / Subscribed, the grid of `backend.channelVideos`.
- `HistoryPage`: when on, the list (title, channel, when) with "Clear history" (asks once); when off, the engine's line `pages.history_off` and an `Act` "Turn on".
- `SettingsPage` (object name `settings`), sections under `SectionHead`s, every explanatory line from `backend.privacy`:
  - Route: Direct / Tor / Proxy as `TabWord`s with the engine's line for each, the proxy `Field`, `backend.privacy.points`.
  - History: on/off, the engine's line.
  - Resume (object name `set-resume`): Off / 1 / 7 / 30 days as `TabWord`s (a custom number in the config shows as its own tab), the engine's line `pages.resume`, "Forget all positions".
  - Thumbnails: Frosted / Clear / Off. Quality: 480 / 720 / 1080 / 1440 / 2160. Shorts: shown or hidden.
- `RoutePopup` keeps its behaviour, drawn with the new parts; "All settings" at its foot → `win.go("settings")`.
- `HelpOverlay` lists every key, including the new ones.

- [ ] **Step 1: Write the failing tests:**

```cpp
void settingsChangeTheEngineAndUseItsWords();
// click the 30 days tab under set-resume → backend.config.resumeDays == 30 and the line under it == privacy.pages.resume;
// Off → resumeDays == 0 and backend.resume is empty; no settings text contains a privacy word not from the engine
// (extend theWindowHasNoPrivacyClaimsOfItsOwn's scan to every new QML file).
void unsubscribingAndClearingAskOnce();
void everyPageLoadsCleanInBothDirections();
// for each of home, lenses, channels, channel, history, search, settings and watch: go there with layoutDirection LTR then RTL,
// no QML warning, and no item with visible text has width 0.
void noMaterialIsLeft();
// no file under qrc:/ contains "Controls.Material".
void theHelpListsEveryKeyTheWindowBinds();
// every Shortcut sequence in Main.qml appears in the help overlay's text.
```

- [ ] **Step 2: Run** `bin/test`; expect FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `bin/test`; take all stills with `bin/shot` into `build/shots/`.
- [ ] **Step 5: Commit** "Redraw channels, history and settings, and remove what the old window left".

---

### Task 11: Review rounds, README and the 0.4.0 version

**Files:**
- Modify: `README.md`, `docs/screenshot.png`, `core/Cargo.toml`, `core/Cargo.lock`, `pkgbuild/PKGBUILD`, `pkgbuild/PKGBUILD.local`

- [ ] **Step 1: Review rounds.** Run the `running-review-councils` skill on fresh `bin/shot` stills of every view (and the owner's stills as the brief): fresh blind seats each round, one seat judging identity against the owner's qnet, oc and Sound Hub panels, no score shared with any seat or implementer. Apply each round's fixes as their own commits. Stop when the chair's structural diagnosis has nothing left, or after three rounds, and show the owner the stills. The owner's rating decides; 8 is the target.
- [ ] **Step 2: README.** Rewrite "The app" (home, lenses, watch page, comments, resume, picture in picture, frosted thumbnails), the key table (all keys), the command line (`qveil comments`, `resume`, `read`, `lens …`), and "How it's built" (new modules and QML parts). Replace `docs/screenshot.png` with `bin/shot home`. The privacy section is already generated by the engine plan; `cargo test` confirms it.
- [ ] **Step 3: Version.** Set `0.4.0` in `core/Cargo.toml`, run `cargo build --manifest-path core/Cargo.toml` to update `Cargo.lock`, set `pkgver=0.4.0` in both PKGBUILDs. Remove `qt6-shadertools`-only build steps if the PKGBUILD had any for the dye shader.
- [ ] **Step 4: Verify.** `bin/test` passes; `bin/install` builds the package in its scratch directory (stop before the `sudo` step if it asks for a password and tell the owner to run `cd pkgbuild && makepkg -si`).
- [ ] **Step 5: Commit** "Release 0.4.0". Do not tag or push: the owner decides when.
