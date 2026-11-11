//! A stand-in for googlevideo that speaks HTTP/2, for the tests. It speaks it
//! in clear text to clients that know it will (`Net::speaking_h2c`), which is
//! what the real one does over TLS once ALPN has picked `h2`: the streams,
//! their cancelling and the flow control are the same either way.
//!
//! It answers every path like `/big` of the HTTP/1.1 stand-in (the bytes
//! `i % 251`, Range requests answered as ranges), with `size=` and `pace=`
//! (ms between 16 KiB frames) in the query, and `ttfb=` (ms before the
//! answer). `accept_delay` stands in for what a new connection costs over Tor.

use bytes::Bytes;
use std::future::poll_fn;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::Duration;

/// The size of a stream unless `size=` says otherwise: as `/big` of the
/// HTTP/1.1 stand-in, two 10 MiB chunks and a bit.
pub const BIG: u64 = 2 * (10 << 20) + 12_345;

/// What the stand-in has seen.
#[derive(Default)]
pub struct Stats {
    /// TCP connections accepted.
    pub connections: AtomicUsize,
    /// The Range header of every request, in order.
    pub ranges: Mutex<Vec<Option<String>>>,
    /// Streams the client cancelled before their end.
    pub resets: AtomicUsize,
    /// Body bytes sent, over all streams.
    pub sent: AtomicUsize,
}

pub struct H2Upstream {
    pub addr: SocketAddr,
    pub stats: Arc<Stats>,
}

impl H2Upstream {
    pub fn start() -> Self {
        Self::start_with(Duration::ZERO)
    }

    /// Each new connection is accepted only after `accept_delay`.
    pub fn start_with(accept_delay: Duration) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let stats = Arc::new(Stats::default());
        let seen = Arc::clone(&stats);
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                while let Ok((sock, _)) = listener.accept().await {
                    seen.connections.fetch_add(1, Ordering::SeqCst);
                    let stats = Arc::clone(&seen);
                    tokio::spawn(async move {
                        tokio::time::sleep(accept_delay).await;
                        serve(sock, stats).await;
                    });
                }
            });
        });
        H2Upstream { addr, stats }
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    pub fn connections(&self) -> usize {
        self.stats.connections.load(Ordering::SeqCst)
    }

    pub fn resets(&self) -> usize {
        self.stats.resets.load(Ordering::SeqCst)
    }

    pub fn sent(&self) -> usize {
        self.stats.sent.load(Ordering::SeqCst)
    }

    pub fn ranges(&self) -> Vec<Option<String>> {
        self.stats.ranges.lock().unwrap().clone()
    }
}

/// The bytes `from..to` of a stream.
pub fn big_bytes_h2(from: u64, to: u64) -> Vec<u8> {
    (from..to).map(|i| (i % 251) as u8).collect()
}

async fn serve(sock: tokio::net::TcpStream, stats: Arc<Stats>) {
    let Ok(mut conn) = h2::server::handshake(sock).await else {
        return;
    };
    while let Some(Ok((req, respond))) = conn.accept().await {
        tokio::spawn(answer(req, respond, Arc::clone(&stats)));
    }
}

fn param<'a>(query: &'a str, k: &str) -> Option<&'a str> {
    query
        .split('&')
        .find_map(|kv| kv.strip_prefix(k)?.strip_prefix('='))
}

async fn answer(
    req: http::Request<h2::RecvStream>,
    mut respond: h2::server::SendResponse<Bytes>,
    stats: Arc<Stats>,
) {
    let query = req.uri().query().unwrap_or("").to_string();
    let range = req
        .headers()
        .get("range")
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    stats.ranges.lock().unwrap().push(range.clone());
    let num = |k: &str, or: u64| param(&query, k).and_then(|s| s.parse().ok()).unwrap_or(or);
    let size = num("size", BIG);
    let pace = num("pace", 0);
    tokio::time::sleep(Duration::from_millis(num("ttfb", 0))).await;
    let (a, b) = match range.as_deref().and_then(|r| r.strip_prefix("bytes=")) {
        Some(r) => {
            let (a, b) = r.split_once('-').unwrap();
            let a: u64 = a.parse().unwrap();
            (a, b.parse().map_or(size - 1, |b: u64| b.min(size - 1)))
        }
        None => (0, size - 1),
    };
    let mut head = http::Response::builder()
        .header("content-type", "video/mp4")
        .header("content-length", b - a + 1);
    head = match range {
        Some(_) => head
            .status(206)
            .header("content-range", format!("bytes {a}-{b}/{size}")),
        None => head.status(200),
    };
    let Ok(mut body) = respond.send_response(head.body(()).unwrap(), false) else {
        return;
    };
    let mut at = a;
    while at <= b {
        let want = (16u64 << 10).min(b + 1 - at) as usize;
        body.reserve_capacity(want);
        // Waits for the client to have room, unless it cancels the stream.
        let room = poll_fn(|cx| {
            if body.poll_reset(cx).is_ready() {
                return Poll::Ready(None);
            }
            loop {
                match body.poll_capacity(cx) {
                    Poll::Ready(Some(Ok(0))) => continue,
                    Poll::Ready(Some(Ok(n))) => return Poll::Ready(Some(n)),
                    Poll::Ready(_) => return Poll::Ready(None),
                    Poll::Pending => return Poll::Pending,
                }
            }
        })
        .await;
        let Some(n) = room else {
            stats.resets.fetch_add(1, Ordering::SeqCst);
            return;
        };
        let n = n.min(want) as u64;
        let block = big_bytes_h2(at, at + n);
        let end = at + n > b;
        if body.send_data(Bytes::from(block), end).is_err() {
            stats.resets.fetch_add(1, Ordering::SeqCst);
            return;
        }
        stats.sent.fetch_add(n as usize, Ordering::SeqCst);
        at += n;
        if pace > 0 {
            tokio::time::sleep(Duration::from_millis(pace)).await;
        }
    }
}
