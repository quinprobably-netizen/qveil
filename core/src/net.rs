//! Network identity: proxy routing and the HTTP client used for feeds.

use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;
use std::time::Duration;

/// The User-Agent qveil's requests send. The engine's own requests (feeds,
/// thumbnails, the relayed video bytes) send it, and every yt-dlp call is
/// told to send it too: otherwise the engine would say Firefox while
/// yt-dlp, which picks a random Chrome version for each call, said Chrome.
/// It has the shape of yt-dlp's own default, with a Chrome major version
/// inside the range yt-dlp currently picks from (145 to 151), so it is no
/// more conspicuous than yt-dlp's traffic is anyway. Bump it when yt-dlp's
/// range moves on.
///
/// It is a header, not an identity. The TLS and HTTP/2 handshakes are
/// rustls's and hyper's, not Chrome's, and yt-dlp's are Python's; and
/// yt-dlp's player lookups send the User-Agent of the YouTube client they
/// pretend to be. A circuit that carries a lookup and its video can show
/// more than one client, and the fingerprints don't match the browser the
/// header names.
pub const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/150.0.0.0 Safari/537.36";

pub const TOR_PROXY: &str = "socks5h://127.0.0.1:9050";

/// Precedes the proxy's address where `Net::explain` adds it to an error;
/// `errors::friendly` reads it back.
pub const PROXY_MARK: &str = "[proxy ";

/// How long a relayed stream may go without receiving a single byte.
pub const RELAY_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// HTTP/2 flow control: how far one stream, and one connection, may run
/// ahead of what has been read. A stream can carry no more than this per
/// round trip, and a round trip over Tor is half a second or more, so the
/// HTTP/2 default of 64 KiB (or hyper's 2 MiB) would hold a video back.
/// These are the window sizes Chrome uses. They don't make the connection
/// look like Chrome's, whose other SETTINGS, and their order, hyper doesn't copy.
const STREAM_WINDOW: u32 = 6 << 20;
const CONNECTION_WINDOW: u32 = 15 << 20;

/// How many idle connections to one host a client keeps for later
/// requests. A session's relay rarely has more than its video's, its
/// audio's and a spare open at once, and over Tor every one kept is a
/// connection the exit sees held open.
pub const POOLED_PER_HOST: usize = 4;
/// How long a client keeps an idle connection. googlevideo may close one
/// sooner; the relay counts the idle connections (`relay::Pool`) by the
/// same two numbers.
pub const POOLED_FOR: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, Default)]
pub struct Net {
    proxy: Option<String>,
    isolate: bool,
    /// Whether plain `http://` URLs may be fetched. Always false in
    /// production, so nothing leaves the machine unencrypted through the exit
    /// node or proxy; only tests, which talk to a local server, turn it on.
    allow_http: bool,
    /// Whether plain-http requests speak HTTP/2 without asking first, the
    /// way a local stand-in for googlevideo expects. Only tests set it; over
    /// TLS, HTTP/2 is agreed on in the handshake.
    h2c: bool,
    /// The relay's idle timeout, when not `RELAY_IDLE_TIMEOUT`. Only tests
    /// set it, to see one run out in less than a minute.
    relay_idle: Option<Duration>,
    /// A root certificate (PEM) trusted on top of the usual ones. Only
    /// tests set it, for a local stand-in that speaks TLS.
    trusted: Option<&'static [u8]>,
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
            h2c: false,
            relay_idle: None,
            trusted: None,
        })
    }

    /// The same route, but plain http is allowed (for tests with a local upstream).
    #[cfg(test)]
    pub fn allowing_http(mut self) -> Self {
        self.allow_http = true;
        self
    }

    /// The same route, speaking HTTP/2 in clear text to a local upstream.
    #[cfg(test)]
    pub fn speaking_h2c(mut self) -> Self {
        self.allow_http = true;
        self.h2c = true;
        self
    }

    /// The same route, trusting `root` (PEM) as well, for a local upstream
    /// over TLS. Nothing else changes: plain http is still refused.
    #[cfg(test)]
    pub fn trusting(mut self, root: &'static [u8]) -> Self {
        self.trusted = Some(root);
        self
    }

    /// The same route, with relay clients that give up after `idle`.
    #[cfg(test)]
    pub fn idling_after(mut self, idle: Duration) -> Self {
        self.relay_idle = Some(idle);
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

    /// What the route does, for the window's foot and `qveil config`. It only claims
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
        // Both parts random, so the credentials the proxy sees say nothing
        // about which program sent them.
        let (user, pass): (u64, u64) = (rand::random(), rand::random());
        Some(format!("{scheme}://{user:016x}:{pass:016x}@{rest}"))
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
    /// instead of holding a thread and a connection forever. The relay then
    /// replaces the client (see `Server::recover`): this one would keep its
    /// stalled HTTP/2 connection for every later request.
    pub fn relay_client(&self) -> Result<Client> {
        self.relay_client_idle(self.relay_idle.unwrap_or(RELAY_IDLE_TIMEOUT))
    }

    fn relay_client_idle(&self, idle: Duration) -> Result<Client> {
        self.build(Some(idle))
    }

    /// HTTP/2 where the server offers it, as googlevideo and YouTube do: the
    /// requests of one client then share one connection as streams, and a
    /// stream given up on part-way (the relay does that whenever the player
    /// seeks) is cancelled on its own while the connection stays open for
    /// the next request. Over HTTP/1.1 the connection would have to be
    /// closed, and over Tor a new one costs a TCP and a TLS handshake
    /// through the circuit, most of a second.
    fn build(&self, timeout: Option<Duration>) -> Result<Client> {
        let mut b = Client::builder()
            .user_agent(USER_AGENT)
            .referer(false)
            .https_only(!self.allow_http)
            .connect_timeout(Duration::from_secs(20))
            .timeout(timeout)
            .http2_initial_stream_window_size(STREAM_WINDOW)
            .http2_initial_connection_window_size(CONNECTION_WINDOW)
            .pool_max_idle_per_host(POOLED_PER_HOST)
            .pool_idle_timeout(POOLED_FOR)
            // The route is the one chosen in qveil, never HTTP_PROXY or
            // ALL_PROXY from the environment: on Direct, the app tells you
            // YouTube sees your IP, and that has to be true.
            .no_proxy();
        if self.h2c {
            b = b.http2_prior_knowledge();
        }
        if let Some(root) = self.trusted {
            b = b.add_root_certificate(reqwest::Certificate::from_pem(root)?);
        }
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
        // Random hex for both the user and the password, so nothing in them
        // names the app, and two calls share neither.
        let creds = |u: &str| {
            let rest = u.strip_prefix("socks5h://").unwrap();
            let (creds, host) = rest.rsplit_once('@').unwrap();
            assert_eq!(host, "127.0.0.1:9050");
            let (user, pass) = creds.split_once(':').unwrap();
            for part in [user, pass] {
                assert!(
                    part.len() == 16 && part.bytes().all(|b| b.is_ascii_hexdigit()),
                    "{u}"
                );
            }
            (user.to_string(), pass.to_string())
        };
        let ((ua, pa), (ub, pb)) = (creds(&a), creds(&b));
        assert!(ua != ub && pa != pb, "{a} {b}");
        assert!(!a.contains("qveil") && !b.contains("qveil"));
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
    fn a_stream_given_up_on_is_cancelled_and_its_connection_kept() {
        use crate::h2up::{BIG, big_bytes_h2};
        use std::io::Read;
        let up = H2Upstream::start();
        let client = Net::new(None, true)
            .unwrap()
            .speaking_h2c()
            .relay_client()
            .unwrap();
        let url = up.url("/big?pace=5");
        // Reads the start of a long range and gives up on the rest, as the
        // relay does when the player seeks or hangs up.
        let mut first = client.get(&url).header("range", "bytes=0-").send().unwrap();
        let mut buf = vec![0u8; 100 << 10];
        first.read_exact(&mut buf).unwrap();
        drop(first);
        let started = std::time::Instant::now();
        while up.resets() == 0 {
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "the stream was not cancelled"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        // The next request goes over the same connection.
        let mut body = Vec::new();
        client
            .get(up.url("/big"))
            .header("range", format!("bytes={}-", BIG - 1000))
            .send()
            .unwrap()
            .read_to_end(&mut body)
            .unwrap();
        assert!(body == big_bytes_h2(BIG - 1000, BIG), "the bytes differ");
        assert_eq!(
            up.connections(),
            1,
            "a new connection for the second request"
        );
    }

    use crate::h2up::H2Upstream;

    #[test]
    fn a_stream_may_run_ahead_of_its_reader_by_a_generous_window() {
        let up = H2Upstream::start();
        let client = Net::new(None, true)
            .unwrap()
            .speaking_h2c()
            .relay_client()
            .unwrap();
        // Nothing of the body is read, so what arrives is what flow control
        // lets through: over Tor's long round trips, a small window would
        // hold the stream back.
        let _held = client
            .get(up.url(&format!("/big?size={}", 8 << 20)))
            .header("range", "bytes=0-")
            .send()
            .unwrap();
        // It gets that far however slow the sender is (10 s is far more
        // than it takes)...
        let started = std::time::Instant::now();
        while up.sent() < STREAM_WINDOW as usize {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "{} KiB let through",
                up.sent() >> 10
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        // ...and no further, with nothing read.
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(
            up.sent(),
            STREAM_WINDOW as usize,
            "{} KiB let through",
            up.sent() >> 10
        );
    }

    #[test]
    fn a_client_keeps_no_more_than_a_few_idle_connections() {
        use std::io::{BufRead, BufReader, Write};
        use std::sync::{Arc, Barrier};
        const AT_ONCE: usize = POOLED_PER_HOST + 2;
        // Answers each request only once `AT_ONCE` of them are waiting, so
        // every round needs that many connections at the same time. Counts
        // the requests each connection served.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let served = Arc::new(Mutex::new(Vec::<usize>::new()));
        let barrier = Arc::new(Barrier::new(AT_ONCE));
        let (log, gate) = (Arc::clone(&served), Arc::clone(&barrier));
        std::thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                let (log, gate) = (Arc::clone(&log), Arc::clone(&gate));
                std::thread::spawn(move || {
                    let mut out = conn.try_clone().unwrap();
                    let mut r = BufReader::new(conn);
                    let me = {
                        let mut log = log.lock().unwrap();
                        log.push(0);
                        log.len() - 1
                    };
                    loop {
                        let mut line = String::new();
                        while r.read_line(&mut line).is_ok_and(|n| n > 2) {
                            line.clear();
                        }
                        if line.is_empty() {
                            return;
                        }
                        log.lock().unwrap()[me] += 1;
                        gate.wait();
                        let _ = write!(out, "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi");
                    }
                });
            }
        });
        let client = Net::new(None, true)
            .unwrap()
            .allowing_http()
            .relay_client()
            .unwrap();
        let round = || {
            std::thread::scope(|s| {
                for _ in 0..AT_ONCE {
                    s.spawn(|| {
                        let body = client.get(&url).send().unwrap().text().unwrap();
                        assert_eq!(body, "hi");
                    });
                }
            })
        };
        round();
        assert_eq!(served.lock().unwrap().len(), AT_ONCE);
        // Time for the connections to go back to the pool.
        std::thread::sleep(Duration::from_millis(300));
        round();
        // Of the connections the first round opened, only the few the
        // client kept were used again. (Fewer, if the client hadn't put them
        // back yet; never more.)
        let reused = served.lock().unwrap().iter().filter(|&&n| n > 1).count();
        assert!(reused <= POOLED_PER_HOST, "{reused} reused");
    }

    #[test]
    fn over_tls_the_production_clients_agree_on_http2() {
        use crate::h2up::{TEST_ROOT, big_bytes_h2};
        let up = H2Upstream::start_tls();
        // What production builds, with only the stand-in's root added: no
        // plain http, no h2 without asking. ALPN has to pick it.
        let net = Net::new(None, true).unwrap().trusting(TEST_ROOT);
        for client in [net.client().unwrap(), net.relay_client().unwrap()] {
            let resp = client
                .get(up.url("/big?size=5000"))
                .header("range", "bytes=10-19")
                .send()
                .unwrap();
            assert_eq!(resp.version(), reqwest::Version::HTTP_2);
            assert_eq!(resp.status(), 206);
            assert!(resp.bytes().unwrap() == big_bytes_h2(10, 20));
        }
        assert!(up.url("/").starts_with("https://127.0.0.1:"));
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
