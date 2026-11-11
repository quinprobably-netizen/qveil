//! What the server's tests share: servers on a loopback port, a fake
//! googlevideo, fake yt-dlps, and ways to talk to them.

use super::*;
use crate::config::Config;
use crate::store::testing::TempDir;
use relay::CHUNK;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

pub fn server() -> (Arc<Server>, String, TempDir) {
    server_with(Config::default())
}

/// A server whose clients may talk plain http, so a local upstream can
/// stand in for googlevideo.
pub fn server_with(cfg: Config) -> (Arc<Server>, String, TempDir) {
    server_tuned(cfg, |_| {})
}

/// `server_with`, after `tune` has changed the runtime (to point its
/// thumbnails at a local upstream, say).
pub fn server_tuned(
    cfg: Config,
    tune: impl FnOnce(&mut Runtime),
) -> (Arc<Server>, String, TempDir) {
    let dir = TempDir::new();
    let paths = Paths {
        config: dir.0.join("config.toml"),
        data: dir.0.join("data"),
        cache: dir.0.join("cache"),
    };
    let mut rt = Runtime::new(cfg, &paths, None, false).unwrap();
    rt.net = rt.net.allowing_http();
    rt.yt.net = rt.net.clone();
    tune(&mut rt);
    let s = Server::new(rt, paths);
    // Most tests count what the upstream is asked; the ones about warming
    // turn it back on.
    s.warming.store(false, Ordering::Relaxed);
    s.sparing.store(false, Ordering::Relaxed);
    let base = s.listen().unwrap();
    (s, base, dir)
}

/// A server whose clients speak HTTP/2 to a local upstream (see `h2up`).
pub fn server_h2c() -> (Arc<Server>, String, TempDir) {
    server_tuned(Config::default(), |rt| {
        rt.net = rt.net.clone().speaking_h2c();
        rt.yt.net = rt.net.clone();
    })
}

pub fn addr(base: &str) -> &str {
    base.trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap()
}

pub fn http(base: &str, method: &str, path: &str, body: &str) -> (u16, String) {
    let mut c = TcpStream::connect(addr(base)).unwrap();
    write!(
        c,
        "{method} {path} HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut resp = String::new();
    c.read_to_string(&mut resp).unwrap();
    let status = resp[9..12].parse().unwrap();
    (status, resp.split_once("\r\n\r\n").unwrap().1.to_string())
}

pub fn path(base: &str, rest: &str) -> String {
    format!("/{}/{rest}", base.rsplit('/').next().unwrap())
}

/// Sends raw bytes and returns the status line's code (0 if none came
/// back). The server may hang up before reading everything, so write and
/// read errors are ignored: what matters is what the client got.
pub fn raw(base: &str, request: &[u8]) -> u16 {
    let mut c = TcpStream::connect(addr(base)).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let _ = c.write_all(request);
    let mut resp = Vec::new();
    let _ = c.read_to_end(&mut resp);
    std::str::from_utf8(resp.get(9..12).unwrap_or_default())
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

/// 100 bytes of `abc…zabc…`, easy to recognise a range of.
pub fn payload() -> Vec<u8> {
    (0..100u8).map(|i| b'a' + i % 26).collect()
}

pub type Seen = Arc<Mutex<Vec<(String, Option<String>)>>>;

/// What the upstream's endless streams are doing.
#[derive(Default)]
pub struct Gauge {
    /// Streams still being written.
    pub live: AtomicUsize,
    /// Bytes written so far, over all of them.
    pub sent: AtomicUsize,
    /// How many more `/big` responses to cut off half-way.
    pub cuts: Mutex<usize>,
    /// While set, a thumbnail (`/vi/…`) isn't answered, so that a test can
    /// do something (switch routes) while the request is in flight.
    pub hold: AtomicBool,
    /// How many thumbnail requests are waiting on `hold`.
    pub waiting: AtomicUsize,
    /// While set, a `/big?gated=1` response stops after its first 16 KiB and
    /// sends the rest once it is cleared (or after 30 s).
    pub gate: AtomicBool,
}

/// A tiny stand-in for googlevideo on a local port. It records every
/// request target and Range header it gets.
pub struct Upstream {
    pub addr: std::net::SocketAddr,
    pub seen: Seen,
    pub gauge: Arc<Gauge>,
}

impl Upstream {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Seen::default();
        let log = Arc::clone(&seen);
        let gauge = Arc::new(Gauge::default());
        let counted = Arc::clone(&gauge);
        std::thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                let (log, gauge) = (Arc::clone(&log), Arc::clone(&counted));
                std::thread::spawn(move || upstream_serve(conn, &log, &gauge));
            }
        });
        Upstream { addr, seen, gauge }
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    pub fn requests(&self) -> Vec<(String, Option<String>)> {
        self.seen.lock().unwrap().clone()
    }
}

pub fn upstream_serve(
    mut conn: TcpStream,
    log: &Mutex<Vec<(String, Option<String>)>>,
    gauge: &Gauge,
) {
    let mut r = BufReader::new(conn.try_clone().unwrap());
    let mut line = String::new();
    r.read_line(&mut line).unwrap();
    let target = line.split_whitespace().nth(1).unwrap_or("/").to_string();
    let head_only = line.starts_with("HEAD");
    let mut range = None;
    loop {
        let mut h = String::new();
        r.read_line(&mut h).unwrap();
        if h.trim().is_empty() {
            break;
        }
        if let Some(v) = h.to_ascii_lowercase().strip_prefix("range:") {
            range = Some(v.trim().to_string());
        }
    }
    log.lock().unwrap().push((target.clone(), range.clone()));
    let body = payload();
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    if path == "/big" {
        return big(conn, query, range.as_deref(), head_only, gauge);
    }
    if path.starts_with("/vi/") {
        gauge.waiting.fetch_add(1, Ordering::SeqCst);
        let started = Instant::now();
        while gauge.hold.load(Ordering::SeqCst) && started.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(10));
        }
        gauge.waiting.fetch_sub(1, Ordering::SeqCst);
        let _ = write!(
            conn,
            "HTTP/1.1 200 OK\r\nContent-Type: image/jpeg\r\nContent-Length: 6\r\nConnection: close\r\n\r\n"
        );
        let _ = conn.write_all(b"thumb!");
        return;
    }
    if path == "/endless" {
        // A video that never ends, 16 KiB at a time, `pace` ms apart, until
        // the other side hangs up.
        let pace: u64 = query
            .strip_prefix("pace=")
            .and_then(|p| p.parse().ok())
            .unwrap_or(0);
        let _ = write!(
            conn,
            "HTTP/1.1 200 OK\r\nContent-Type: video/mp4\r\nContent-Length: 1000000000000\r\nConnection: close\r\n\r\n"
        );
        gauge.live.fetch_add(1, Ordering::SeqCst);
        let chunk = [b'x'; 16 << 10];
        while conn.write_all(&chunk).is_ok() {
            gauge.sent.fetch_add(chunk.len(), Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(pace));
        }
        gauge.live.fetch_sub(1, Ordering::SeqCst);
        return;
    }
    let mut send = |status: &str, headers: &str, body: &[u8], claimed: usize| {
        let _ = write!(
            conn,
            "HTTP/1.1 {status}\r\n{headers}Content-Length: {claimed}\r\nConnection: close\r\n\r\n"
        );
        if !head_only {
            let _ = conn.write_all(body);
        }
    };
    match path {
        // Like a URL from a retired Tor circuit: `gen=1` is the first
        // resolve of a stream, and anything under `stale=` is never good.
        "/video" if query == "gen=1" || query.starts_with("stale=") => {
            send("403 Forbidden", "", b"denied", 6)
        }
        "/video" => match range.as_deref().and_then(|r| r.strip_prefix("bytes=")) {
            Some(r) => {
                let (a, b) = r.split_once('-').unwrap();
                let (a, b): (usize, usize) = (a.parse().unwrap(), b.parse().unwrap());
                send(
                    "206 Partial Content",
                    &format!("Content-Type: video/mp4\r\nContent-Range: bytes {a}-{b}/100\r\n"),
                    &body[a..=b],
                    b - a + 1,
                );
            }
            None => send("200 OK", "Content-Type: video/mp4\r\n", &body, 100),
        },
        // A manifest, as its own type unless `type=` says another, after
        // a byte order mark and some white space with `pad=1`, and after an
        // XML declaration with `xml=1`. A Range from the start gets a 206.
        "/hls" | "/dash" => {
            let param = |k: &str| {
                query
                    .split('&')
                    .find_map(|kv| kv.strip_prefix(k)?.strip_prefix('='))
            };
            let (own, text) = if path == "/hls" {
                (
                    "application/vnd.apple.mpegurl",
                    "#EXTM3U\nhttps://rr1.googlevideo.com/seg1.ts\n",
                )
            } else {
                (
                    "application/dash+xml",
                    "<MPD><BaseURL>https://rr1.googlevideo.com/</BaseURL></MPD>",
                )
            };
            let mut doc = Vec::new();
            if param("pad").is_some() {
                doc.extend_from_slice(b"\xEF\xBB\xBF \r\n\t");
            }
            if param("xml").is_some() {
                doc.extend_from_slice(b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
            }
            doc.extend_from_slice(text.as_bytes());
            let ctype = param("type").unwrap_or(own);
            let len = doc.len();
            match range.as_deref() {
                Some(r) if r.starts_with("bytes=0-") => send(
                    "206 Partial Content",
                    &format!(
                        "Content-Type: {ctype}\r\nContent-Range: bytes 0-{}/{len}\r\n",
                        len - 1
                    ),
                    &doc,
                    len,
                ),
                _ => send("200 OK", &format!("Content-Type: {ctype}\r\n"), &doc, len),
            }
        }
        // Promises 100 bytes, sends 10 and hangs up.
        "/cut" => send("200 OK", "Content-Type: video/mp4\r\n", &body[..10], 100),
        _ => send("404 Not Found", "", b"", 0),
    }
}

/// The size of `/big` unless asked otherwise: two chunks and a bit, so a
/// whole read crosses two chunk boundaries.
pub const BIG: u64 = 2 * CHUNK + 12_345;

/// Byte `i` of `/big`. 251 is prime, so the pattern never lines up with
/// a chunk boundary and a byte lost or repeated there shows.
pub fn big_byte(i: u64) -> u8 {
    (i % 251) as u8
}

pub fn big_bytes(from: u64, to: u64) -> Vec<u8> {
    (from..to).map(big_byte).collect()
}

/// A long video on the fake googlevideo, answering Range requests like
/// the real one. In the query: `size=` its length, `ttfb=` ms before the
/// response head (a round trip through Tor), `pace=` ms between 16 KiB
/// writes, `gen=1` a URL that has gone stale once the first chunk
/// is through (a retired Tor circuit), `stale=` one that is never good
/// again after that, and `dead=1` one whose connections die at once after
/// the first chunk. `hold=B` leaves a request for a range that ends at byte
/// `B` unanswered for ten seconds, and `cut=B` stops one half-way. `Gauge::cuts`
/// makes that many responses stop half-way, and `gated=1` holds what follows
/// the first 16 KiB of a response behind `Gauge::gate`.
pub fn big(mut conn: TcpStream, query: &str, range: Option<&str>, head_only: bool, gauge: &Gauge) {
    big_to(&mut conn, query, range, head_only, gauge, true);
}

/// `big`, on a connection that is kept open afterwards unless `close`.
/// False when the connection is no good for another request.
fn big_to(
    conn: &mut TcpStream,
    query: &str,
    range: Option<&str>,
    head_only: bool,
    gauge: &Gauge,
    close: bool,
) -> bool {
    let connection = if close { "close" } else { "keep-alive" };
    let param = |k: &str| {
        query
            .split('&')
            .find_map(|kv| kv.strip_prefix(k)?.strip_prefix('='))
    };
    let size: u64 = param("size").and_then(|s| s.parse().ok()).unwrap_or(BIG);
    let pace: u64 = param("pace").and_then(|s| s.parse().ok()).unwrap_or(0);
    if let Some(ms) = param("ttfb").and_then(|s| s.parse().ok()) {
        std::thread::sleep(Duration::from_millis(ms));
    }
    let (a, b) = match range.and_then(|r| r.strip_prefix("bytes=")) {
        Some(r) => {
            let (a, b) = r.split_once('-').unwrap();
            let a: u64 = a.parse().unwrap();
            let b = b.parse().map_or(size - 1, |b: u64| b.min(size - 1));
            (a, b)
        }
        None => (0, size - 1),
    };
    let ends_at = |k: &str| param(k).and_then(|s| s.parse::<u64>().ok()) == Some(b);
    if ends_at("hold") {
        std::thread::sleep(Duration::from_secs(10));
        return false;
    }
    let past_first = a > 0;
    if past_first && (param("gen") == Some("1") || param("stale").is_some()) {
        let _ = write!(
            conn,
            "HTTP/1.1 403 Forbidden\r\nContent-Length: 6\r\nConnection: {connection}\r\n\r\ndenied"
        );
        return !close;
    }
    if a >= size {
        let _ = write!(
            conn,
            "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{size}\r\nContent-Length: 0\r\nConnection: {connection}\r\n\r\n"
        );
        return !close;
    }
    let status = if range.is_some() {
        format!("206 Partial Content\r\nContent-Range: bytes {a}-{b}/{size}")
    } else {
        "200 OK".to_string()
    };
    let _ = write!(
        conn,
        "HTTP/1.1 {status}\r\nContent-Type: video/mp4\r\nContent-Length: {}\r\nConnection: {connection}\r\n\r\n",
        b - a + 1
    );
    if head_only {
        return !close;
    }
    let mut stop = b + 1;
    if past_first && param("dead").is_some() {
        stop = a;
    }
    let cut = {
        let mut cuts = gauge.cuts.lock().unwrap();
        let cut = *cuts > 0;
        *cuts = cuts.saturating_sub(1);
        cut
    };
    if cut || ends_at("cut") {
        stop = a + (b + 1 - a) / 2;
    }
    gauge.live.fetch_add(1, Ordering::SeqCst);
    let mut at = a;
    while at < stop {
        let n = (16 << 10).min(stop - at);
        let block = big_bytes(at, at + n);
        if conn.write_all(&block).is_err() {
            gauge.live.fetch_sub(1, Ordering::SeqCst);
            return false;
        }
        gauge.sent.fetch_add(block.len(), Ordering::SeqCst);
        at += n;
        if at == a + n && param("gated").is_some() {
            let started = Instant::now();
            while gauge.gate.load(Ordering::SeqCst) && started.elapsed() < Duration::from_secs(30) {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        if pace > 0 {
            std::thread::sleep(Duration::from_millis(pace));
        }
    }
    gauge.live.fetch_sub(1, Ordering::SeqCst);
    !close && stop == b + 1
}

/// A stand-in for googlevideo as it really answers: HTTP/1.1, keeping each
/// connection open for the next request, where a new connection costs
/// `dial` before its first answer (the TCP and TLS handshakes through a Tor
/// circuit). It serves `/big` (see `big`), and counts its connections.
pub struct KeepAlive {
    pub addr: std::net::SocketAddr,
    seen: Seen,
    connections: Arc<AtomicUsize>,
    /// The Range header of each request, with the number of the connection
    /// (from 0) it came on.
    on: OnConnection,
}

type OnConnection = Arc<Mutex<Vec<(usize, Option<String>)>>>;

impl KeepAlive {
    pub fn start(dial: Duration) -> Self {
        Self::start_dialling(move |_| dial)
    }

    /// `start`, where the `n`th connection (from 0) costs `dial(n)`.
    pub fn start_dialling(dial: impl Fn(usize) -> Duration + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (seen, gauge) = (Seen::default(), Arc::new(Gauge::default()));
        let connections = Arc::new(AtomicUsize::new(0));
        let on = OnConnection::default();
        let (log, counted, count, on_log) = (
            Arc::clone(&seen),
            Arc::clone(&gauge),
            Arc::clone(&connections),
            Arc::clone(&on),
        );
        std::thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                let n = count.fetch_add(1, Ordering::SeqCst);
                let dial = dial(n);
                let (log, gauge, on) =
                    (Arc::clone(&log), Arc::clone(&counted), Arc::clone(&on_log));
                std::thread::spawn(move || {
                    std::thread::sleep(dial);
                    keep_alive_serve(conn, &log, &gauge, |range| {
                        on.lock().unwrap().push((n, range));
                    });
                });
            }
        });
        KeepAlive {
            addr,
            seen,
            connections,
            on,
        }
    }

    /// The Range header of each request, in order, with the number of the
    /// connection it came on (from 0, in the order they were made).
    pub fn ranges_on(&self) -> Vec<(usize, Option<String>)> {
        self.on.lock().unwrap().clone()
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    pub fn connections(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }

    /// The Range headers it has been sent, in order.
    pub fn ranges(&self) -> Vec<String> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter_map(|r| r.1.clone())
            .collect()
    }
}

fn keep_alive_serve(
    mut conn: TcpStream,
    log: &Mutex<Vec<(String, Option<String>)>>,
    gauge: &Gauge,
    on: impl Fn(Option<String>),
) {
    let mut r = BufReader::new(conn.try_clone().unwrap());
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let target = line.split_whitespace().nth(1).unwrap_or("/").to_string();
        let head_only = line.starts_with("HEAD");
        let mut range = None;
        loop {
            let mut h = String::new();
            if r.read_line(&mut h).unwrap_or(0) == 0 {
                return;
            }
            if h.trim().is_empty() {
                break;
            }
            if let Some(v) = h.to_ascii_lowercase().strip_prefix("range:") {
                range = Some(v.trim().to_string());
            }
        }
        log.lock().unwrap().push((target.clone(), range.clone()));
        on(range.clone());
        let query = target.split_once('?').map_or("", |(_, q)| q);
        if !big_to(&mut conn, query, range.as_deref(), head_only, gauge, false) {
            return;
        }
    }
}

/// Asks the relay for `range` of `stream` and returns the response head
/// and the socket, with the time it took for the first byte of the
/// body to arrive.
pub fn first_byte(base: &str, stream: &str, range: &str) -> (String, Duration, Vec<u8>, TcpStream) {
    let mut c = TcpStream::connect(addr(base)).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let started = Instant::now();
    write!(
        c,
        "GET {} HTTP/1.1\r\nHost: x\r\nRange: {range}\r\n\r\n",
        path(base, stream)
    )
    .unwrap();
    let mut got = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = c.read(&mut buf).unwrap();
        assert!(n > 0, "the relay hung up");
        got.extend_from_slice(&buf[..n]);
        if let Some(at) = got.windows(4).position(|w| w == b"\r\n\r\n")
            && got.len() > at + 4
        {
            let head = String::from_utf8_lossy(&got[..at]).into_owned();
            return (head, started.elapsed(), got[at + 4..].to_vec(), c);
        }
    }
}

/// Reads from `c` until `have` holds `n` bytes.
pub fn read_up_to(c: &mut TcpStream, have: &mut Vec<u8>, n: usize) {
    let mut buf = vec![0u8; 64 << 10];
    while have.len() < n {
        let got = c.read(&mut buf).unwrap();
        assert!(got > 0, "the relay hung up after {} bytes", have.len());
        have.extend_from_slice(&buf[..got]);
    }
}

pub type Fetched = (u16, HashMap<String, String>, Vec<u8>);

/// A request with extra headers; returns the status, the lowercased
/// response headers and the body bytes.
pub fn fetch(base: &str, method: &str, rest: &str, headers: &[(&str, &str)]) -> Fetched {
    let mut c = TcpStream::connect(addr(base)).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let extra: String = headers
        .iter()
        .map(|(k, v)| format!("{k}: {v}\r\n"))
        .collect();
    let target = path(base, rest);
    write!(c, "{method} {target} HTTP/1.1\r\nHost: x\r\n{extra}\r\n").unwrap();
    let mut raw = Vec::new();
    let _ = c.read_to_end(&mut raw);
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("no response head");
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let mut lines = head.lines();
    let status = lines.next().unwrap()[9..12].parse().unwrap();
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    (status, headers, raw[split + 4..].to_vec())
}

pub fn test_video() -> VideoRef {
    VideoRef {
        id: "dQw4w9WgXcQ".into(),
        start: None,
    }
}

/// Polls until `cond` holds, or panics after `within`.
pub fn wait_for(what: &str, within: Duration, mut cond: impl FnMut() -> bool) {
    let started = Instant::now();
    while !cond() {
        assert!(started.elapsed() < within, "gave up waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Opens a relayed stream like a player would, and returns the socket once
/// the response head has arrived.
pub fn start_stream(base: &str, sid: &str) -> TcpStream {
    let mut c = TcpStream::connect(addr(base)).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write!(
        c,
        "GET {} HTTP/1.1\r\nHost: x\r\nRange: bytes=0-\r\n\r\n",
        path(base, &format!("s/{sid}/v"))
    )
    .unwrap();
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        let mut b = [0u8; 1];
        assert_eq!(c.read(&mut b).unwrap(), 1, "the head ended early");
        head.push(b[0]);
    }
    assert!(head.starts_with(b"HTTP/1.1 200"), "{head:?}");
    c
}

pub fn switch_to_tor(base: &str) {
    let (status, body) = http(
        base,
        "POST",
        &path(base, "api/config"),
        r#"{"route":"tor"}"#,
    );
    assert_eq!(status, 200, "{body}");
}

/// A yt-dlp that records its process id and the private directory of its
/// files, then hangs: a search or lookup still waiting on YouTube.
pub fn hanging_ytdlp(dir: &std::path::Path) -> String {
    crate::store::testing::script(
        &dir.join("yt-dlp"),
        r#"d=$(dirname "$0")
# Each is written whole or not at all (the process id last): a test may be reading them meanwhile.
while [ $# -gt 0 ]; do
  [ "$1" = --config-locations ] && dirname "$2" > "$d/private-dir.new" && mv "$d/private-dir.new" "$d/private-dir"
  shift
done
echo $$ > "$d/pid.new" && mv "$d/pid.new" "$d/pid"
exec sleep 60
"#,
    )
}

/// The process id and private directory that `hanging_ytdlp` recorded,
/// once it has recorded both.
pub fn hanging_started(dir: &std::path::Path) -> Option<(String, std::path::PathBuf)> {
    let pid = std::fs::read_to_string(dir.join("pid")).ok()?;
    let private = std::fs::read_to_string(dir.join("private-dir")).ok()?;
    (pid.ends_with('\n') && private.ends_with('\n'))
        .then(|| (pid.trim().to_string(), private.trim().into()))
}

/// Waits until `hanging_ytdlp` is running, and returns what it recorded.
pub fn hanging_running(dir: &std::path::Path) -> (String, std::path::PathBuf) {
    let mut found = None;
    wait_for("yt-dlp to start", Duration::from_secs(10), || {
        found = hanging_started(dir);
        found.is_some()
    });
    found.unwrap()
}

pub fn assert_stopped_and_cleaned_up(pid: &str, private: &std::path::Path) {
    wait_for("yt-dlp to be gone", Duration::from_secs(5), || {
        !std::path::Path::new("/proc").join(pid).exists()
    });
    wait_for(
        "its private files to be removed",
        Duration::from_secs(5),
        || !private.exists(),
    );
}

/// A yt-dlp that answers with a URL on the local upstream, numbered by how
/// often it has run (`<url>1`, `<url>2`, …), and counts its runs.
pub fn fake_ytdlp(dir: &std::path::Path, url: &str) -> String {
    let script = r#"n=$(cat '@COUNT@' 2>/dev/null || echo 0)
n=$((n + 1))
# Whole or not at all: a test may be reading it meanwhile.
echo $n > '@COUNT@.new' && mv '@COUNT@.new' '@COUNT@'
cat <<EOF
{"title":"T","requested_formats":[
 {"url":"@URL@$n","protocol":"https","vcodec":"avc1"},
 {"url":"@URL@$n","protocol":"https","vcodec":"none"}]}
EOF
"#
    .replace("@COUNT@", &dir.join("runs").display().to_string())
    .replace("@URL@", url);
    crate::store::testing::script(&dir.join("yt-dlp"), &script)
}

pub fn runs(dir: &std::path::Path) -> u32 {
    std::fs::read_to_string(dir.join("runs"))
        .map(|s| s.trim().parse().unwrap())
        .unwrap_or(0)
}

/// The path of a relay URL below the server's base.
pub fn rel<'a>(base: &str, url: &'a str) -> &'a str {
    url.strip_prefix(&format!("{base}/")).unwrap()
}

pub fn watched() -> Watched {
    Watched {
        id: "dQw4w9WgXcQ".into(),
        title: "T".into(),
        channel: None,
        at: 0,
    }
}

/// The Range headers the upstream has been sent, in order.
pub fn ranges(up: &Upstream) -> Vec<String> {
    up.requests().into_iter().filter_map(|r| r.1).collect()
}

/// Like `start_stream`, but expects a chunked relay's 206.
pub fn start_ranged(base: &str, sid: &str) -> TcpStream {
    let mut c = TcpStream::connect(addr(base)).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write!(
        c,
        "GET {} HTTP/1.1\r\nHost: x\r\nRange: bytes=0-\r\n\r\n",
        path(base, &format!("s/{sid}/v"))
    )
    .unwrap();
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        let mut b = [0u8; 1];
        assert_eq!(c.read(&mut b).unwrap(), 1, "the head ended early");
        head.push(b[0]);
    }
    assert!(head.starts_with(b"HTTP/1.1 206"), "{head:?}");
    c
}

/// Reads until the relay closes the connection, which must happen within 5 s.
pub fn wait_for_close(player: &mut TcpStream) {
    let started = Instant::now();
    let mut buf = vec![0u8; 256 << 10];
    loop {
        match player.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => assert!(
                started.elapsed() < Duration::from_secs(5),
                "the stream carries on"
            ),
        }
    }
}

/// Waits until the upstream has had no request and sent no byte for a
/// while, and returns how many requests it had.
pub fn settled(up: &Upstream) -> usize {
    let snapshot = || (up.requests().len(), up.gauge.sent.load(Ordering::SeqCst));
    let mut last = snapshot();
    wait_for("the upstream to go quiet", Duration::from_secs(5), || {
        std::thread::sleep(Duration::from_millis(300));
        let now = snapshot();
        std::mem::replace(&mut last, now) == now
    });
    last.0
}

/// A TCP relay in front of `upstream` whose connections can be frozen: kept
/// open with nothing more passed on either way, as a Tor circuit that
/// stalls does. Connections made after a freeze pass bytes as usual.
pub struct Freezer {
    pub addr: std::net::SocketAddr,
    connections: Arc<AtomicUsize>,
    /// One flag per connection made so far.
    frozen: Arc<Mutex<Vec<Arc<AtomicBool>>>>,
}

impl Freezer {
    pub fn start(upstream: std::net::SocketAddr) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let connections = Arc::new(AtomicUsize::new(0));
        let frozen: Arc<Mutex<Vec<Arc<AtomicBool>>>> = Arc::default();
        let (count, flags) = (Arc::clone(&connections), Arc::clone(&frozen));
        std::thread::spawn(move || {
            for client in listener.incoming().flatten() {
                let Ok(server) = TcpStream::connect(upstream) else {
                    continue;
                };
                count.fetch_add(1, Ordering::SeqCst);
                let flag = Arc::new(AtomicBool::new(false));
                flags.lock().unwrap().push(Arc::clone(&flag));
                for (from, to) in [
                    (client.try_clone().unwrap(), server.try_clone().unwrap()),
                    (server, client),
                ] {
                    let flag = Arc::clone(&flag);
                    std::thread::spawn(move || pass_on(from, to, &flag));
                }
            }
        });
        Freezer {
            addr,
            connections,
            frozen,
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    /// Freezes every connection there is now.
    pub fn freeze(&self) {
        for flag in self.frozen.lock().unwrap().iter() {
            flag.store(true, Ordering::SeqCst);
        }
    }

    pub fn connections(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }
}

/// Copies `from` to `to` until either end closes, or the connection is
/// frozen: then it holds both open, and passes nothing on, for as long as
/// the test runs.
fn pass_on(mut from: TcpStream, mut to: TcpStream, frozen: &AtomicBool) {
    from.set_read_timeout(Some(Duration::from_millis(10)))
        .unwrap();
    let mut buf = vec![0u8; 16 << 10];
    let hold = |held: (TcpStream, TcpStream)| -> ! {
        let _held = held;
        loop {
            std::thread::sleep(Duration::from_secs(60));
        }
    };
    loop {
        if frozen.load(Ordering::SeqCst) {
            hold((from, to));
        }
        match from.read(&mut buf) {
            Ok(0) => return,
            // What arrived while it froze is not passed on either.
            Ok(_) if frozen.load(Ordering::SeqCst) => hold((from, to)),
            Ok(n) => {
                if to.write_all(&buf[..n]).is_err() {
                    return;
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => return,
        }
    }
}

/// A stand-in for googlevideo as it is reached over Tor: a SOCKS5 proxy
/// whose user name is the identity (Tor makes a circuit for each), in
/// front of a server that answers `/big` (see `big`) over HTTP/1.1 with
/// keep-alive, as googlevideo did over Tor, measured. A URL names the
/// identity it was resolved for (`who=`, see `gate_ytdlp`), and is refused
/// with a 403 over any other, as googlevideo refuses an address other than
/// the one that resolved it. Its connections can be frozen (a circuit that
/// stalls) or reset.
/// For each request to a `Gate`: the identity it came over, the one its URL
/// names, and its Range header.
type GateSeen = Arc<Mutex<Vec<(String, String, Option<String>)>>>;
/// A `Gate`'s connections: a flag that freezes each, and its socket on the
/// player's side.
type GateConns = Arc<Mutex<Vec<(Arc<AtomicBool>, TcpStream)>>>;

pub struct Gate {
    pub port: u16,
    /// For each request: the identity it came over, the one its URL names,
    /// and its Range header.
    seen: GateSeen,
    /// The connections made so far, to freeze or reset.
    conns: GateConns,
    pub gauge: Arc<Gauge>,
}

impl Gate {
    pub fn start() -> Self {
        let inner = TcpListener::bind("127.0.0.1:0").unwrap();
        let inner_addr = inner.local_addr().unwrap();
        let seen = GateSeen::default();
        let gauge = Arc::new(Gauge::default());
        {
            let (seen, gauge) = (Arc::clone(&seen), Arc::clone(&gauge));
            std::thread::spawn(move || {
                for conn in inner.incoming().flatten() {
                    let (seen, gauge) = (Arc::clone(&seen), Arc::clone(&gauge));
                    std::thread::spawn(move || gate_serve(conn, &seen, &gauge));
                }
            });
        }
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let conns = GateConns::default();
        let held = Arc::clone(&conns);
        std::thread::spawn(move || {
            for mut client in listener.incoming().flatten() {
                let Some(user) = socks_handshake(&mut client) else {
                    continue;
                };
                let Ok(mut server) = TcpStream::connect(inner_addr) else {
                    continue;
                };
                // The server is told the identity first.
                if writeln!(server, "{user}").is_err() {
                    continue;
                }
                let flag = Arc::new(AtomicBool::new(false));
                held.lock()
                    .unwrap()
                    .push((Arc::clone(&flag), client.try_clone().unwrap()));
                for (from, to) in [
                    (client.try_clone().unwrap(), server.try_clone().unwrap()),
                    (server, client),
                ] {
                    let flag = Arc::clone(&flag);
                    std::thread::spawn(move || pass_on(from, to, &flag));
                }
            }
        });
        Gate {
            port,
            seen,
            conns,
            gauge,
        }
    }

    pub fn proxy(&self) -> String {
        format!("socks5h://127.0.0.1:{}", self.port)
    }

    /// Freezes every connection there is now.
    pub fn freeze(&self) {
        for (flag, _) in self.conns.lock().unwrap().iter() {
            flag.store(true, Ordering::SeqCst);
        }
    }

    /// Resets every connection there is now, as a circuit that breaks does.
    pub fn reset(&self) {
        for (_, client) in self.conns.lock().unwrap().drain(..) {
            reset(&client);
        }
    }

    /// The identities that requests came over, each once, in order.
    pub fn identities(&self) -> Vec<String> {
        let mut found: Vec<String> = Vec::new();
        for (who, _, _) in self.seen.lock().unwrap().iter() {
            if !found.contains(who) {
                found.push(who.clone());
            }
        }
        found
    }

    /// How many requests came over an identity other than their URL's
    /// (and were refused).
    pub fn refused(&self) -> usize {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|(over, of, _)| over != of)
            .count()
    }

    pub fn requests(&self) -> Vec<(String, String, Option<String>)> {
        self.seen.lock().unwrap().clone()
    }
}

/// Closes `conn` with a TCP reset rather than an orderly close.
fn reset(conn: &TcpStream) {
    use std::os::fd::AsRawFd;
    let linger = libc::linger {
        l_onoff: 1,
        l_linger: 0,
    };
    // SAFETY: a valid socket, and a pointer to a linger of the right size.
    unsafe {
        libc::setsockopt(
            conn.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_LINGER,
            (&linger as *const libc::linger).cast(),
            std::mem::size_of::<libc::linger>() as libc::socklen_t,
        );
    }
    let _ = conn.shutdown(std::net::Shutdown::Both);
}

/// The server side of a SOCKS5 handshake with a user name and password,
/// for a CONNECT to anywhere. Returns the user name.
fn socks_handshake(c: &mut TcpStream) -> Option<String> {
    let mut head = [0u8; 2];
    c.read_exact(&mut head).ok()?;
    let mut methods = vec![0u8; head[1] as usize];
    c.read_exact(&mut methods).ok()?;
    c.write_all(&[5, 2]).ok()?;
    let mut len = [0u8; 2];
    c.read_exact(&mut len).ok()?;
    let mut user = vec![0u8; len[1] as usize];
    c.read_exact(&mut user).ok()?;
    let mut plen = [0u8; 1];
    c.read_exact(&mut plen).ok()?;
    let mut pass = vec![0u8; plen[0] as usize];
    c.read_exact(&mut pass).ok()?;
    c.write_all(&[1, 0]).ok()?;
    // Version, CONNECT, reserved, and the address type.
    let mut req = [0u8; 4];
    c.read_exact(&mut req).ok()?;
    let skip = match req[3] {
        1 => 4,
        4 => 16,
        _ => {
            let mut n = [0u8; 1];
            c.read_exact(&mut n).ok()?;
            n[0] as usize
        }
    };
    let mut rest = vec![0u8; skip + 2];
    c.read_exact(&mut rest).ok()?;
    c.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).ok()?;
    Some(String::from_utf8_lossy(&user).into_owned())
}

/// `keep_alive_serve` for the `Gate`: the first line is the identity the
/// connection came over.
fn gate_serve(
    mut conn: TcpStream,
    seen: &Mutex<Vec<(String, String, Option<String>)>>,
    gauge: &Gauge,
) {
    let mut r = BufReader::new(conn.try_clone().unwrap());
    let mut over = String::new();
    if r.read_line(&mut over).unwrap_or(0) == 0 {
        return;
    }
    let over = over.trim().to_string();
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let target = line.split_whitespace().nth(1).unwrap_or("/").to_string();
        let head_only = line.starts_with("HEAD");
        let mut range = None;
        loop {
            let mut h = String::new();
            if r.read_line(&mut h).unwrap_or(0) == 0 {
                return;
            }
            if h.trim().is_empty() {
                break;
            }
            if let Some(v) = h.to_ascii_lowercase().strip_prefix("range:") {
                range = Some(v.trim().to_string());
            }
        }
        let query = target.split_once('?').map_or("", |(_, q)| q);
        let of = query
            .split('&')
            .find_map(|kv| kv.strip_prefix("who="))
            .unwrap_or_default()
            .to_string();
        seen.lock()
            .unwrap()
            .push((over.clone(), of.clone(), range.clone()));
        if of != over {
            let _ = write!(
                conn,
                "HTTP/1.1 403 Forbidden\r\nContent-Length: 6\r\nConnection: keep-alive\r\n\r\ndenied"
            );
            continue;
        }
        if !big_to(&mut conn, query, range.as_deref(), head_only, gauge, false) {
            return;
        }
    }
}

/// A yt-dlp that answers with both streams at `url` on a `Gate`, for the
/// identity it was given (the user name of its proxy), and counts its runs.
pub fn gate_ytdlp(dir: &std::path::Path, url: &str) -> String {
    let script = r#"n=$(cat '@COUNT@' 2>/dev/null || echo 0)
n=$((n + 1))
# Whole or not at all: a test may be reading it meanwhile.
echo $n > '@COUNT@.new' && mv '@COUNT@.new' '@COUNT@'
while [ $# -gt 0 ]; do [ "$1" = --config-locations ] && conf="$2"; shift; done
who=$(sed -n "s|.*--proxy 'socks5h://\([^:]*\):.*|\1|p" "$conf")
cat <<EOF
{"title":"T","requested_formats":[
 {"url":"@URL@&who=$who&n=$n","protocol":"https","vcodec":"avc1"},
 {"url":"@URL@&who=$who&n=$n","protocol":"https","vcodec":"none"}]}
EOF
"#
    .replace("@COUNT@", &dir.join("runs").display().to_string())
    .replace("@URL@", url);
    crate::store::testing::script(&dir.join("yt-dlp"), &script)
}
