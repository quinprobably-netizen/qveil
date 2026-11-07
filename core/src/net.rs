//! Network identity: proxy routing and the HTTP client used for feeds.

use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;
use std::time::Duration;

/// The one browser identity qveil presents. The engine's own requests (feeds,
/// thumbnails) send it, and every yt-dlp call is told to send it too, so the
/// video bytes carry it as well: otherwise the engine would say Firefox while
/// yt-dlp, which picks a random Chrome version for each call, said Chrome, and
/// one circuit would show two browsers. It has the shape of yt-dlp's own
/// default, with a Chrome major version inside the range yt-dlp currently
/// picks from (145 to 151), so it is no more conspicuous than yt-dlp's
/// traffic is anyway. Bump it when yt-dlp's range moves on.
pub const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/150.0.0.0 Safari/537.36";

pub const TOR_PROXY: &str = "socks5h://127.0.0.1:9050";

/// Precedes the proxy's address where `Net::explain` adds it to an error;
/// `errors::friendly` reads it back.
pub const PROXY_MARK: &str = "[proxy ";

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
        // Tor is only private enough with a circuit per request: one shared
        // circuit lets the exit link the feeds, searches and videos you
        // watch. So that is not left to a setting. Whether a SOCKS proxy is
        // Tor can't be told from its address (Tor Browser listens on 9150, a
        // system Tor may be reached as `localhost` or through a tunnel on any
        // port), so the rule can't be tied to one address: it holds for every
        // SOCKS proxy. A proxy URL that carries its own credentials is the
        // deliberate way to share one identity, and `isolates` then says so.
        if let Some(p) = &proxy
            && !isolate
            && is_socks_url(p)
            && !has_credentials(p)
        {
            bail!(
                "isolate_circuits = false is not allowed with a SOCKS proxy: qveil can't tell Tor from another proxy by its address, and one shared circuit lets the exit link your feeds, searches and videos. Remove the setting, or put your own user:password in the proxy URL to share one identity on purpose"
            );
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
        self.proxy.as_deref().is_some_and(is_socks_url)
    }

    /// What the route does, for the top bar and `qveil config`. It only claims
    /// what `proxy_url` really does.
    pub fn describe(&self) -> String {
        let Some(p) = &self.proxy else {
            return "none: YouTube sees your real IP".into();
        };
        let shown = redact(p);
        if !self.is_socks() {
            shown
        } else if has_credentials(p) {
            // The URL's own credentials go with every request, which is one
            // identity for all of them: Tor would put them on one circuit.
            format!(
                "{shown} (not isolated: every request uses the credentials in the URL, so they share one circuit)"
            )
        } else if self.isolate {
            format!("{shown} (fresh credentials per request; Tor gives each its own circuit)")
        } else {
            format!("{shown} (not isolated: one shared identity)")
        }
    }

    /// Proxy URL for one logical request. With isolation on, each call gets
    /// new random SOCKS credentials, so Tor (with IsolateSOCKSAuth, its
    /// default) builds a separate circuit and requests can't be linked by
    /// exit IP. Credentials the user put in the URL are used as given, and
    /// then every request shares them: `isolates` is false in that case.
    pub fn proxy_url(&self) -> Option<String> {
        let p = self.proxy.as_ref()?;
        if !self.isolates() {
            return Some(p.clone());
        }
        let (scheme, rest) = p.split_once("://")?;
        let user: u64 = rand::random();
        Some(format!("{scheme}://qveil{user:016x}:x@{rest}"))
    }

    pub fn proxy(&self) -> Option<&str> {
        self.proxy.as_deref()
    }

    /// Names the proxy in an error that says a connection was refused.
    /// Neither reqwest nor yt-dlp say which proxy they were talking to, and
    /// `friendly` has to tell "Tor isn't running" from "your own proxy is
    /// down" from an error that has nothing to do with the route.
    pub fn explain(&self, err: &str) -> String {
        let addr = self.proxy.as_deref().map(|p| {
            let rest = p.split_once("://").map_or(p, |(_, rest)| rest);
            rest.rsplit_once('@').map_or(rest, |(_, host)| host)
        });
        match addr {
            Some(addr) if err.to_ascii_lowercase().contains("connection refused") => {
                format!("{err} {PROXY_MARK}{addr}]")
            }
            _ => err.to_string(),
        }
    }

    /// Whether each request really gets an identity of its own: only over
    /// SOCKS, and only when the proxy URL has no credentials of its own,
    /// because those are sent with every request and make them all one.
    pub fn isolates(&self) -> bool {
        self.isolate
            && self
                .proxy
                .as_deref()
                .is_some_and(|p| is_socks_url(p) && !has_credentials(p))
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
            .timeout(timeout)
            // The route is the one chosen in qveil, never HTTP_PROXY or
            // ALL_PROXY from the environment: on Direct, the app tells you
            // YouTube sees your IP, and that has to be true.
            .no_proxy();
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

fn is_socks_url(proxy: &str) -> bool {
    proxy.starts_with("socks")
}

/// Whether the proxy URL has a `user:password@` part of its own.
fn has_credentials(proxy: &str) -> bool {
    proxy
        .split_once("://")
        .is_some_and(|(_, rest)| rest.contains('@'))
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
    use std::sync::Mutex;

    #[test]
    fn rejects_dns_leaking_socks() {
        assert!(Net::new(Some("socks5://127.0.0.1:9050".into()), true).is_err());
        assert!(Net::new(Some("ftp://x".into()), true).is_err());
    }

    #[test]
    fn every_socks_proxy_isolates_and_cannot_opt_out() {
        // Tor can't be told from another SOCKS proxy by its address, so none
        // of these may turn isolation off: the preset, `localhost`, Tor
        // Browser's port, a tunnel on any port, an IPv6 address.
        for proxy in [
            TOR_PROXY,
            "socks5h://localhost:9050",
            "socks5h://127.0.0.1:9150",
            "socks5h://10.0.0.2:1080",
            "socks5h://[::1]:9050",
        ] {
            let err = Net::new(Some(proxy.into()), false).unwrap_err();
            assert!(
                err.to_string().contains("not allowed with a SOCKS proxy"),
                "{proxy}: {err}"
            );
            let net = Net::new(Some(proxy.into()), true).unwrap();
            assert!(net.isolates(), "{proxy}");
            assert!(
                net.describe().contains("fresh credentials per request"),
                "{proxy}: {}",
                net.describe()
            );
        }
        // Where isolation means nothing, switching it off is harmless.
        assert!(Net::new(Some("http://10.0.0.1:3128".into()), false).is_ok());
        assert!(Net::new(None, false).is_ok());
    }

    #[test]
    fn credentials_in_the_url_mean_no_isolation_and_say_so() {
        for isolate in [true, false] {
            let net = Net::new(Some("socks5h://me:pw@10.0.0.2:1080".into()), isolate).unwrap();
            assert!(!net.isolates(), "isolate = {isolate}");
            // Used as given, every time: one identity for everything.
            let (a, b) = (net.proxy_url().unwrap(), net.proxy_url().unwrap());
            assert_eq!(
                (a.as_str(), a == b),
                ("socks5h://me:pw@10.0.0.2:1080", true)
            );
            let said = net.describe();
            assert!(
                said.contains("not isolated") && said.contains("share one circuit"),
                "{said}"
            );
            assert!(!said.contains("fresh") && !said.contains("pw"), "{said}");
        }
        // The same goes for Tor's own address with credentials added.
        let tor = Net::new(Some("socks5h://me:pw@127.0.0.1:9050".into()), true).unwrap();
        assert!(!tor.isolates() && tor.describe().contains("not isolated"));
        // What the engine reports to the window agrees with the route.
        assert!(!tor.same_route(&Net::new(Some(TOR_PROXY.into()), true).unwrap()));
    }

    #[test]
    fn only_a_socks_proxy_without_credentials_has_isolation_to_describe() {
        let plain = Net::new(Some("socks5h://10.0.0.2:1080".into()), true).unwrap();
        assert!(plain.describe().starts_with("socks5h://10.0.0.2:1080 ("));
        let http = Net::new(Some("http://u:pw@10.0.0.1:3128".into()), true).unwrap();
        assert!(!http.isolates());
        assert_eq!(http.describe(), "http://***@10.0.0.1:3128");
        assert_eq!(
            Net::new(None, true).unwrap().describe(),
            "none: YouTube sees your real IP"
        );
    }

    #[test]
    fn refused_connections_name_the_proxy() {
        let tor = Net::new(Some(TOR_PROXY.into()), true).unwrap();
        assert_eq!(
            tor.explain("socks connect error: Connection refused"),
            "socks connect error: Connection refused [proxy 127.0.0.1:9050]"
        );
        let creds = Net::new(Some("http://me:pw@10.0.0.1:3128".into()), true).unwrap();
        let named = creds.explain("tunnel error: connection refused");
        assert!(named.ends_with("[proxy 10.0.0.1:3128]") && !named.contains("pw"));
        assert_eq!(tor.explain("HTTP Error 404"), "HTTP Error 404");
        let direct = Net::new(None, true).unwrap();
        assert_eq!(direct.explain("Connection refused"), "Connection refused");
    }

    #[test]
    fn isolation_rotates_credentials() {
        let n = Net::new(Some(TOR_PROXY.into()), true).unwrap();
        let (a, b) = (n.proxy_url().unwrap(), n.proxy_url().unwrap());
        assert_ne!(a, b);
        assert!(a.starts_with("socks5h://qveil") && a.ends_with(":x@127.0.0.1:9050"));
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
    fn every_client_sends_the_one_browser_identity() {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let seen = std::sync::Arc::new(Mutex::new(Vec::new()));
        let log = std::sync::Arc::clone(&seen);
        std::thread::spawn(move || {
            for mut conn in listener.incoming().flatten() {
                let mut r = BufReader::new(conn.try_clone().unwrap());
                let mut line = String::new();
                while r.read_line(&mut line).is_ok_and(|n| n > 2) {}
                let agent = line
                    .lines()
                    .find_map(|l| {
                        l.strip_prefix("user-agent: ")
                            .or(l.strip_prefix("User-Agent: "))
                    })
                    .map(String::from);
                log.lock().unwrap().push(agent);
                let _ = write!(conn, "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
            }
        });
        let net = Net::new(None, true).unwrap().allowing_http();
        net.client().unwrap().get(&url).send().unwrap();
        net.relay_client().unwrap().get(&url).send().unwrap();
        let seen = seen.lock().unwrap().clone();
        let want = Some(USER_AGENT.to_string());
        assert_eq!(seen, [want.clone(), want]);
        // The same string is what yt-dlp is told to send (see `Ytdlp::command`),
        // and it has to look like a desktop browser, not like qveil or reqwest.
        assert!(USER_AGENT.starts_with("Mozilla/5.0 (Windows NT 10.0; Win64; x64)"));
        assert!(!USER_AGENT.to_ascii_lowercase().contains("qveil"));
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

    /// Run in a child process by `direct_ignores_the_proxy_environment`,
    /// because the environment can't be changed in a test binary whose other
    /// threads are building clients; a test that isn't called that way does nothing.
    #[test]
    fn child_of_direct_ignores_the_proxy_environment() {
        let (Ok(url), Ok(mode)) = (
            std::env::var("QVEIL_TEST_URL"),
            std::env::var("QVEIL_TEST_MODE"),
        ) else {
            return;
        };
        if mode == "plain" {
            // What a client does unless told otherwise: it follows the environment.
            let client = Client::builder()
                .timeout(Duration::from_millis(700))
                .build()
                .unwrap();
            let _ = client.get(&url).send();
        } else {
            let net = Net::new(None, true).unwrap().allowing_http();
            for client in [net.client().unwrap(), net.relay_client().unwrap()] {
                assert_eq!(client.get(&url).send().unwrap().text().unwrap(), "hi");
            }
        }
    }

    #[test]
    fn direct_ignores_the_proxy_environment() {
        // The proxy named in the environment is a listener that just counts
        // who connects to it.
        let proxy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        proxy.set_nonblocking(true).unwrap();
        let proxy_url = format!("http://{}", proxy.local_addr().unwrap());
        let url = upstream(&[(0, "hi")]);
        let run = |mode: &str| {
            let mut c = std::process::Command::new(std::env::current_exe().unwrap());
            c.args([
                "--exact",
                "net::tests::child_of_direct_ignores_the_proxy_environment",
            ])
            .env("QVEIL_TEST_URL", &url)
            .env("QVEIL_TEST_MODE", mode)
            .env_remove("NO_PROXY")
            .env_remove("no_proxy");
            for var in ["HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"] {
                c.env(var, &proxy_url);
            }
            let out = c.output().unwrap();
            assert!(
                out.status.success(),
                "{mode}: {}",
                String::from_utf8_lossy(&out.stdout)
            );
            proxy.accept().is_ok()
        };
        assert!(
            run("plain"),
            "the control: an ordinary client does use the proxy from the environment"
        );
        assert!(
            !run("direct"),
            "the Direct route must not use the proxy from the environment"
        );
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
        // Credentials of its own make a different route from the same proxy without.
        let other = |p: &str| Net::new(Some(p.into()), true).unwrap();
        assert!(
            !other("socks5h://10.0.0.2:1080").same_route(&other("socks5h://a:b@10.0.0.2:1080"))
        );
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
