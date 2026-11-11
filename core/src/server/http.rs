//! Reading requests and answering them, within limits.
//!
//! Everything here is about the connection, not about what it asks for: how
//! much a client may send before it has shown the token, how long it has to
//! send it, how many may be served at once, and how replies are written.

use crate::util::lock;
use anyhow::{Context, Result};
use std::cell::Cell;
use std::collections::{HashMap, VecDeque};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, SocketAddrV4, TcpStream};
use std::os::fd::{AsRawFd, RawFd};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub(super) const MAX_BODY: usize = 8 << 20;

// Limits that apply before a client has proven it holds the token, so that a
// local program (or a web page that guessed the port) can't tie up threads or
// memory just by connecting.
pub(super) const MAX_REQUEST_LINE: u64 = 8 << 10;
pub(super) const MAX_HEADERS: u64 = 32 << 10;
pub(super) const MAX_HEADER_COUNT: usize = 100;
/// How many connections that have presented the token are served at once.
pub(super) const MAX_CONNECTIONS: usize = 64;
/// How many connections that haven't yet are kept open (see `Unverified`).
pub(super) const MAX_UNVERIFIED: usize = 64;
/// A client has this long to send a request line, and so to show it has the
/// token. The real client sends it the moment it connects.
pub(super) const REQUEST_LINE_TIMEOUT: Duration = Duration::from_secs(2);
/// Once the token is shown, the client has this long to send the rest of its request.
pub(super) const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// Ordinary responses are small, so a client that doesn't read them is stuck.
pub(super) const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long, and how much, to read from a refused client before closing (see `refuse`).
pub(super) const LINGER: Duration = Duration::from_secs(1);
pub(super) const LINGER_BYTES: u64 = 64 << 10;
/// Pause after a failed `accept`, so running out of file descriptors doesn't spin a core.
pub(super) const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

pub(super) struct Request {
    pub(super) method: String,
    pub(super) path: String,
    pub(super) query: HashMap<String, String>,
    pub(super) headers: Vec<(String, String)>,
    pub(super) body: Vec<u8>,
}

impl Request {
    pub(super) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
    pub(super) fn param(&self, name: &str) -> Result<&str> {
        self.query
            .get(name)
            .map(String::as_str)
            .with_context(|| format!("missing {name}"))
    }
    pub(super) fn num(&self, name: &str, default: usize) -> usize {
        self.query
            .get(name)
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    }
    pub(super) fn flag(&self, name: &str) -> bool {
        self.query
            .get(name)
            .is_some_and(|v| v == "1" || v == "true")
    }
}

/// The path after `/<token>/`, if the token is right. Compared without
/// stopping at the first wrong byte, so response timing says nothing about it.
pub(super) fn token_rest<'a>(token: &str, path: &'a str) -> Option<&'a str> {
    let p = path.strip_prefix('/')?;
    let (given, rest) = p.split_at_checked(token.len())?;
    let rest = rest.strip_prefix('/')?;
    constant_time_eq(given.as_bytes(), token.as_bytes()).then_some(rest)
}

/// A request the server refuses to read on, with the status to say so.
pub(super) struct Reject {
    status: u16,
    msg: &'static str,
}

impl Reject {
    pub(super) const fn new(status: u16, msg: &'static str) -> Self {
        Self { status, msg }
    }
}

/// Answers a request we stopped reading, then closes politely. Closing a
/// socket that still holds unread bytes resets the connection, and the client
/// may then lose the answer we just wrote, so first read (and drop) a bounded
/// amount of what it is still sending.
pub(super) fn refuse(
    stream: &mut TcpStream,
    r: &mut BufReader<Deadline>,
    reject: Reject,
) -> Result<()> {
    respond(stream, reject.status, "text/plain", reject.msg.as_bytes())?;
    stream.shutdown(Shutdown::Write)?;
    r.get_mut().until = Instant::now() + LINGER;
    let _ = io::copy(&mut r.by_ref().take(LINGER_BYTES), &mut io::sink());
    Ok(())
}

impl From<io::Error> for Reject {
    fn from(e: io::Error) -> Self {
        match e.kind() {
            // What a socket read timeout looks like, depending on the platform.
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => {
                Reject::new(408, "request timed out")
            }
            _ => Reject::new(400, "bad request"),
        }
    }
}

/// Reads from a socket, but gives up at a fixed deadline for the whole
/// request, so a client can't hold a thread by sending one byte at a time.
pub(super) struct Deadline {
    stream: TcpStream,
    pub(super) until: Instant,
}

impl Deadline {
    pub(super) fn new(stream: TcpStream, within: Duration) -> Self {
        Self {
            stream,
            until: Instant::now() + within,
        }
    }
}

impl Read for Deadline {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let left = self
            .until
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or(io::ErrorKind::TimedOut)?;
        self.stream.set_read_timeout(Some(left))?;
        // A socket read timeout surfaces as WouldBlock on Unix; report it as
        // the deadline it is, so callers see one kind for "too slow".
        self.stream.read(buf).map_err(|e| match e.kind() {
            io::ErrorKind::WouldBlock => io::Error::from(io::ErrorKind::TimedOut),
            _ => e,
        })
    }
}

pub(super) struct RequestLine {
    method: String,
    pub(super) path: String,
    query: HashMap<String, String>,
}

pub(super) fn read_request_line(r: &mut impl BufRead) -> Result<RequestLine, Reject> {
    let mut line = String::new();
    // `take` is what bounds this: read_line alone would buffer an endless line.
    r.by_ref().take(MAX_REQUEST_LINE).read_line(&mut line)?;
    if !line.ends_with('\n') {
        return Err(if line.len() as u64 >= MAX_REQUEST_LINE {
            Reject::new(414, "request line too long")
        } else {
            Reject::new(400, "malformed request line")
        });
    }
    let mut parts = line.split_whitespace();
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        return Err(Reject::new(400, "malformed request line"));
    };
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let query = url::form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .collect();
    Ok(RequestLine {
        method: method.into(),
        path: path.into(),
        query,
    })
}

pub(super) fn read_request_rest(
    r: &mut impl BufRead,
    line: RequestLine,
) -> Result<Request, Reject> {
    let mut headers = Vec::new();
    let mut budget = r.by_ref().take(MAX_HEADERS);
    loop {
        let mut h = String::new();
        budget.read_line(&mut h)?;
        if !h.ends_with('\n') {
            return Err(if budget.limit() == 0 {
                Reject::new(431, "headers too large")
            } else {
                Reject::new(400, "incomplete headers")
            });
        }
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if headers.len() >= MAX_HEADER_COUNT {
            return Err(Reject::new(431, "too many headers"));
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    let len = match headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
    {
        None => 0,
        Some((_, v)) => v
            .parse::<usize>()
            .map_err(|_| Reject::new(400, "bad Content-Length"))?,
    };
    if len > MAX_BODY {
        return Err(Reject::new(413, "body too large"));
    }
    // Grown as the bytes arrive rather than allocated up front, so a
    // Content-Length that is never fulfilled costs nothing.
    let mut body = Vec::new();
    r.by_ref().take(len as u64).read_to_end(&mut body)?;
    if body.len() != len {
        return Err(Reject::new(400, "body shorter than Content-Length"));
    }
    Ok(Request {
        method: line.method,
        path: line.path,
        query: line.query,
        headers,
        body,
    })
}

/// The connections that haven't shown the token, kept apart from the main
/// pool so that a program that merely connects can't use up the places the
/// app needs. Only connections of our own user get here (`from_our_user`). A full pool doesn't turn the newcomer away, because the app's own
/// connection is a newcomer too: the oldest stranger is closed instead. A
/// connection that has been served ends up out of here within microseconds,
/// so the one that is closed is nearly always one that is just sitting there.
#[derive(Default)]
pub(super) struct Unverified(Mutex<VecDeque<(u64, TcpStream)>>);

/// A place in `Unverified`, given up when dropped (or when it is closed to make room).
pub(super) struct Pending {
    pool: Arc<Unverified>,
    id: u64,
}

impl Pending {
    pub(super) fn admit(pool: &Arc<Unverified>, conn: &TcpStream) -> io::Result<Pending> {
        let id = rand::random();
        let mine = conn.try_clone()?;
        let mut waiting = lock(&pool.0);
        if waiting.len() >= MAX_UNVERIFIED
            && let Some((_, oldest)) = waiting.pop_front()
        {
            // Wakes the thread serving it, which then ends.
            let _ = oldest.shutdown(Shutdown::Both);
        }
        waiting.push_back((id, mine));
        Ok(Pending {
            pool: Arc::clone(pool),
            id,
        })
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        lock(&self.pool.0).retain(|(id, _)| *id != self.id);
    }
}

/// Whether the other end of `conn`, a connection to the loopback listener,
/// belongs to the user qveil runs as. Only those are let into `Unverified`:
/// a process of another user on this machine could otherwise keep the pool
/// full of connections and close the window's own before it shows the
/// token. A process of the same user can compete for those places, but it
/// could just as well kill qveil, so shutting it out would protect nothing.
///
/// Read from /proc/net/tcp, where the client's end of the connection is the
/// row whose own address is our peer's. Should that table be unreadable,
/// the token alone guards the server, as it does elsewhere.
pub(super) fn from_our_user(conn: &TcpStream) -> bool {
    let (Ok(peer), Ok(local)) = (conn.peer_addr(), conn.local_addr()) else {
        // Already gone.
        return false;
    };
    let (SocketAddr::V4(peer), SocketAddr::V4(local)) = (peer, local) else {
        // The listener is IPv4; anything else can't be looked up here.
        return true;
    };
    let table = std::fs::read_to_string("/proc/net/tcp").ok();
    // SAFETY: geteuid has no preconditions and can't fail.
    let me = unsafe { libc::geteuid() };
    owned_by(table.as_deref(), peer, local, me)
}

/// The decision of `from_our_user`, given the table (if it could be read).
fn owned_by(table: Option<&str>, peer: SocketAddrV4, local: SocketAddrV4, me: u32) -> bool {
    match table {
        None => true,
        Some(table) => uid_in(table, peer, local) == Some(me),
    }
}

/// The owner of the socket whose own end is `at`, connected to `to`, in a
/// table laid out as /proc/net/tcp.
fn uid_in(table: &str, at: SocketAddrV4, to: SocketAddrV4) -> Option<u32> {
    table.lines().find_map(|row| {
        // `sl`, local, remote, state, queues, timer, retransmits, uid, ...
        let mut cols = row.split_whitespace();
        let (local, remote) = (cols.nth(1)?, cols.next()?);
        if addr_hex(local)? != at || addr_hex(remote)? != to {
            return None;
        }
        cols.nth(4)?.parse().ok()
    })
}

/// An address as /proc/net/tcp writes it: `0100007F:1F90` is the address's
/// four bytes as one number in the machine's byte order, then the port.
fn addr_hex(s: &str) -> Option<SocketAddrV4> {
    let (ip, port) = s.split_once(':')?;
    let ip = u32::from_str_radix(ip, 16).ok()?;
    let port = u16::from_str_radix(port, 16).ok()?;
    Some(SocketAddrV4::new(Ipv4Addr::from(ip.to_ne_bytes()), port))
}

/// One of the `MAX_CONNECTIONS` places; given back when dropped.
pub(super) struct Slot(Arc<AtomicUsize>);

impl Slot {
    pub(super) fn take(counter: &Arc<AtomicUsize>) -> Option<Slot> {
        if counter.fetch_add(1, Ordering::AcqRel) >= MAX_CONNECTIONS {
            counter.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        Some(Slot(Arc::clone(counter)))
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
}

pub(super) fn respond(out: &mut TcpStream, status: u16, ctype: &str, body: &[u8]) -> Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        408 => "Request Timeout",
        413 => "Content Too Large",
        414 => "URI Too Long",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Bad Gateway",
    };
    write!(
        out,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    out.write_all(body)?;
    Ok(())
}

pub(super) fn respond_cached(out: &mut TcpStream, jpeg: &[u8]) -> Result<()> {
    write!(
        out,
        "HTTP/1.1 200 OK\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\nCache-Control: private, max-age=86400\r\nConnection: close\r\n\r\n",
        jpeg.len()
    )?;
    out.write_all(jpeg)?;
    Ok(())
}

/// The head of a reply that is a stream of JSON objects, one per line, which
/// the window reads as they arrive: the response has no length and ends when
/// the connection closes.
pub(super) const NDJSON_HEAD: &str = "HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n";

/// How often `Watch` looks at the socket. Often enough that a yt-dlp whose
/// caller has gone is stopped within a fraction of a second, rarely enough
/// that looking costs nothing.
const WATCH_EVERY: Duration = Duration::from_millis(100);

/// Notices that the client of a request has hung up while its answer is still
/// being worked on, so that the work can stop instead of running for a reply
/// that has nowhere to go.
///
/// It peeks at the socket without waiting and without taking anything off it:
/// a client that has closed its end reads as end of file (or as an error). The
/// request has been read completely by then, and nothing more is expected
/// from the client, so there is nothing to mistake it for.
///
/// It holds the socket's number, not a borrow of the socket, so the answer
/// can still be written meanwhile. It must not outlive the stream it was made
/// for: the handler's own function is where it lives.
pub(super) struct Watch {
    fd: RawFd,
    checked: Cell<Instant>,
    gone: Cell<bool>,
}

impl Watch {
    pub(super) fn new(stream: &TcpStream) -> Self {
        Watch {
            fd: stream.as_raw_fd(),
            checked: Cell::new(Instant::now()),
            gone: Cell::new(false),
        }
    }

    /// Whether the client has hung up. Looks at the socket at most every
    /// `WATCH_EVERY`, so it can be asked as often as is convenient.
    pub(super) fn gone(&self) -> bool {
        if !self.gone.get() && self.checked.get().elapsed() >= WATCH_EVERY {
            self.checked.set(Instant::now());
            self.gone.set(peer_gone(self.fd));
        }
        self.gone.get()
    }
}

fn peer_gone(fd: RawFd) -> bool {
    let mut byte = 0u8;
    // SAFETY: the buffer is one byte, as long as the length says, and the
    // socket is open for as long as the `Watch` that asks (see its docs).
    let n = unsafe {
        libc::recv(
            fd,
            (&raw mut byte).cast(),
            1,
            libc::MSG_PEEK | libc::MSG_DONTWAIT,
        )
    };
    match n {
        // End of file: the client closed its end.
        0 => true,
        // It sent more: it is there.
        1.. => false,
        _ => !matches!(
            io::Error::last_os_error().kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::testkit::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::{Duration, Instant};

    /// Rows of /proc/net/tcp: a listener, the server's end of a
    /// connection to it (ours), and the client's end (another user's).
    const TABLE: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 593353 1 0 100 0 0 10 0
   1: 0100007F:1F90 0100007F:D431 01 00000000:00000000 00:00000000 00000000  1000        0 593354 1 0 20 4 30 10 -1
   2: 0100007F:D431 0100007F:1F90 01 00000000:00000000 00:00000000 00000000  1001        0 593355 1 0 20 4 30 10 -1
";

    #[test]
    fn the_owner_of_the_other_end_is_read_from_the_table() {
        let server: std::net::SocketAddrV4 = "127.0.0.1:8080".parse().unwrap();
        let client: std::net::SocketAddrV4 = "127.0.0.1:54321".parse().unwrap();
        // The row whose own end is the client's, talking to the server.
        assert_eq!(uid_in(TABLE, client, server), Some(1001));
        assert_eq!(uid_in(TABLE, server, client), Some(1000));
        // A port nobody has, or the header line and junk, say nothing.
        let other: std::net::SocketAddrV4 = "127.0.0.1:54322".parse().unwrap();
        assert_eq!(uid_in(TABLE, other, server), None);
        assert_eq!(uid_in("garbage\n   0: x y z\n", client, server), None);
        assert_eq!(addr_hex("0100007F:1F90"), Some(server));
        assert_eq!(addr_hex("0100007F"), None);
    }

    #[test]
    fn a_connection_of_our_own_user_is_ours_and_one_of_another_is_not() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let _client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (conn, _) = listener.accept().unwrap();
        assert!(from_our_user(&conn));
        // The same decision, with the client's end owned by another user.
        let me = unsafe { libc::geteuid() };
        let server: std::net::SocketAddrV4 = "127.0.0.1:8080".parse().unwrap();
        let client: std::net::SocketAddrV4 = "127.0.0.1:54321".parse().unwrap();
        assert!(!owned_by(Some(TABLE), client, server, 1000));
        assert!(owned_by(Some(TABLE), client, server, 1001));
        // Not found: it is gone, or was never ours.
        assert!(!owned_by(Some(""), client, server, me));
        // A table that can't be read leaves it to the token.
        assert!(owned_by(None, client, server, me));
    }

    #[test]
    fn a_client_without_the_token_gets_nothing_more_than_one_line() {
        let (_s, base, _d) = server();
        // The request line alone is enough to be refused: the server doesn't
        // wait for headers that, here, never come.
        assert_eq!(raw(&base, b"GET /nope/api/config HTTP/1.1\r\n"), 404);
    }

    #[test]
    fn request_line_and_header_sizes_are_capped() {
        let (_s, base, _d) = server();
        let long = format!("GET /{} HTTP/1.1\r\n\r\n", "a".repeat(9000));
        assert_eq!(raw(&base, long.as_bytes()), 414);

        let token = base.rsplit('/').next().unwrap();
        let head = format!("GET /{token}/api/config HTTP/1.1\r\n");
        // 40 headers of 1 KiB: few enough to count, too many bytes to read.
        let big = format!(
            "{head}{}\r\n",
            format!("X-Pad: {}\r\n", "a".repeat(1000)).repeat(40)
        );
        assert_eq!(raw(&base, big.as_bytes()), 431);
        // Many tiny headers hit the count limit instead.
        let many = format!("{head}{}\r\n", "X: y\r\n".repeat(150));
        assert_eq!(raw(&base, many.as_bytes()), 431);
        // A body bigger than any request the app sends is refused unread.
        let body = format!("POST /{token}/api/config HTTP/1.1\r\nContent-Length: 99999999\r\n\r\n");
        assert_eq!(raw(&base, body.as_bytes()), 413);
        // The caps leave ordinary requests alone.
        let fine = format!("{head}Host: x\r\n\r\n");
        assert_eq!(raw(&base, fine.as_bytes()), 200);
    }

    #[test]
    fn clients_without_the_token_cannot_lock_the_app_out() {
        let (_s, base, _d) = server();
        // More silent connections than the server has places for, and when
        // each was made.
        let idle: Vec<(TcpStream, Instant)> = (0..MAX_UNVERIFIED + MAX_CONNECTIONS + 16)
            .map(|_| (TcpStream::connect(addr(&base)).unwrap(), Instant::now()))
            .collect();
        // The app, which has the token, is served all the same, and at once:
        // before a single stranger has been told it was too slow, so it
        // didn't have to wait for their places to run out.
        let asked = Instant::now();
        let (status, body) = http(&base, "GET", &path(&base, "api/config"), "");
        let served = Instant::now();
        assert_eq!(status, 200, "{body}");
        assert!(
            served - asked < REQUEST_LINE_TIMEOUT,
            "the app waited {:?} to be served",
            served - asked
        );
        // A stranger may have been told it was too slow only if it had been
        // waiting that long: on a busy machine, making the connections alone
        // can take longer than that.
        for (c, made) in &idle {
            if served - *made >= REQUEST_LINE_TIMEOUT {
                continue;
            }
            c.set_nonblocking(true).unwrap();
            let mut head = [0u8; 12];
            let n = c.peek(&mut head).unwrap_or(0);
            assert!(
                !head[..n].starts_with(b"HTTP/1.1 408"),
                "the app was served only after the strangers timed out"
            );
            c.set_nonblocking(false).unwrap();
        }
        let idle: Vec<TcpStream> = idle.into_iter().map(|(c, _)| c).collect();

        // And the silent ones don't linger: each is closed, within the short
        // deadline for a request line, not left for the full 15 s.
        let started = Instant::now();
        for mut c in idle {
            c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            let mut said = Vec::new();
            let _ = c.read_to_end(&mut said);
            let said = String::from_utf8_lossy(&said);
            // Closed to make room, or told it was too slow.
            assert!(
                said.is_empty() || said.starts_with("HTTP/1.1 408"),
                "{said}"
            );
        }
        assert!(
            started.elapsed() < REQUEST_LINE_TIMEOUT + Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn the_connections_with_the_token_are_capped_and_released() {
        let (s, base, _d) = server();
        let token = base.rsplit('/').next().unwrap();
        // Connections that show the token but never finish their request,
        // each given time to be moved to the main pool.
        let held: Vec<TcpStream> = (1..=MAX_CONNECTIONS)
            .map(|n| {
                let mut c = TcpStream::connect(addr(&base)).unwrap();
                write!(c, "GET /{token}/api/config HTTP/1.1\r\n").unwrap();
                wait_for(
                    "the connection to be served",
                    Duration::from_secs(5),
                    || s.connections.load(Ordering::Acquire) == n,
                );
                c
            })
            .collect();
        let probe = || {
            raw(
                &base,
                format!("GET /{token}/api/config HTTP/1.1\r\n\r\n").as_bytes(),
            )
        };
        // The main pool is full, so the next one is turned away.
        assert_eq!(probe(), 503);
        drop(held);
        // Hanging up frees the places again.
        let started = Instant::now();
        let status = loop {
            let status = probe();
            if status != 503 || started.elapsed() > Duration::from_secs(5) {
                break status;
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        assert_eq!(status, 200, "places were not released");
    }

    #[test]
    fn the_rest_of_a_request_has_longer_than_the_request_line() {
        let (_s, base, _d) = server();
        let token = base.rsplit('/').next().unwrap();
        let mut c = TcpStream::connect(addr(&base)).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        write!(c, "GET /{token}/api/config HTTP/1.1\r\n").unwrap();
        // Slower than a stranger is allowed to be, but it has shown the token.
        std::thread::sleep(REQUEST_LINE_TIMEOUT + Duration::from_millis(500));
        write!(c, "Host: x\r\n\r\n").unwrap();
        let mut resp = String::new();
        c.read_to_string(&mut resp).unwrap();
        assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
    }

    #[test]
    fn a_trickling_client_cannot_outlast_the_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let trickler = std::thread::spawn(move || {
            let mut c = TcpStream::connect(addr).unwrap();
            // One byte every 50 ms never trips a per-read timeout of 300 ms,
            // and the trickle lasts 10 s, far past the deadline.
            for _ in 0..200 {
                if c.write_all(b"a").is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
        let (peer, _) = listener.accept().unwrap();
        let started = Instant::now();
        let mut reader = Deadline::new(peer, Duration::from_millis(300));
        let mut buf = [0u8; 1];
        let err = loop {
            if let Err(e) = reader.read(&mut buf) {
                break e;
            }
        };
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        // Generous for loaded CI machines, yet well short of the 10 s trickle.
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        drop(reader);
        trickler.join().unwrap();
    }

    #[test]
    fn a_hung_up_client_is_noticed_and_one_that_is_still_there_is_left_alone() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (mut mine, _) = listener.accept().unwrap();
        let watch = Watch::new(&mine);
        std::thread::sleep(WATCH_EVERY + Duration::from_millis(30));
        assert!(!watch.gone(), "it is still there");
        // Looking doesn't take anything off the socket.
        client.write_all(b"x").unwrap();
        std::thread::sleep(WATCH_EVERY + Duration::from_millis(30));
        assert!(!watch.gone());
        let mut byte = [0u8; 1];
        mine.read_exact(&mut byte).unwrap();
        assert_eq!(&byte, b"x");

        drop(client);
        let started = Instant::now();
        while !watch.gone() {
            assert!(started.elapsed() < Duration::from_secs(2), "never noticed");
            std::thread::sleep(Duration::from_millis(10));
        }
        // And it stays noticed.
        assert!(watch.gone());
    }
}
