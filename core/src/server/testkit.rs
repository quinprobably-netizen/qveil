//! What the server's tests share: servers on a loopback port, a fake
//! googlevideo, fake yt-dlps, and ways to talk to them.

use super::*;
use crate::config::Config;
use crate::store::testing::TempDir;
use relay::CHUNK;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};
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
    fetch_target(base, method, &path(base, rest), headers)
}

/// `fetch` for a request target as it is, with no relay's path in front.
pub fn fetch_target(base: &str, method: &str, target: &str, headers: &[(&str, &str)]) -> Fetched {
    let mut c = TcpStream::connect(addr(base)).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let extra: String = headers
        .iter()
        .map(|(k, v)| format!("{k}: {v}\r\n"))
        .collect();
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

/// A stand-in for googlevideo serving a live stream, on a local port. Its
/// head grows by one segment every `target_ms`; each segment is 64 KiB of
/// its own name (see `live_segment`), and `X-Head-Seqnum` says how far it
/// has got. What it answers:
///
/// - `/live/{generation}/{track}` (`v` or `a`): a HEAD, or any request without
///   `&sq=`, is sent on (302) to `/live2/{generation}/{track}`, which is 200 with
///   `X-Head-Seqnum` and no body. A request with `&sq=n` (after the path, as
///   a Track's base takes it, or after a `?`) is segment `n`, in `/live` as
///   in `/live2`, with `X-Head-Seqnum` and `X-Sequence-Num`.
/// - A segment below the first there is, below `drop_before`, or above the
///   head is a 404.
/// - A generation refused by `forbid_until_relooked` is a 403, whatever the
///   request.
/// - A segment asked for with `Range: bytes=a-` is a 206 of its bytes from
///   `a` on.
///
/// The generation is the one in the URL that `fake_live_ytdlp` printed: one
/// more for each lookup. Every request target is kept (`requests`).
pub struct LiveUpstream {
    pub base: String,
    live: Arc<LiveShared>,
}

/// What a `LiveUpstream`'s threads share.
struct LiveShared {
    target_ms: u64,
    first: u64,
    head: u64,
    began: Instant,
    /// The head, once the stream has ended.
    ended: Mutex<Option<u64>>,
    /// Segments below this are gone.
    dropped: AtomicU64,
    /// The segment whose next requests hang, and how many of them.
    frozen: Mutex<Option<(u64, usize)>>,
    /// The track (`v` or `a`) whose base requests hang, while set.
    held_base: Mutex<Option<String>>,
    /// The segment whose next request stops after this many bytes of its
    /// body, and hangs.
    cut: Mutex<Option<(u64, usize)>>,
    /// Bodies are sent 1 KiB at a time, this many ms apart (0: at once).
    trickle_ms: AtomicU64,
    /// Segment bodies being sent now.
    in_flight: AtomicUsize,
    /// Each segment request's target and Range header, in order.
    ranged: Mutex<Vec<(String, Option<String>)>>,
    /// Once ended, a live lookup says the stream kept no recording.
    unrecorded: AtomicBool,
    /// The head stands still here (see `hold_head`).
    held: Mutex<Option<u64>>,
    /// Segments the head is behind the clock, for the time it was held.
    behind: AtomicU64,
    /// A held head goes on once more live lookups than this have been made.
    release_after: Mutex<Option<u64>>,
    /// What a live lookup answers instead, while set (see `lookups_answer`).
    says: Mutex<Option<String>>,
    /// Generations up to this one answer 403 (-1: none).
    forbidden: AtomicI64,
    /// The newest generation any request named (-1: none yet).
    newest: AtomicI64,
    hanging: AtomicUsize,
    seen: Mutex<Vec<String>>,
    /// The folders of the fake yt-dlps made for it (see `fake_live_ytdlp`),
    /// which learn of the end, and say how many generations they have given.
    homes: Mutex<Vec<std::path::PathBuf>>,
    /// Set when the `LiveUpstream` is dropped.
    released: AtomicBool,
}

impl LiveShared {
    /// The newest segment that exists.
    fn head(&self) -> u64 {
        match *self.ended.lock().unwrap() {
            Some(head) => head,
            None => self.growing_head(),
        }
    }

    fn growing_head(&self) -> u64 {
        let after = *self.release_after.lock().unwrap();
        if after.is_some_and(|n| self.lookups() > n) {
            *self.release_after.lock().unwrap() = None;
            self.release();
        }
        if let Some(head) = *self.held.lock().unwrap() {
            return head;
        }
        self.clock_head() - self.behind.load(Ordering::SeqCst)
    }

    /// Lets a held head go on, with the segment that came in meanwhile
    /// there at once.
    fn release(&self) {
        if let Some(head) = self.held.lock().unwrap().take() {
            self.behind
                .store(self.clock_head().saturating_sub(head + 1), Ordering::SeqCst);
        }
    }

    /// The live lookups the fake yt-dlps made for it have made.
    fn lookups(&self) -> u64 {
        self.homes
            .lock()
            .unwrap()
            .iter()
            .filter_map(|home| std::fs::read_to_string(home.join("live-lookups")).ok())
            .filter_map(|n| n.trim().parse::<u64>().ok())
            .sum()
    }

    fn clock_head(&self) -> u64 {
        self.head + self.began.elapsed().as_millis() as u64 / self.target_ms
    }
}

/// The marker a fake yt-dlp finds in its folder once the stream is over.
const LIVE_ENDED: &str = "live-ended";
/// What a fake yt-dlp keeps in its folder: the next generation to give.
const LIVE_GEN: &str = "live-gen";
/// The marker a fake yt-dlp finds once the stream is over and kept nothing.
const LIVE_UNRECORDED: &str = "live-unrecorded";
/// What a fake yt-dlp's live lookup answers instead, while it is there: an
/// `ERROR:` line is said on stderr and fails, anything else is printed.
const LIVE_SAYS: &str = "live-says";

/// What a segment of the stand-in is: its track's name and number
/// (`v-5`) repeated to 64 KiB.
pub fn live_segment(track: &str, n: u64) -> Vec<u8> {
    format!("{track}-{n}")
        .bytes()
        .cycle()
        .take(64 << 10)
        .collect()
}

impl LiveUpstream {
    /// A stream whose segments `first..=head` exist, and whose head grows by
    /// one every `target_ms` from now on.
    pub fn start(target_ms: u64, first: u64, head: u64) -> Self {
        assert!(target_ms > 0, "a segment takes no time");
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{addr}");
        let live = Arc::new(LiveShared {
            target_ms,
            first,
            head,
            began: Instant::now(),
            ended: Mutex::new(None),
            dropped: AtomicU64::new(0),
            frozen: Mutex::new(None),
            held_base: Mutex::new(None),
            cut: Mutex::new(None),
            trickle_ms: AtomicU64::new(0),
            in_flight: AtomicUsize::new(0),
            ranged: Mutex::default(),
            unrecorded: AtomicBool::new(false),
            held: Mutex::new(None),
            behind: AtomicU64::new(0),
            release_after: Mutex::new(None),
            says: Mutex::new(None),
            forbidden: AtomicI64::new(-1),
            newest: AtomicI64::new(-1),
            hanging: AtomicUsize::new(0),
            seen: Mutex::default(),
            homes: Mutex::default(),
            released: AtomicBool::new(false),
        });
        let shared = Arc::clone(&live);
        let own = base.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                if shared.released.load(Ordering::SeqCst) {
                    break;
                }
                let (shared, own) = (Arc::clone(&shared), own.clone());
                std::thread::spawn(move || live_serve(conn, &shared, &own));
            }
        });
        LiveUpstream { base, live }
    }

    /// The newest segment that exists now.
    pub fn head(&self) -> u64 {
        self.live.head()
    }

    /// The stream is over: the head stops, and a lookup made from now on
    /// says `was_live`.
    pub fn end(&self) {
        let homes = self.live.homes.lock().unwrap();
        self.live
            .ended
            .lock()
            .unwrap()
            .get_or_insert_with(|| self.live.growing_head());
        for home in homes.iter() {
            std::fs::write(home.join(LIVE_ENDED), "").unwrap();
        }
    }

    /// `end`, and YouTube kept no recording of it: a live lookup made from
    /// now on fails, saying so, as one of a stream without an archive does.
    pub fn end_unrecorded(&self) {
        self.live.unrecorded.store(true, Ordering::SeqCst);
        for home in self.live.homes.lock().unwrap().iter() {
            std::fs::write(home.join(LIVE_UNRECORDED), "").unwrap();
        }
        self.end();
    }

    /// The first request that would be answered with segment `n` (of either
    /// track) sends its head and the first `at` bytes of its body, and then
    /// nothing, as a Tor circuit that stalls part way does, until the client
    /// hangs up or the `LiveUpstream` is dropped (it counts as `hanging`).
    pub fn cut(&self, n: u64, at: usize) {
        *self.live.cut.lock().unwrap() = Some((n, at));
    }

    /// The head stands still, as it does when a streamer's connection
    /// drops for a while, until `release_head`.
    pub fn hold_head(&self) {
        let head = self.live.growing_head();
        self.live.held.lock().unwrap().get_or_insert(head);
    }

    /// The head grows again from where it stood, the next segment there at
    /// once.
    pub fn release_head(&self) {
        self.live.release();
    }

    /// `release_head` as soon as a live lookup is made: the next head asked
    /// for after it has moved on.
    pub fn release_head_at_the_next_lookup(&self) {
        *self.live.release_after.lock().unwrap() = Some(self.live.lookups());
    }

    /// Every live lookup from now on answers `text` (see `LIVE_SAYS`), or as
    /// it did again with `None`.
    pub fn lookups_answer(&self, text: Option<&str>) {
        *self.live.says.lock().unwrap() = text.map(str::to_string);
        for home in self.live.homes.lock().unwrap().iter() {
            write_says(home, text);
        }
    }

    /// Segment bodies are sent 1 KiB at a time, `ms` apart, from now on.
    pub fn trickle(&self, ms: u64) {
        self.live.trickle_ms.store(ms, Ordering::SeqCst);
    }

    /// How many segment bodies are being sent now.
    pub fn in_flight(&self) -> usize {
        self.live.in_flight.load(Ordering::SeqCst)
    }

    /// The Range headers of the requests for `target`, in order (`None` for
    /// one without).
    pub fn ranges_of(&self, target: &str) -> Vec<Option<String>> {
        self.live
            .ranged
            .lock()
            .unwrap()
            .iter()
            .filter(|(t, _)| t == target)
            .map(|(_, r)| r.clone())
            .collect()
    }

    /// Segments below `n` are gone (404), as those past the rewind window
    /// are.
    pub fn drop_before(&self, n: u64) {
        self.live.dropped.store(n, Ordering::SeqCst);
    }

    /// The first request that would be answered with segment `n` (of either
    /// track) is left hanging, as a Tor circuit that stalls leaves one,
    /// until the client hangs up or the `LiveUpstream` is dropped. The ones
    /// after it are answered.
    pub fn freeze(&self, n: u64) {
        self.freeze_times(n, 1);
    }

    /// `freeze`, for the first `k` requests for segment `n`.
    pub fn freeze_times(&self, n: u64, k: usize) {
        *self.live.frozen.lock().unwrap() = Some((n, k));
    }

    /// Requests for the base of `track` (`v` or `a`), which say where the
    /// head is, hang from now on (see `freeze`).
    pub fn hold_base(&self, track: &str) {
        *self.live.held_base.lock().unwrap() = Some(track.to_string());
    }

    /// How many requests are hanging (see `freeze`).
    pub fn hanging(&self) -> usize {
        self.live.hanging.load(Ordering::SeqCst)
    }

    /// Every URL of the newest generation that was given out (or asked
    /// for) so far answers 403, as one from a retired circuit does. The
    /// next lookup's URLs, one generation on, are good.
    pub fn forbid_until_relooked(&self) {
        let given = self
            .live
            .homes
            .lock()
            .unwrap()
            .iter()
            .filter_map(|home| std::fs::read_to_string(home.join(LIVE_GEN)).ok())
            .filter_map(|n| n.trim().parse::<i64>().ok())
            .map(|next| next - 1)
            .max()
            .unwrap_or(-1);
        let newest = self.live.newest.load(Ordering::SeqCst);
        self.live
            .forbidden
            .fetch_max(given.max(newest), Ordering::SeqCst);
    }

    /// The request targets so far, in the order they came.
    pub fn requests(&self) -> Vec<String> {
        self.live.seen.lock().unwrap().clone()
    }

    /// Makes a fake yt-dlp's folder known, so that it hears of the end.
    fn adopt(&self, home: &std::path::Path) {
        let mut homes = self.live.homes.lock().unwrap();
        homes.push(home.to_path_buf());
        if self.live.ended.lock().unwrap().is_some() {
            std::fs::write(home.join(LIVE_ENDED), "").unwrap();
        }
        if self.live.unrecorded.load(Ordering::SeqCst) {
            std::fs::write(home.join(LIVE_UNRECORDED), "").unwrap();
        }
        write_says(home, self.live.says.lock().unwrap().as_deref());
    }
}

/// Puts `text` in a fake yt-dlp's folder for its live lookups, whole or not
/// at all, or takes it away.
fn write_says(home: &std::path::Path, text: Option<&str>) {
    match text {
        Some(text) => {
            let new = home.join(format!("{LIVE_SAYS}.new"));
            std::fs::write(&new, text).unwrap();
            std::fs::rename(new, home.join(LIVE_SAYS)).unwrap();
        }
        None => {
            let _ = std::fs::remove_file(home.join(LIVE_SAYS));
        }
    }
}

impl Drop for LiveUpstream {
    fn drop(&mut self) {
        self.live.released.store(true, Ordering::SeqCst);
        // The listener is waiting for a connection: give it one, to see it.
        let _ = TcpStream::connect(addr(&self.base));
    }
}

fn live_serve(mut conn: TcpStream, live: &LiveShared, base: &str) {
    let mut r = BufReader::new(conn.try_clone().unwrap());
    let mut line = String::new();
    if r.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let mut words = line.split_whitespace();
    let head_only = words.next() == Some("HEAD");
    let target = words.next().unwrap_or("/").to_string();
    let mut range = None;
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':')
            && k.trim().eq_ignore_ascii_case("range")
        {
            range = Some(v.trim().to_string());
        }
    }
    live.seen.lock().unwrap().push(target.clone());
    let reply = |conn: &mut TcpStream, status: &str, headers: &str, body: &[u8]| {
        let _ = write!(
            conn,
            "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        if !head_only {
            let _ = conn.write_all(body);
        }
    };
    // `/live/0/v&sq=5` or `/live/0/v?...&sq=5`: the path ends at the first
    // `&` or `?`, and what follows are parameters.
    let (path, query) = match target.find(['?', '&']) {
        Some(i) => (&target[..i], &target[i + 1..]),
        None => (target.as_str(), ""),
    };
    let sq: Option<u64> = query
        .split('&')
        .find_map(|kv| kv.strip_prefix("sq=")?.parse().ok());
    let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    let (kind, generation, track) = match parts[..] {
        [kind @ ("live" | "live2"), generation, track @ ("v" | "a")] => {
            match generation.parse::<u64>() {
                Ok(generation) => (kind, generation, track),
                Err(_) => return reply(&mut conn, "404 Not Found", "", b""),
            }
        }
        _ => return reply(&mut conn, "404 Not Found", "", b""),
    };
    live.newest.fetch_max(generation as i64, Ordering::SeqCst);
    if generation as i64 <= live.forbidden.load(Ordering::SeqCst) {
        return reply(&mut conn, "403 Forbidden", "", b"denied");
    }
    let head = || format!("X-Head-Seqnum: {}\r\n", live.head());
    let Some(n) = sq else {
        if live.held_base.lock().unwrap().as_deref() == Some(track) {
            live.hanging.fetch_add(1, Ordering::SeqCst);
            hang(&mut conn, live);
            live.hanging.fetch_sub(1, Ordering::SeqCst);
            return;
        }
        return match kind {
            "live" => reply(
                &mut conn,
                "302 Found",
                &format!("Location: {base}/live2/{generation}/{track}\r\n"),
                b"",
            ),
            _ => reply(&mut conn, "200 OK", &head(), b""),
        };
    };
    let first = live.first.max(live.dropped.load(Ordering::SeqCst));
    if n < first || n > live.head() {
        return reply(&mut conn, "404 Not Found", "", b"");
    }
    live.ranged
        .lock()
        .unwrap()
        .push((target.clone(), range.clone()));
    let hold = {
        let mut frozen = live.frozen.lock().unwrap();
        match *frozen {
            Some((m, k)) if m == n => {
                *frozen = (k > 1).then_some((m, k - 1));
                true
            }
            _ => false,
        }
    };
    if hold {
        live.hanging.fetch_add(1, Ordering::SeqCst);
        hang(&mut conn, live);
        live.hanging.fetch_sub(1, Ordering::SeqCst);
        return;
    }
    let cut = {
        let mut cut = live.cut.lock().unwrap();
        match *cut {
            Some((m, at)) if m == n => {
                *cut = None;
                Some(at)
            }
            _ => None,
        }
    };
    let ctype = if track == "v" {
        "video/webm"
    } else {
        "audio/mp4"
    };
    let whole = live_segment(track, n);
    let from = range
        .as_deref()
        .and_then(|r| r.strip_prefix("bytes="))
        .and_then(|r| r.strip_suffix('-'))
        .and_then(|a| a.parse::<usize>().ok())
        .filter(|a| *a < whole.len());
    let (status, extra, body) = match from {
        Some(a) => (
            "206 Partial Content",
            format!(
                "Content-Range: bytes {a}-{}/{}\r\n",
                whole.len() - 1,
                whole.len()
            ),
            &whole[a..],
        ),
        None => ("200 OK", String::new(), &whole[..]),
    };
    let _ = write!(
        conn,
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\n{}X-Sequence-Num: {n}\r\n{extra}\
         Content-Length: {}\r\nConnection: close\r\n\r\n",
        head(),
        body.len()
    );
    if head_only {
        return;
    }
    live.in_flight.fetch_add(1, Ordering::SeqCst);
    let sent = match cut {
        Some(at) => conn
            .write_all(&body[..at.min(body.len())])
            .and_then(|()| conn.flush()),
        None => send_body(&mut conn, live, body),
    };
    live.in_flight.fetch_sub(1, Ordering::SeqCst);
    if cut.is_some() && sent.is_ok() {
        live.hanging.fetch_add(1, Ordering::SeqCst);
        hang(&mut conn, live);
        live.hanging.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Sends `body`, at once or a trickle (see `LiveUpstream::trickle`), until
/// the client hangs up or the `LiveUpstream` is dropped.
fn send_body(conn: &mut TcpStream, live: &LiveShared, body: &[u8]) -> std::io::Result<()> {
    let ms = live.trickle_ms.load(Ordering::SeqCst);
    if ms == 0 {
        return conn.write_all(body);
    }
    for piece in body.chunks(1024) {
        if live.released.load(Ordering::SeqCst) {
            return Ok(());
        }
        conn.write_all(piece)?;
        std::thread::sleep(Duration::from_millis(ms));
    }
    Ok(())
}

/// Holds `conn` open, answering nothing, until the client hangs up or the
/// `LiveUpstream` is dropped.
fn hang(conn: &mut TcpStream, live: &LiveShared) {
    conn.set_read_timeout(Some(Duration::from_millis(10)))
        .unwrap();
    let mut b = [0u8; 1];
    while !live.released.load(Ordering::SeqCst) {
        match conn.read(&mut b) {
            Ok(0) => return,
            Ok(_) => {}
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => return,
        }
    }
}

/// A yt-dlp for a `LiveUpstream`, which counts its runs (`runs`) and keeps
/// each one's config as `conf<n>`. The usual lookup (manifests skipped)
/// finds no format, as one of a running stream does. The live lookup
/// (`--live-from-start`) prints the stream with two formats, `271` (video,
/// WebM, the track `v`) and `140` (audio, the track `a`), at
/// `{base}/live/{generation}/v` and `/a`, where the generation is 0 for the first
/// lookup that gives URLs and one more for each after. Once the
/// `LiveUpstream` has ended, a lookup says `was_live`.
pub fn fake_live_ytdlp(dir: &std::path::Path, up: &LiveUpstream) -> std::path::PathBuf {
    fake_live_ytdlp_upcoming(dir, up, 0, 0)
}

/// `fake_live_ytdlp`, announced at first: its first `k` live lookups answer
/// that the stream starts at `starts` (Unix seconds), and don't count as
/// generations. The usual lookup says so too until then, and after that it
/// says the stream is running.
pub fn fake_live_ytdlp_upcoming(
    dir: &std::path::Path,
    up: &LiveUpstream,
    k: usize,
    starts: i64,
) -> std::path::PathBuf {
    up.adopt(dir);
    // Each counter is written whole or not at all: a test may be reading
    // it meanwhile.
    let script = r#"d='@DIR@'
while [ $# -gt 0 ]; do [ "$1" = --config-locations ] && conf="$2"; shift; done
bump() {
  n=$(cat "$d/$1" 2>/dev/null || echo 0)
  echo $((n + 1)) > "$d/$1.new" && mv "$d/$1.new" "$d/$1"
  echo $n
}
run=$(bump runs)
cp "$conf" "$d/conf$((run + 1))"
asked=$(cat "$d/live-lookups" 2>/dev/null || echo 0)
if ! grep -qx -- --live-from-start "$conf"; then
  if [ "$asked" -lt @UPCOMING@ ]; then
    echo 'ERROR: [youtube] x: This live event will begin in 2 hours.' >&2
  else
    echo 'ERROR: [youtube] x: This live stream recording is not available.' >&2
  fi
  exit 1
fi
bump live-lookups > /dev/null
if [ -e "$d/@SAYS@" ]; then
  if grep -q '^ERROR:' "$d/@SAYS@"; then cat "$d/@SAYS@" >&2; exit 1; fi
  cat "$d/@SAYS@"
  exit 0
fi
if [ -e "$d/@UNRECORDED@" ]; then
  echo 'ERROR: [youtube] x: This live stream recording is not available.' >&2
  exit 1
fi
if [ "$asked" -lt @UPCOMING@ ]; then
  echo '{"title":"A live cam","channel":"Cam","channel_id":"UCaaaaaaaaaaaaaaaaaaaaaa","live_status":"is_upcoming","is_upcoming":true,"release_timestamp":@STARTS@}'
  exit 0
fi
gen=$(bump @GEN@)
if [ -e "$d/@ENDED@" ]; then
  state='"live_status":"was_live"'
else
  state='"live_status":"is_live","is_live":true'
fi
# A height the default cap allows: this is what the track is called, not a size.
cat <<EOF
{"title":"A live cam","channel":"Cam","channel_id":"UCaaaaaaaaaaaaaaaaaaaaaa",$state,
 "release_timestamp":1771008448,"concurrent_view_count":567,"formats":[
 {"format_id":"140","ext":"m4a","protocol":"http_dash_segments_generator","height":null,"vcodec":"none","acodec":"mp4a.40.2","target_duration":@TARGET@,"url":"@BASE@/live/$gen/a","http_headers":{"User-Agent":"UA"}},
 {"format_id":"271","ext":"webm","protocol":"http_dash_segments_generator","height":720,"vcodec":"vp9","acodec":"none","target_duration":@TARGET@,"url":"@BASE@/live/$gen/v","http_headers":{"User-Agent":"UA"}}]}
EOF
"#
    .replace("@DIR@", &dir.display().to_string())
    .replace("@UPCOMING@", &k.to_string())
    .replace("@STARTS@", &starts.to_string())
    .replace("@GEN@", LIVE_GEN)
    .replace("@ENDED@", LIVE_ENDED)
    .replace("@UNRECORDED@", LIVE_UNRECORDED)
    .replace("@SAYS@", LIVE_SAYS)
    .replace("@TARGET@", &(up.live.target_ms as f64 / 1000.0).to_string())
    .replace("@BASE@", &up.base);
    crate::store::testing::script(&dir.join("yt-dlp"), &script).into()
}

/// A SOCKS5 proxy on a local port that passes each connection on to where
/// it asks to go (on this machine), as Tor would to an exit, and keeps the
/// identity (the user name) each came over, with where it went. Tor makes a
/// circuit for each identity; this is how a test sees which identities the
/// requests used.
pub struct Hop {
    pub port: u16,
    /// For each connection: the identity, and the port it went to.
    seen: Arc<Mutex<Vec<(String, u16)>>>,
}

impl Hop {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen: Arc<Mutex<Vec<(String, u16)>>> = Arc::default();
        let kept = Arc::clone(&seen);
        std::thread::spawn(move || {
            for mut client in listener.incoming().flatten() {
                let kept = Arc::clone(&kept);
                std::thread::spawn(move || {
                    let Some((user, to)) = socks_connect(&mut client) else {
                        return;
                    };
                    let Ok(server) = TcpStream::connect(("127.0.0.1", to)) else {
                        return;
                    };
                    kept.lock().unwrap().push((user, to));
                    let flag = Arc::new(AtomicBool::new(false));
                    let (up, down) = (server.try_clone().unwrap(), client.try_clone().unwrap());
                    let f = Arc::clone(&flag);
                    std::thread::spawn(move || pass_on(up, down, &f));
                    pass_on(client, server, &flag);
                });
            }
        });
        Hop { port, seen }
    }

    pub fn proxy(&self) -> String {
        format!("socks5h://127.0.0.1:{}", self.port)
    }

    /// The identities that connections to `port` came over, each once, in order.
    pub fn identities_to(&self, port: u16) -> Vec<String> {
        let mut found: Vec<String> = Vec::new();
        for (who, to) in self.seen.lock().unwrap().iter() {
            if *to == port && !found.contains(who) {
                found.push(who.clone());
            }
        }
        found
    }
}

/// `socks_handshake`, which also says the port the client asks to reach.
fn socks_connect(c: &mut TcpStream) -> Option<(String, u16)> {
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
    let port = u16::from_be_bytes([rest[skip], rest[skip + 1]]);
    c.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).ok()?;
    Some((String::from_utf8_lossy(&user).into_owned(), port))
}

/// What yt-dlp says when YouTube asks the address to sign in.
pub const BOT_CHECK: &str = "ERROR: [youtube] dQw4w9WgXcQ: Sign in to confirm you\u{2019}re not a bot. \
    Use --cookies-from-browser or --cookies for the authentication.";

/// A yt-dlp in `dir` that meets the bot check on its first `n` runs and
/// then runs `inner` as it was asked. Every run's identity (its proxy's
/// user name, empty without one) is kept, in order (see `identities_of`).
pub fn bot_checked_ytdlp(dir: &std::path::Path, inner: &str, n: u32) -> String {
    let script = r#"d='@DIR@'
conf=""
prev=""
for a in "$@"; do [ "$prev" = --config-locations ] && conf="$a"; prev="$a"; done
who=$(sed -n "s|.*--proxy 'socks5h://\([^:]*\):.*|\1|p" "$conf")
echo "$who" >> "$d/identities"
k=$(cat "$d/bot-runs" 2>/dev/null || echo 0)
echo $((k + 1)) > "$d/bot-runs.new" && mv "$d/bot-runs.new" "$d/bot-runs"
if [ "$k" -lt @N@ ]; then
  cat "$d/bot-check" >&2
  exit 1
fi
exec '@INNER@' "$@"
"#
    .replace("@DIR@", &dir.display().to_string())
    .replace("@N@", &n.to_string())
    .replace("@INNER@", inner);
    std::fs::write(dir.join("bot-check"), format!("{BOT_CHECK}\n")).unwrap();
    crate::store::testing::script(&dir.join("yt-dlp-bot"), &script)
}

/// The identities a `bot_checked_ytdlp` in `dir` ran on, in order.
pub fn identities_of(dir: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(dir.join("identities"))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live::{Fragments, Opening};
    use serde_json::Value;
    use std::path::Path;
    use std::process::Command;

    const WATCH: &str = "https://www.youtube.com/watch?v=dQw4w9WgXcQ";

    fn get(up: &LiveUpstream, method: &str, target: &str) -> Fetched {
        fetch_target(&up.base, method, target, &[])
    }

    fn head_seqnum(headers: &HashMap<String, String>) -> u64 {
        headers["x-head-seqnum"].parse().unwrap()
    }

    /// Runs a fake yt-dlp as `Ytdlp` does: `options` are its config file's
    /// lines. Returns whether it succeeded, what it printed and what it said.
    fn ask(bin: &Path, options: &[&str]) -> (bool, String, String) {
        let conf = bin.with_file_name("conf-asked");
        std::fs::write(&conf, options.join("\n") + "\n").unwrap();
        let out = Command::new(bin)
            .arg("--config-locations")
            .arg(&conf)
            .output()
            .unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    const PLAIN: &[&str] = &[
        "-J",
        "--no-playlist",
        "--extractor-args",
        "youtube:skip=hls,dash",
    ];
    const LIVE: &[&str] = &[
        "-J",
        "--no-playlist",
        "--live-from-start",
        "--ignore-no-formats-error",
    ];

    /// The URL of a track (`v` or `a`) in a live lookup's answer.
    fn url_of(answer: &str, format_id: &str) -> String {
        let v: Value = serde_json::from_str(answer).unwrap();
        let f = v["formats"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["format_id"] == format_id)
            .unwrap();
        f["url"].as_str().unwrap().to_string()
    }

    #[test]
    fn the_live_fake_grows_redirects_and_numbers_segments() {
        let up = LiveUpstream::start(500, 0, 10);
        // The base redirects, and the head's number is where it goes.
        let (status, headers, _) = get(&up, "HEAD", "/live/0/v");
        assert_eq!(status, 302);
        let to = headers["location"].clone();
        assert_eq!(to, format!("{}/live2/0/v", up.base));
        assert!(!headers.contains_key("x-head-seqnum"));
        let followed = to.strip_prefix(&up.base).unwrap().to_string();
        let (status, headers, _) = get(&up, "HEAD", &followed);
        assert_eq!((status, head_seqnum(&headers)), (200, 10));
        // One tick later, the head is one on.
        wait_for("the head to grow", Duration::from_secs(5), || {
            head_seqnum(&get(&up, "HEAD", &followed).1) >= 11
        });
        assert!(up.head() >= 11);
        // A segment is numbered in its body and its headers, 64 KiB of it.
        let (status, headers, body) = get(&up, "GET", "/live/0/v&sq=5");
        assert_eq!(status, 200);
        assert!(body.starts_with(b"v-5v-5v-5"), "{:?}", &body[..12]);
        assert_eq!(body.len(), 64 << 10);
        assert_eq!(headers["x-sequence-num"], "5");
        assert_eq!(headers["content-length"], (64 << 10).to_string());
        assert!(head_seqnum(&headers) >= 11);
        let (_, _, body) = get(&up, "GET", "/live/0/a&sq=3");
        assert!(body.starts_with(b"a-3a-3"));
        assert_eq!(body, live_segment("a", 3));
        // `sq` is found after a `?` as well.
        let (status, _, body) = get(&up, "GET", "/live2/0/v?mime=video&sq=6");
        assert_eq!((status, &body[..3]), (200, &b"v-6"[..]));
        // Every request is kept, as it was asked.
        let seen = up.requests();
        assert_eq!(seen[0], "/live/0/v");
        assert_eq!(seen[1], "/live2/0/v");
        assert!(seen.contains(&"/live/0/v&sq=5".to_string()));
        assert!(seen.contains(&"/live2/0/v?mime=video&sq=6".to_string()));
    }

    #[test]
    fn the_live_fake_stops_growing_when_the_stream_ends() {
        let up = LiveUpstream::start(40, 0, 3);
        wait_for("the head to grow", Duration::from_secs(5), || {
            up.head() >= 5
        });
        up.end();
        let head = up.head();
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(up.head(), head);
        let (_, headers, _) = get(&up, "HEAD", "/live2/0/v");
        assert_eq!(head_seqnum(&headers), head);
        // And what exists stays there.
        assert_eq!(get(&up, "GET", &format!("/live/0/a&sq={head}")).0, 200);
        assert_eq!(
            get(&up, "GET", &format!("/live/0/a&sq={}", head + 1)).0,
            404
        );
    }

    #[test]
    fn the_live_fake_has_no_segment_before_the_first_after_the_head_or_dropped() {
        let up = LiveUpstream::start(60_000, 2, 8);
        let status = |n: u64| get(&up, "GET", &format!("/live/0/v&sq={n}")).0;
        assert_eq!([1, 2, 8, 9].map(status), [404, 200, 200, 404]);
        up.drop_before(4);
        assert_eq!([2, 3, 4, 8].map(status), [404, 404, 200, 200]);
        // Neither a track nor a generation that is not one.
        assert_eq!(get(&up, "GET", "/live/0/x&sq=5").0, 404);
        assert_eq!(get(&up, "GET", "/elsewhere").0, 404);
    }

    /// Asks for a segment of the audio track and does not read the answer.
    fn ask_for(up: &LiveUpstream, n: u64) -> TcpStream {
        let mut c = TcpStream::connect(addr(&up.base)).unwrap();
        write!(c, "GET /live/0/a&sq={n} HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        c
    }

    fn timed_out(e: &std::io::Error) -> bool {
        matches!(
            e.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        )
    }

    #[test]
    fn a_frozen_segment_hangs_once_and_is_freed_when_the_client_leaves() {
        let up = LiveUpstream::start(60_000, 0, 8);
        up.freeze(5);
        // Another segment is as usual.
        assert_eq!(get(&up, "GET", "/live/0/v&sq=4").0, 200);
        // The first request for 5 is not answered.
        let mut first = ask_for(&up, 5);
        first
            .set_read_timeout(Some(Duration::from_millis(300)))
            .unwrap();
        let err = first.read(&mut [0u8; 1]).unwrap_err();
        assert!(timed_out(&err), "{err}");
        assert_eq!(up.hanging(), 1);
        // The second is answered, with the segment.
        let (status, _, body) = get(&up, "GET", "/live/0/v&sq=5");
        assert_eq!((status, &body[..3]), (200, &b"v-5"[..]));
        assert_eq!(up.hanging(), 1);
        // The client that gives up frees it.
        drop(first);
        wait_for(
            "the hung request to be let go",
            Duration::from_secs(5),
            || up.hanging() == 0,
        );
        // A segment ahead of the head is not hung on: it is not there.
        up.freeze(9);
        assert_eq!(get(&up, "GET", "/live/0/v&sq=9").0, 404);
        assert_eq!(up.hanging(), 0);
    }

    #[test]
    fn a_frozen_segment_is_let_go_when_the_fake_is_dropped() {
        let up = LiveUpstream::start(60_000, 0, 8);
        up.freeze(6);
        let mut waiting = ask_for(&up, 6);
        waiting
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        wait_for("it to hang", Duration::from_secs(5), || up.hanging() == 1);
        drop(up);
        let freed = waiting.read(&mut [0u8; 1]);
        assert!(
            matches!(freed, Ok(0)) || freed.as_ref().is_err_and(|e| !timed_out(e)),
            "{freed:?}"
        );
    }

    #[test]
    fn the_live_fake_forbids_a_generation_until_the_stream_is_looked_up_again() {
        let up = LiveUpstream::start(60_000, 0, 8);
        let bin = TempDir::new();
        let yt = fake_live_ytdlp(&bin.0, &up);
        let (ok, answer, _) = ask(&yt, LIVE);
        assert!(ok);
        assert_eq!(url_of(&answer, "271"), format!("{}/live/0/v", up.base));
        assert_eq!(url_of(&answer, "140"), format!("{}/live/0/a", up.base));
        let status = |generation: u64, target: &str| {
            get(&up, "GET", &format!("/live/{generation}/{target}")).0
        };
        assert_eq!(status(0, "v&sq=5"), 200);
        up.forbid_until_relooked();
        // Every request of the old generation is refused: segments, and the head's.
        assert_eq!(status(0, "v&sq=5"), 403);
        assert_eq!(status(0, "v"), 403);
        assert_eq!(get(&up, "HEAD", "/live2/0/a").0, 403);
        // The next lookup's URLs are good, and so are the following ones.
        let (_, answer, _) = ask(&yt, LIVE);
        assert_eq!(url_of(&answer, "271"), format!("{}/live/1/v", up.base));
        assert_eq!(status(1, "v&sq=5"), 200);
        assert_eq!(status(1, "a"), 302);
        // And the one after that is forbidden in turn.
        up.forbid_until_relooked();
        assert_eq!(
            [
                status(0, "v&sq=5"),
                status(1, "v&sq=5"),
                status(2, "v&sq=5")
            ],
            [403, 403, 200]
        );
        assert_eq!(runs(&bin.0), 2);
    }

    #[test]
    fn the_live_fake_ytdlp_opens_as_live_and_then_as_ended() {
        let up = LiveUpstream::start(300, 0, 10);
        let bin = TempDir::new();
        let ytdlp = fake_live_ytdlp(&bin.0, &up).display().to_string();
        let (s, _base, _d) = server_with(Config {
            ytdlp,
            ..Config::default()
        });
        let rt = s.runtime();
        let open = || rt.yt.open_until(WATCH, "b", 1080, &|| false).unwrap();
        let Opening::Live(live) = open() else {
            panic!("not live")
        };
        assert_eq!(live.video.base, format!("{}/live/0/v", up.base));
        assert_eq!(live.audio.base, format!("{}/live/0/a", up.base));
        assert_eq!((live.video.ext, live.audio.ext), ("mkv", "m4a"));
        assert_eq!(live.fragments, Fragments::Numbered);
        assert_eq!(live.target, 0.3);
        assert!(!live.ended);
        // The usual lookup found no format, and the live one answered.
        assert_eq!(runs(&bin.0), 2);
        // A later lookup is a new generation, and says it is over once it is.
        up.end();
        let Opening::Live(live) = open() else {
            panic!("not live")
        };
        assert_eq!(live.video.base, format!("{}/live/1/v", up.base));
        assert!(live.ended);
        assert_eq!(runs(&bin.0), 4);
    }

    #[test]
    fn the_upcoming_fake_ytdlp_announces_k_times_and_then_is_live() {
        let up = LiveUpstream::start(300, 0, 10);
        let bin = TempDir::new();
        let yt = fake_live_ytdlp_upcoming(&bin.0, &up, 2, 1_800_000_000);
        // While it is announced, the usual lookup says so.
        let (ok, _, said) = ask(&yt, PLAIN);
        assert!(!ok);
        assert!(said.contains("live event will begin"), "{said}");
        let ytdlp = yt.display().to_string();
        let (s, _base, _d) = server_with(Config {
            ytdlp,
            ..Config::default()
        });
        let rt = s.runtime();
        let open = || rt.yt.open_until(WATCH, "b", 1080, &|| false).unwrap();
        for _ in 0..2 {
            let Opening::Upcoming { starts, .. } = open() else {
                panic!("not upcoming")
            };
            assert_eq!(starts, Some(1_800_000_000));
        }
        // The third time it has started, and its numbers begin at 0.
        let Opening::Live(live) = open() else {
            panic!("not live")
        };
        assert_eq!(live.video.base, format!("{}/live/0/v", up.base));
        // 1 above, then three lookups of two calls each.
        assert_eq!(runs(&bin.0), 7);
        let (_, _, said) = ask(&yt, PLAIN);
        assert!(
            said.contains("live stream recording is not available"),
            "{said}"
        );
    }
}
