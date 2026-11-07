//! Network identity: proxy routing and the HTTP client used for feeds.

use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;
use std::time::Duration;

/// A common desktop browser UA, so requests don't announce "veil" or "reqwest".
pub const USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:140.0) Gecko/20100101 Firefox/140.0";

pub const TOR_PROXY: &str = "socks5h://127.0.0.1:9050";

/// How long a relayed stream may go without receiving a single byte.
pub const RELAY_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, Default)]
pub struct Net {
    proxy: Option<String>,
    isolate: bool,
    /// Whether plain `http://` URLs may be fetched. Always false in
    /// production, so nothing leaves the machine unencrypted through the exit
    /// node or proxy; only tests, which talk to a local server, turn it on.
    allow_http: bool,
}

impl Net {
    pub fn new(proxy: Option<String>, isolate: bool) -> Result<Self> {
        if let Some(p) = &proxy {
            let scheme = p.split_once("://").map(|(s, _)| s).unwrap_or("");
            match scheme {
                "http" | "https" | "socks5h" => {}
                "socks5" => bail!(
                    "proxy {p:?} would resolve DNS locally and leak lookups; use socks5h:// instead"
                ),
                _ => bail!("unsupported proxy {p:?}; use http://, https:// or socks5h://"),
            }
        }
        Ok(Self {
            proxy,
            isolate,
            allow_http: false,
        })
    }

    /// The same route, but plain http is allowed (for tests with a local upstream).
    #[cfg(test)]
    pub fn allowing_http(mut self) -> Self {
        self.allow_http = true;
        self
    }

    /// Whether both go through the same proxy with the same isolation, which
    /// is what decides if a stream resolved on one can be played on the other.
    pub fn same_route(&self, other: &Net) -> bool {
        self.proxy == other.proxy && self.isolates() == other.isolates()
    }

    pub fn is_socks(&self) -> bool {
        self.proxy
            .as_deref()
            .is_some_and(|p| p.starts_with("socks"))
    }

    pub fn describe(&self) -> String {
        match &self.proxy {
            None => "none: YouTube sees your real IP".into(),
            Some(p) => {
                let p = redact(p);
                if self.is_socks() && self.isolate {
                    format!("{p} (fresh circuit per request)")
                } else {
                    p
                }
            }
        }
    }

    /// Proxy URL for one logical request. With isolation on, each call gets
    /// new random SOCKS credentials, so Tor (with IsolateSOCKSAuth, its
    /// default) builds a separate circuit and requests can't be linked by
    /// exit IP.
    pub fn proxy_url(&self) -> Option<String> {
        let p = self.proxy.as_ref()?;
        if !(self.isolate && self.is_socks()) {
            return Some(p.clone());
        }
        let (scheme, rest) = p.split_once("://")?;
        if rest.contains('@') {
            return Some(p.clone()); // the user set their own credentials
        }
        let user: u64 = rand::random();
        Some(format!("{scheme}://veil{user:016x}:x@{rest}"))
    }

    pub fn proxy(&self) -> Option<&str> {
        self.proxy.as_deref()
    }

    pub fn isolates(&self) -> bool {
        self.isolate && self.is_socks()
    }

    /// The same route with one fixed identity, for work that must keep a
    /// single exit IP: stream URLs only work from the IP that resolved them,
    /// so yt-dlp and the relay have to share a circuit.
    pub fn pinned(&self) -> Net {
        Net {
            proxy: self.proxy_url(),
            isolate: false,
            ..self.clone()
        }
    }

    /// A fresh client: no cookie store, no Referer, browser UA, proxy applied.
    pub fn client(&self) -> Result<Client> {
        self.build(Some(Duration::from_secs(20)))
    }

    /// Like `client`, but without a total timeout, because a relayed video
    /// body can stream for an hour. What it has instead is an idle timeout:
    /// reqwest's blocking client applies its timeout to waiting for the
    /// response head and then to each separate read of the body, so a stalled
    /// upstream (a dead Tor circuit, say) ends the stream with an error
    /// instead of holding a thread and a connection forever.
    pub fn relay_client(&self) -> Result<Client> {
        self.relay_client_idle(RELAY_IDLE_TIMEOUT)
    }

    fn relay_client_idle(&self, idle: Duration) -> Result<Client> {
        self.build(Some(idle))
    }

    fn build(&self, timeout: Option<Duration>) -> Result<Client> {
        let mut b = Client::builder()
            .user_agent(USER_AGENT)
            .referer(false)
            .https_only(!self.allow_http)
            .connect_timeout(Duration::from_secs(20))
            .timeout(timeout);
        if let Some(p) = self.proxy_url() {
            b = b.proxy(reqwest::Proxy::all(&p).context("invalid proxy URL")?);
        }
        Ok(b.build()?)
    }
}

/// Whether something is listening on Tor's default SOCKS port.
pub fn tor_running() -> bool {
    std::net::TcpStream::connect_timeout(&([127, 0, 0, 1], 9050).into(), Duration::from_millis(300))
        .is_ok()
}

/// The proxy URL without its credentials: `scheme://***@host:port`.
pub fn redact(p: &str) -> String {
    match (p.split_once("://"), p.rsplit_once('@')) {
        (Some((scheme, _)), Some((_, host))) => format!("{scheme}://***@{host}"),
        _ => p.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_dns_leaking_socks() {
        assert!(Net::new(Some("socks5://127.0.0.1:9050".into()), true).is_err());
        assert!(Net::new(Some("ftp://x".into()), true).is_err());
    }

    #[test]
    fn isolation_rotates_credentials() {
        let n = Net::new(Some(TOR_PROXY.into()), true).unwrap();
        let (a, b) = (n.proxy_url().unwrap(), n.proxy_url().unwrap());
        assert_ne!(a, b);
        assert!(a.starts_with("socks5h://veil") && a.ends_with(":x@127.0.0.1:9050"));
        let pinned = n.pinned();
        assert_eq!(
            pinned.proxy_url(),
            pinned.proxy_url(),
            "a pinned route keeps one circuit"
        );
    }

    /// Answers one request on a local port: the head, then `chunks`, each
    /// after its delay. Returns the base URL.
    fn upstream(chunks: &'static [(u64, &'static str)]) -> String {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for mut conn in listener.incoming().flatten() {
                let mut r = BufReader::new(conn.try_clone().unwrap());
                while r.read_line(&mut String::new()).is_ok_and(|n| n > 2) {}
                let total: usize = chunks.iter().map(|c| c.1.len()).sum();
                let _ = write!(conn, "HTTP/1.1 200 OK\r\nContent-Length: {total}\r\n\r\n");
                for (delay, data) in chunks {
                    std::thread::sleep(Duration::from_millis(*delay));
                    let _ = conn.write_all(data.as_bytes());
                }
            }
        });
        url
    }

    #[test]
    fn production_clients_refuse_plain_http() {
        let url = upstream(&[(0, "hi")]);
        let direct = Net::new(None, true).unwrap();
        for client in [direct.client().unwrap(), direct.relay_client().unwrap()] {
            let err = client.get(&url).send().unwrap_err();
            assert!(
                err.is_builder(),
                "plain http must be refused up front: {err}"
            );
        }
        // Only the test-only switch allows it, and it survives `pinned`.
        let open = direct.allowing_http();
        assert_eq!(
            open.client()
                .unwrap()
                .get(&url)
                .send()
                .unwrap()
                .text()
                .unwrap(),
            "hi"
        );
        let pinned = open.pinned();
        assert!(pinned.client().unwrap().get(&url).send().is_ok());
    }

    #[test]
    fn the_relay_times_out_when_idle_but_not_in_total() {
        use std::io::Read;
        let net = Net::new(None, true).unwrap().allowing_http();
        let idle = Duration::from_millis(400);
        let client = net.relay_client_idle(idle).unwrap();

        // Four chunks 150 ms apart take longer than the idle timeout in
        // total, but never go quiet for that long: this must succeed.
        let url = upstream(&[(150, "a"), (150, "b"), (150, "c"), (150, "d")]);
        let started = std::time::Instant::now();
        let mut body = String::new();
        client
            .get(&url)
            .send()
            .unwrap()
            .read_to_string(&mut body)
            .unwrap();
        assert_eq!(body, "abcd");
        assert!(
            started.elapsed() > idle,
            "the test no longer outlasts the timeout"
        );

        // One chunk, then silence for 5 s: the read fails after about 400 ms.
        let url = upstream(&[(0, "a"), (5_000, "b")]);
        let started = std::time::Instant::now();
        let mut resp = client.get(&url).send().unwrap();
        let mut got = String::new();
        let err = resp.read_to_string(&mut got).unwrap_err();
        let timed_out = err
            .get_ref()
            .and_then(|e| e.downcast_ref::<reqwest::Error>())
            .is_some_and(|e| e.is_timeout());
        assert!(timed_out, "{err}");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn same_route_means_same_proxy_and_isolation() {
        let tor = || Net::new(Some(TOR_PROXY.into()), true).unwrap();
        assert!(tor().same_route(&tor()));
        assert!(!tor().same_route(&Net::new(None, true).unwrap()));
        assert!(!tor().same_route(&Net::new(Some(TOR_PROXY.into()), false).unwrap()));
        let http = Net::new(Some("http://10.0.0.1:3128".into()), true).unwrap();
        assert!(!tor().same_route(&http));
        // Isolation only means something over SOCKS, so it can't tell two HTTP proxies apart.
        assert!(http.same_route(&Net::new(Some("http://10.0.0.1:3128".into()), false).unwrap()));
    }

    #[test]
    fn no_isolation_for_http_or_user_creds() {
        let n = Net::new(Some("http://10.0.0.1:3128".into()), true).unwrap();
        assert_eq!(n.proxy_url().unwrap(), "http://10.0.0.1:3128");
        let n = Net::new(Some("socks5h://me:pw@h:1".into()), true).unwrap();
        assert_eq!(n.proxy_url().unwrap(), "socks5h://me:pw@h:1");
        assert_eq!(redact("socks5h://me:pw@h:1"), "socks5h://***@h:1");
    }
}
