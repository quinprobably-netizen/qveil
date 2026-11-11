//! Thumbnails: fetched through the route, kept in memory, and, over a SOCKS
//! proxy, fetched a page at a time on one identity.
//!
//! A page of the window has dozens of thumbnails. Each on a circuit of its
//! own would be dozens of circuits built for a handful of cards, which is
//! slow, and a burst of requests for exactly the videos on a page, each
//! from a different exit. So the window gives the thumbnails of one page the
//! same `batch` token, and they share one identity (see `Net::pinned`): one
//! circuit per page instead of one per thumbnail.
//!
//! With `keep_thumbnails` on, what was fetched is also kept on disk (see
//! `Disk`), and read from there before it is fetched again.

use super::Server;
use super::http::{Request, respond, respond_cached};
use crate::app::Runtime;
use crate::ids;
use crate::inflight::ROUTE_CHANGED;
use crate::net::Net;
use crate::util::lock;
use anyhow::Result;
use reqwest::blocking::Client;
use std::collections::{HashMap, VecDeque};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const MAX_THUMBS: usize = 800;
/// How many pages' identities are kept. A page is in use while it is shown,
/// and the one before it while its last thumbnails arrive; older ones are never
/// asked for again, and one that is gets a new identity, which only costs a circuit.
const MAX_BATCHES: usize = 8;
/// The longest batch token (the shortest is 1).
const MAX_BATCH_TOKEN: usize = 32;

/// How much the thumbnails kept on disk may take up. A thumbnail is about
/// 15 KiB, so this is a few thousand of them.
const DISK_MAX: u64 = 64 << 20;

#[derive(Default)]
pub(super) struct Thumbs {
    pub(super) cache: Mutex<HashMap<String, Arc<Vec<u8>>>>,
    /// Those kept on disk.
    pub(super) disk: Disk,
    /// The identity of each page, oldest first.
    batches: Mutex<VecDeque<(String, Batch)>>,
}

struct Batch {
    /// The route it was made for: an identity of one route is no use on another.
    route: Net,
    /// Every request of this client uses one set of SOCKS credentials.
    client: Client,
}

/// A batch token is one to 32 letters and digits.
fn valid_batch(token: &str) -> bool {
    (1..=MAX_BATCH_TOKEN).contains(&token.len()) && token.bytes().all(|b| b.is_ascii_alphanumeric())
}

impl Thumbs {
    /// The client for a thumbnail of the page `batch`, if there is one. Only
    /// over a route that gives each request an identity does a page need one
    /// of its own: elsewhere everything leaves from the same address anyway,
    /// and the shared client reuses its connections.
    fn client(&self, rt: &Runtime, batch: Option<&str>) -> Result<Client> {
        let Some(token) = batch.filter(|_| rt.net.isolates()) else {
            return rt.web_client();
        };
        let mut batches = lock(&self.batches);
        if let Some((_, b)) = batches.iter().find(|(t, _)| t == token)
            && b.route.same_route(&rt.net)
        {
            return Ok(b.client.clone());
        }
        batches.retain(|(t, _)| t != token);
        let client = rt.net.pinned().client()?;
        batches.push_back((
            token.to_string(),
            Batch {
                route: rt.net.clone(),
                client: client.clone(),
            },
        ));
        while batches.len() > MAX_BATCHES {
            batches.pop_front();
        }
        Ok(client)
    }

    /// Keeps a thumbnail in memory.
    fn remember(&self, id: &str, bytes: Arc<Vec<u8>>) {
        let mut cache = lock(&self.cache);
        if cache.len() >= MAX_THUMBS {
            cache.clear();
        }
        cache.insert(id.to_string(), bytes);
    }

    /// Forgets every page's identity, as a route change does.
    pub(super) fn forget_batches(&self) {
        lock(&self.batches).clear();
    }
}

/// The thumbnails kept on disk: one file per video, `<id>.jpg`, mode 0600 in
/// a 0700 directory, each written atomically. At most `max` bytes in all; past
/// that, the least recently used go first. Use is told by the file's
/// modification time, which a read sets, so it carries over a restart.
pub(super) struct Disk {
    max: u64,
    /// What is in the directory, once it has been looked at. Also held while
    /// writing and deleting, so that a write can't recreate what was deleted.
    index: Mutex<Option<Index>>,
}

impl Default for Disk {
    fn default() -> Self {
        Disk {
            max: DISK_MAX,
            index: Mutex::new(None),
        }
    }
}

#[derive(Default)]
struct Index {
    /// Each file's size, and when it was last used (a count, not a time).
    files: HashMap<String, (u64, u64)>,
    total: u64,
    clock: u64,
}

impl Index {
    /// What is in `dir`, the least recently used first.
    fn scan(dir: &Path) -> Index {
        let mut found: Vec<(std::time::SystemTime, String, u64)> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                let id = name
                    .strip_suffix(".jpg")
                    .filter(|id| ids::is_video_id(id))?;
                let meta = e.metadata().ok().filter(|m| m.is_file())?;
                Some((meta.modified().ok()?, id.to_string(), meta.len()))
            })
            .collect();
        found.sort();
        let mut index = Index::default();
        for (_, id, size) in found {
            index.clock += 1;
            index.total += size;
            index.files.insert(id, (size, index.clock));
        }
        index
    }

    fn used(&mut self, id: &str, size: u64) {
        self.clock += 1;
        if let Some((old, _)) = self.files.insert(id.to_string(), (size, self.clock)) {
            self.total -= old;
        }
        self.total += size;
    }

    fn remove(&mut self, id: &str) {
        if let Some((size, _)) = self.files.remove(id) {
            self.total -= size;
        }
    }
}

fn file(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.jpg"))
}

impl Disk {
    fn index<'a>(&self, slot: &'a mut Option<Index>, dir: &Path) -> &'a mut Index {
        slot.get_or_insert_with(|| Index::scan(dir))
    }

    /// The thumbnail of `id`, if it is kept in `dir`.
    fn read(&self, dir: &Path, id: &str) -> Option<Vec<u8>> {
        let mut slot = lock(&self.index);
        let path = file(dir, id);
        let Ok(bytes) = std::fs::read(&path) else {
            // Deleted behind our back (`qveil wipe`, say), or never there.
            self.index(&mut slot, dir).remove(id);
            return None;
        };
        self.index(&mut slot, dir).used(id, bytes.len() as u64);
        // So that it is used lately after a restart too.
        if let Ok(f) = std::fs::File::options().append(true).open(&path) {
            let _ = f.set_modified(std::time::SystemTime::now());
        }
        Some(bytes)
    }

    /// Keeps `bytes` as the thumbnail of `id` in `dir`, unless `still` says
    /// (under the lock that deleting takes) that keeping them is off now,
    /// and makes room by deleting the least recently used.
    fn write(
        &self,
        dir: &Path,
        id: &str,
        bytes: &[u8],
        still: impl FnOnce() -> bool,
    ) -> Result<()> {
        let mut slot = lock(&self.index);
        if !still() {
            return Ok(());
        }
        crate::store::write_atomic(&file(dir, id), bytes)?;
        let index = self.index(&mut slot, dir);
        index.used(id, bytes.len() as u64);
        while index.total > self.max {
            let Some(oldest) = index
                .files
                .iter()
                .min_by_key(|(_, (_, used))| *used)
                .map(|(id, _)| id.clone())
            else {
                break;
            };
            let _ = std::fs::remove_file(file(dir, &oldest));
            index.remove(&oldest);
        }
        Ok(())
    }

    /// Deletes every thumbnail kept in `dir`, and the directory.
    pub(super) fn forget(&self, dir: &Path) -> Result<()> {
        let mut slot = lock(&self.index);
        *slot = None;
        match std::fs::remove_dir_all(dir) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(anyhow::Error::new(e).context(format!("deleting {}", dir.display())))
            }
            _ => Ok(()),
        }
    }
}

impl Server {
    /// `GET thumb/<id>`, with `?batch=<token>` for the page it is on.
    pub(super) fn thumb(&self, out: &mut TcpStream, req: &Request, id: &str) -> Result<()> {
        if !ids::is_video_id(id) {
            return respond(out, 404, "text/plain", b"not found");
        }
        // A token that isn't one is the window's mistake, and said so, rather
        // than quietly fetching without the page's identity.
        let batch = match req.query.get("batch") {
            None => None,
            Some(token) if valid_batch(token) => Some(token.as_str()),
            Some(_) => return respond(out, 400, "text/plain", b"bad batch"),
        };
        if let Some(bytes) = lock(&self.thumbs.cache).get(id).cloned() {
            return respond_cached(out, &bytes);
        }
        let rt = self.runtime();
        let dir = self.paths.thumbs();
        if rt.cfg.keep_thumbnails
            && let Some(bytes) = self.thumbs.disk.read(&dir, id)
        {
            let bytes = Arc::new(bytes);
            self.thumbs.remember(id, Arc::clone(&bytes));
            return respond_cached(out, &bytes);
        }
        if !rt.cfg.thumbnails {
            return respond(out, 403, "text/plain", b"thumbnails are off");
        }
        let url = format!("{}/vi/{id}/mqdefault.jpg", rt.thumb_base);
        let fetched = self
            .thumbs
            .client(&rt, batch)
            .and_then(|c| Ok(c.get(url).send()?.error_for_status()?.bytes()?));
        // A thumbnail that arrives after the route changed is neither shown nor
        // kept: it was fetched over a route the user left.
        let fetched = if self.still_on(&rt) {
            fetched
        } else {
            return respond(out, 503, "text/plain", ROUTE_CHANGED.as_bytes());
        };
        match fetched {
            Ok(bytes) => {
                let bytes = Arc::new(bytes.to_vec());
                self.thumbs.remember(id, Arc::clone(&bytes));
                let sent = respond_cached(out, &bytes);
                if rt.cfg.keep_thumbnails {
                    // Only while it is still on: turning it off deletes them.
                    let still = || self.runtime().cfg.keep_thumbnails;
                    if let Err(e) = self.thumbs.disk.write(&dir, id, &bytes, still) {
                        note!("qveil: couldn't keep a thumbnail on disk: {e:#}");
                    }
                }
                sent
            }
            Err(e) => respond(out, 502, "text/plain", format!("{e:#}").as_bytes()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::server::testkit::*;
    use crate::util::lock;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    #[test]
    fn a_thumbnail_that_arrives_after_a_route_switch_is_dropped_and_not_cached() {
        let up = Upstream::start();
        let base_url = up.url("");
        let (s, base, _d) = server_tuned(Config::default(), |rt| {
            rt.thumb_base = base_url.trim_end_matches('/').to_string();
        });
        let thumb = |base: &str| http(base, "GET", &path(base, "thumb/dQw4w9WgXcQ"), "");

        // The control: on an unchanged route it is served and kept.
        let (status, body) = thumb(&base);
        assert_eq!((status, body.as_str()), (200, "thumb!"));
        assert_eq!(lock(&s.thumbs.cache).len(), 1);
        lock(&s.thumbs.cache).clear();

        up.gauge.hold.store(true, Ordering::SeqCst);
        let late = {
            let base = base.clone();
            std::thread::spawn(move || thumb(&base))
        };
        wait_for("the thumbnail request", Duration::from_secs(10), || {
            up.gauge.waiting.load(Ordering::SeqCst) == 1
        });
        switch_to_tor(&base);
        up.gauge.hold.store(false, Ordering::SeqCst);
        let (status, body) = late.join().unwrap();
        assert_eq!(
            (status, body.as_str()),
            (503, "The route changed; try again")
        );
        assert!(
            lock(&s.thumbs.cache).is_empty(),
            "a late thumbnail must not be cached"
        );
    }

    fn mode(p: &std::path::Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    /// A server whose thumbnails come from `up`, with `cfg`. A settings
    /// change would put the real thumbnail host back, so the tests below
    /// fetch nothing after one.
    fn thumbs_from(
        up: &Upstream,
        cfg: Config,
    ) -> (Arc<Server>, String, crate::store::testing::TempDir) {
        let base_url = up.url("");
        server_tuned(cfg, |rt| {
            rt.thumb_base = base_url.trim_end_matches('/').to_string();
        })
    }

    fn keeping() -> Config {
        Config {
            keep_thumbnails: true,
            ..Config::default()
        }
    }

    #[test]
    fn thumbnails_are_not_kept_on_disk_unless_that_is_on() {
        let up = Upstream::start();
        let (s, base, _d) = thumbs_from(&up, Config::default());
        let (status, body) = http(&base, "GET", &path(&base, "thumb/dQw4w9WgXcQ"), "");
        assert_eq!((status, body.as_str()), (200, "thumb!"));
        assert!(!s.paths.thumbs().exists());
    }

    #[test]
    fn kept_thumbnails_are_shown_from_disk_and_go_when_keeping_them_is_turned_off() {
        let up = Upstream::start();
        let (s, base, _d) = thumbs_from(&up, keeping());
        let thumb = || http(&base, "GET", &path(&base, "thumb/dQw4w9WgXcQ"), "");
        let dir = s.paths.thumbs();
        assert_eq!(thumb(), (200, "thumb!".to_string()));
        let kept = dir.join("dQw4w9WgXcQ.jpg");
        assert_eq!(std::fs::read(&kept).unwrap(), b"thumb!");
        assert_eq!((mode(&dir), mode(&kept)), (0o700, 0o600));
        // Shown again with the memory empty (as after a restart): from disk, not fetched.
        let fetched = up.requests().len();
        lock(&s.thumbs.cache).clear();
        assert_eq!(thumb(), (200, "thumb!".to_string()));
        assert_eq!(up.requests().len(), fetched);
        // Turned off, they are deleted.
        let set = http(
            &base,
            "POST",
            &path(&base, "api/config"),
            r#"{"keepThumbnails":false}"#,
        );
        assert_eq!(set.0, 200);
        assert!(!dir.exists());
    }

    #[test]
    fn clearing_the_history_deletes_the_kept_thumbnails() {
        let up = Upstream::start();
        let (s, base, _d) = thumbs_from(&up, keeping());
        http(&base, "GET", &path(&base, "thumb/dQw4w9WgXcQ"), "");
        assert!(s.paths.thumbs().join("dQw4w9WgXcQ.jpg").exists());
        assert_eq!(
            http(&base, "POST", &path(&base, "api/history/clear"), "").0,
            200
        );
        assert!(!s.paths.thumbs().exists());
    }

    #[test]
    fn kept_thumbnails_left_from_when_it_was_on_go_at_startup() {
        let mut left = None;
        let (s, _base, _d) = server_tuned(Config::default(), |rt| {
            let dir = rt.yt.cache_dir.parent().unwrap().join("thumbs");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("dQw4w9WgXcQ.jpg"), b"x").unwrap();
            left = Some(dir);
        });
        assert_eq!(left.unwrap(), s.paths.thumbs());
        assert!(!s.paths.thumbs().exists());
    }

    #[test]
    fn the_disk_keeps_at_most_its_size_and_lets_the_least_recently_used_go() {
        let root = crate::store::testing::TempDir::new();
        let dir = root.0.join("thumbs");
        let disk = || Disk {
            max: 30,
            index: Mutex::new(None),
        };
        let one = disk();
        let id = |c: char| c.to_string().repeat(11);
        let put = |d: &Disk, c: char| d.write(&dir, &id(c), &[c as u8; 10], || true).unwrap();
        let there = |c: char| file(&dir, &id(c)).exists();
        for c in ['a', 'b', 'c'] {
            put(&one, c);
        }
        assert_eq!(one.read(&dir, &id('a')).unwrap(), [b'a'; 10]);
        put(&one, 'd');
        assert_eq!(
            ['a', 'b', 'c', 'd'].map(there),
            [true, false, true, true],
            "b was used least lately"
        );
        // After a restart, use is told by the files' times.
        let two = disk();
        put(&two, 'e');
        assert_eq!(['a', 'c', 'd', 'e'].map(there), [true, false, true, true]);
        // What isn't wanted any more isn't written.
        two.write(&dir, &id('f'), b"f", || false).unwrap();
        assert!(!there('f'));
        assert!(two.read(&dir, &id('f')).is_none());
        two.forget(&dir).unwrap();
        assert!(!dir.exists());
        two.forget(&dir).unwrap();
    }

    use crate::store::testing::socks_spy;

    /// A server on an isolating SOCKS route whose proxy records the user name
    /// of each connection and refuses it: every thumbnail then fails, which
    /// is beside the point, since what is looked at is who it asked as.
    fn on_a_spy() -> (
        Arc<Server>,
        String,
        crate::store::testing::TempDir,
        std::sync::Arc<Mutex<Vec<String>>>,
    ) {
        let (port, users) = socks_spy();
        let (s, base, dir) = server_tuned(
            Config {
                proxy: Some(format!("socks5h://127.0.0.1:{port}")),
                ..Config::default()
            },
            |rt| rt.thumb_base = "http://thumbs.invalid".into(),
        );
        (s, base, dir, users)
    }

    /// Asks for thumbnail `n` (any ID will do) of a page, or of none.
    fn thumb_of(base: &str, n: u32, batch: Option<&str>) -> u16 {
        let query = batch.map(|b| format!("?batch={b}")).unwrap_or_default();
        http(
            base,
            "GET",
            &path(base, &format!("thumb/th{n:09}{query}")),
            "",
        )
        .0
    }

    #[test]
    fn the_thumbnails_of_one_page_share_an_identity_and_each_page_has_its_own() {
        let (_s, base, _d, users) = on_a_spy();
        for (n, batch) in [
            (1, Some("pageA")),
            (2, Some("pageA")),
            (3, Some("pageB")),
            (4, Some("pageA")),
            (5, Some("pageB")),
            // Without a page, as before: each on its own.
            (6, None),
            (7, None),
        ] {
            assert_eq!(thumb_of(&base, n, batch), 502, "{n}");
        }
        let users = lock(&users).clone();
        assert_eq!(users.len(), 7);
        let (a, b) = (&users[0], &users[2]);
        assert_ne!(a, b, "pages share nothing");
        assert_eq!([&users[1], &users[3]], [a, a], "page A");
        assert_eq!(&users[4], b, "page B");
        let alone: std::collections::HashSet<_> = users[5..].iter().collect();
        assert_eq!(alone.len(), 2, "without a batch each is its own");
        assert!(!alone.contains(a) && !alone.contains(b));
    }

    #[test]
    fn only_the_last_few_pages_are_remembered() {
        let (s, base, _d, users) = on_a_spy();
        for page in 0..=MAX_BATCHES {
            thumb_of(&base, page as u32, Some(&format!("page{page}")));
        }
        assert_eq!(lock(&s.thumbs.batches).len(), MAX_BATCHES);
        let seen = lock(&users).len();
        // The newest is still known, and the oldest has been forgotten and
        // gets an identity of its own again.
        thumb_of(&base, 100, Some(&format!("page{MAX_BATCHES}")));
        thumb_of(&base, 101, Some("page0"));
        let users = lock(&users).clone();
        assert_eq!(users.len(), seen + 2);
        assert_eq!(users[MAX_BATCHES], users[seen]);
        assert_ne!(users[0], users[seen + 1]);
        assert_eq!(lock(&s.thumbs.batches).len(), MAX_BATCHES);
    }

    #[test]
    fn a_route_change_forgets_the_pages() {
        let (s, base, _d, _users) = on_a_spy();
        thumb_of(&base, 1, Some("pageA"));
        assert_eq!(lock(&s.thumbs.batches).len(), 1);
        let (status, body) = http(
            &base,
            "POST",
            &path(&base, "api/config"),
            r#"{"route":"direct"}"#,
        );
        assert_eq!(status, 200, "{body}");
        assert!(lock(&s.thumbs.batches).is_empty());
    }

    #[test]
    fn a_batch_is_one_to_32_letters_and_digits() {
        for good in ["a", "A1", "0", &"x".repeat(32)] {
            assert!(valid_batch(good), "{good}");
        }
        for bad in [
            "",
            &"x".repeat(33),
            "a-b",
            "a_b",
            "a b",
            "../x",
            "é",
            "a/b",
            "a%20b",
        ] {
            assert!(!valid_batch(bad), "{bad:?}");
        }
        // The server says so, rather than quietly ignoring it.
        let (_s, base, _d) = server();
        for bad in ["", "a-b", &"x".repeat(33)] {
            let (status, body) = http(
                &base,
                "GET",
                &path(&base, &format!("thumb/dQw4w9WgXcQ?batch={bad}")),
                "",
            );
            assert_eq!((status, body.as_str()), (400, "bad batch"), "{bad:?}");
        }
    }

    #[test]
    fn a_route_that_gives_no_identities_has_no_use_for_pages() {
        // Direct: everything leaves from one address anyway, and the shared
        // client is used, with or without a batch.
        let up = Upstream::start();
        let base_url = up.url("");
        let (s, base, _d) = server_tuned(Config::default(), |rt| {
            rt.thumb_base = base_url.trim_end_matches('/').to_string();
        });
        let (status, body) = http(
            &base,
            "GET",
            &path(&base, "thumb/dQw4w9WgXcQ?batch=pageA"),
            "",
        );
        assert_eq!((status, body.as_str()), (200, "thumb!"));
        assert!(lock(&s.thumbs.batches).is_empty());
    }
}
