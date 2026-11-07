//! Network identity: proxy routing and the HTTP client used for feeds.

use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;
use std::time::Duration;

/// A common desktop browser UA, so requests don't announce "veil" or "reqwest".
pub const USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:140.0) Gecko/20100101 Firefox/140.0";

pub const TOR_PROXY: &str = "socks5h://127.0.0.1:9050";

#[derive(Clone, Debug, Default)]
pub struct Net {
    proxy: Option<String>,
    isolate: bool,
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
        Ok(Self { proxy, isolate })
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
        }
    }

    /// A fresh client: no cookie store, no Referer, browser UA, proxy applied.
    pub fn client(&self) -> Result<Client> {
        self.build(Some(Duration::from_secs(20)))
    }

    /// Like `client`, but without a total timeout, because a relayed video
    /// body can stream for an hour.
    pub fn relay_client(&self) -> Result<Client> {
        self.build(None)
    }

    fn build(&self, timeout: Option<Duration>) -> Result<Client> {
        let mut b = Client::builder()
            .user_agent(USER_AGENT)
            .referer(false)
            .https_only(true)
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

fn redact(p: &str) -> String {
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

    #[test]
    fn no_isolation_for_http_or_user_creds() {
        let n = Net::new(Some("http://10.0.0.1:3128".into()), true).unwrap();
        assert_eq!(n.proxy_url().unwrap(), "http://10.0.0.1:3128");
        let n = Net::new(Some("socks5h://me:pw@h:1".into()), true).unwrap();
        assert_eq!(n.proxy_url().unwrap(), "socks5h://me:pw@h:1");
        assert_eq!(redact("socks5h://me:pw@h:1"), "socks5h://***@h:1");
    }
}
