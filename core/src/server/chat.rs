//! `GET api/chat?v=<id>`: a running stream's live chat, read only and text
//! only.
//!
//! The window opens it while the Chat tab is on screen, and only if the
//! `live_chat` setting is on (it is refused with 409 otherwise). It runs
//! yt-dlp's live-chat reader over the route in use, on an identity of its own
//! (see `Ytdlp::chat`), and tails the file that writes (see `chat::follow`).
//! The lines it reads are NDJSON:
//!
//! - `{"author":..,"text":..,"owner":..,"amount":..}` for each message, as it
//!   comes (see `chat::ChatMessage`).
//! - `{"heartbeat":true}` when the chat has been quiet for a while
//!   (`Runtime::chat_heartbeat`), so that the connection is never idle for
//!   long. The window ignores it.
//! - The usual error line, for something that is not a video, a yt-dlp that
//!   failed, and a route that was left. The job ends.
//!
//! It ends by itself when the stream does (yt-dlp then ends), and when the
//! setting is turned off. yt-dlp is stopped as soon as the window hangs up:
//! within a poll of the file (a quarter of a second) and a look at the socket.

use super::Server;
use super::http::{NDJSON_HEAD, Request, Watch, respond};
use crate::chat;
use crate::errors;
use crate::ids;
use crate::inflight::ROUTE_CHANGED;
use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::io::Write;
use std::net::TcpStream;
use std::sync::Arc;

/// What a chat asked for while the setting is off is refused with.
const CHAT_OFF: &str = "Live chat is turned off in the settings";

impl Server {
    /// `GET api/chat?v=<id>`: see the module's docs.
    pub(super) fn chat(self: &Arc<Self>, out: &mut TcpStream, req: &Request) -> Result<()> {
        let rt = self.runtime();
        if !rt.cfg.live_chat {
            let body = errors::body(CHAT_OFF).to_string();
            return respond(out, 409, "application/json", body.as_bytes());
        }
        let watch = Watch::new(out);
        let mut lines = Lines {
            out,
            started: false,
        };
        let read = req.param("v").and_then(|target| {
            let v = ids::parse_video(target)
                .with_context(|| format!("{target:?} isn't a YouTube video"))?;
            let yt = rt.pinned_ytdlp();
            let reader = yt.chat(&v.watch_url())?;
            let wanted = || !watch.gone() && self.still_on(&rt) && self.runtime().cfg.live_chat;
            if chat::follow(&reader, rt.chat_heartbeat, &wanted, &mut |l| lines.say(l))? {
                return yt.chat_ended(reader);
            }
            // Nobody wants it: yt-dlp is stopped, and its files go.
            drop(reader);
            if !watch.gone() && !self.still_on(&rt) {
                bail!(ROUTE_CHANGED);
            }
            Ok(())
        });
        match read {
            Ok(()) => lines.head(),
            // Nobody is listening for the reason.
            Err(_) if watch.gone() => Ok(()),
            Err(e) => lines.say(&errors::body(&format!("{e:#}"))),
        }
    }
}

/// The reply, a line at a time, after its head.
struct Lines<'a> {
    out: &'a mut TcpStream,
    started: bool,
}

impl Lines<'_> {
    fn say(&mut self, line: &Value) -> Result<()> {
        let mut text = String::new();
        if !self.started {
            text.push_str(NDJSON_HEAD);
        }
        text.push_str(&format!("{line}\n"));
        self.out.write_all(text.as_bytes())?;
        self.started = true;
        Ok(())
    }

    /// The head, if no line has been said yet: a chat that ended without a
    /// word is still a reply.
    fn head(&mut self) -> Result<()> {
        if !self.started {
            self.out.write_all(NDJSON_HEAD.as_bytes())?;
            self.started = true;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Runtime;
    use crate::config::Config;
    use crate::server::testkit::*;
    use crate::store::testing::{TempDir, script};
    use serde_json::{Value, json};
    use std::io::{BufRead, BufReader, Read, Write};
    use std::path::Path;
    use std::time::{Duration, Instant};

    const ID: &str = "dQw4w9WgXcQ";
    /// Chat lines as yt-dlp writes them, with made-up people (see `chat`'s
    /// tests): an emoji, plain text, a Super Chat, the owner, a welcome.
    const LINES: &str = include_str!("../testdata/live-chat.jsonl");
    /// What yt-dlp does once a stream ends: the chat file gets its final name.
    const ENDED: &str = r#"mv "$part" "$base.live_chat.json""#;
    /// What it does while a stream goes on: it waits for more.
    const GOES_ON: &str = "exec sleep 60";

    fn fixture(n: usize) -> &'static str {
        LINES.lines().nth(n).unwrap()
    }

    /// A yt-dlp that reads a stream's chat: it records its config file (as
    /// `conf`), its private directory and its process id (see
    /// `hanging_running`), appends `lines` to the chat file one at a time,
    /// `pause` seconds apart, and then does `then` (shell).
    fn chat_ytdlp(dir: &Path, lines: &[&str], pause: &str, then: &str) -> String {
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(dir.join("lines"), text).unwrap();
        script(
            &dir.join("yt-dlp"),
            &format!(
                r#"d=$(dirname "$0")
conf=""
while [ $# -gt 0 ]; do [ "$1" = --config-locations ] && conf="$2"; shift; done
cp "$conf" "$d/conf"
dirname "$conf" > "$d/private-dir.new" && mv "$d/private-dir.new" "$d/private-dir"
echo $$ > "$d/pid.new" && mv "$d/pid.new" "$d/pid"
out=$(sed -n "s/^-o '\(.*\)'$/\1/p" "$conf")
base=$(printf '%s' "$out" | sed 's/\.%(ext)s$//')
part="$base.live_chat.json.part"
while IFS= read -r l; do printf '%s\n' "$l" >> "$part"; sleep {pause}; done < "$d/lines"
{then}
"#
            ),
        )
    }

    fn serve(ytdlp: String, tune: impl FnOnce(&mut Runtime)) -> (Arc<Server>, String, TempDir) {
        server_tuned(
            Config {
                ytdlp,
                ..Config::default()
            },
            tune,
        )
    }

    fn request(base: &str, query: &str) -> TcpStream {
        let mut c = TcpStream::connect(addr(base)).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        write!(
            c,
            "GET {} HTTP/1.1\r\nHost: x\r\n\r\n",
            path(base, &format!("api/chat?{query}"))
        )
        .unwrap();
        c
    }

    /// A chat that is still going: the response head is read, and each line
    /// as it comes.
    struct Open(BufReader<TcpStream>);

    impl Open {
        fn new(base: &str, query: &str) -> Self {
            let mut r = BufReader::new(request(base, query));
            let mut head = String::new();
            r.read_line(&mut head).unwrap();
            assert!(head.starts_with("HTTP/1.1 200"), "{head}");
            let mut ndjson = false;
            loop {
                head.clear();
                let n = r.read_line(&mut head).unwrap();
                assert!(n > 0, "the reply ended inside its head");
                ndjson |= head.contains("application/x-ndjson");
                if head == "\r\n" {
                    break;
                }
            }
            assert!(ndjson, "not NDJSON");
            Open(r)
        }

        /// The next line, or `None` once the reply has ended.
        fn next(&mut self) -> Option<Value> {
            let mut line = String::new();
            if self.0.read_line(&mut line).unwrap() == 0 {
                return None;
            }
            Some(serde_json::from_str(&line).unwrap_or_else(|e| panic!("{e}: {line:?}")))
        }

        fn line(&mut self) -> Value {
            self.next().expect("the chat ended")
        }
    }

    fn alive(pid: &str) -> bool {
        Path::new("/proc").join(pid).exists()
    }

    #[test]
    fn chat_streams_lines_as_the_file_grows() {
        let bin = TempDir::new();
        let ytdlp = chat_ytdlp(&bin.0, &[fixture(0), fixture(1), fixture(2)], "0.4", ENDED);
        let (_s, base, _d) = serve(ytdlp, |_| {});
        let mut open = Open::new(&base, &format!("v={ID}"));
        let first = open.line();
        let began = Instant::now();
        assert_eq!(
            first,
            json!({"author":"@JacksunStone","text":":cloud_with_lightning_and_rain:","owner":false,"amount":null})
        );
        assert_eq!(
            open.line(),
            json!({"author":"@Invented.Viewer","text":"first time catching one of these live","owner":false,"amount":null})
        );
        assert_eq!(
            open.line(),
            json!({"author":"@TipJarTester","text":"keep it up :hand-pink-waving:","owner":false,"amount":"$5.00"})
        );
        // Each came as it was written, not all of them at the end.
        let took = began.elapsed();
        assert!(took >= Duration::from_millis(600), "{took:?}");
        // The stream ended (yt-dlp is done, the file has its final name): so
        // does the chat.
        assert_eq!(open.next(), None);

        // What it was asked for, from where, and over which route are all in
        // its private file, which is gone once it is done.
        let (_, private) = hanging_running(&bin.0);
        let conf = std::fs::read_to_string(bin.0.join("conf")).unwrap();
        let lines: Vec<&str> = conf.lines().collect();
        assert_eq!(lines[0], "\u{feff}--proxy ''", "{conf}");
        for want in [
            "--skip-download".to_string(),
            "--write-subs".to_string(),
            "--sub-langs 'live_chat'".to_string(),
            format!("-o '{}/c.%(ext)s'", private.display()),
        ] {
            assert!(lines.contains(&want.as_str()), "no {want}:\n{conf}");
        }
        assert_eq!(
            lines.last(),
            Some(&format!("'https://www.youtube.com/watch?v={ID}'").as_str())
        );
        wait_for("the private files to go", Duration::from_secs(5), || {
            !private.exists()
        });
    }

    #[test]
    fn chat_goes_over_the_route_on_an_identity_of_its_own() {
        let bin = TempDir::new();
        let ytdlp = chat_ytdlp(&bin.0, &[fixture(1)], "0", ENDED);
        let (_s, base, _d) = serve(ytdlp, |_| {});
        switch_to_tor(&base);
        let mut proxies = Vec::new();
        for _ in 0..2 {
            let mut open = Open::new(&base, &format!("v={ID}"));
            assert_eq!(open.line()["author"], "@Invented.Viewer");
            assert_eq!(open.next(), None);
            let conf = std::fs::read_to_string(bin.0.join("conf")).unwrap();
            let proxy = conf.lines().next().unwrap().to_string();
            assert!(proxy.starts_with("\u{feff}--proxy 'socks5h://"), "{proxy}");
            assert!(proxy.ends_with("@127.0.0.1:9050'"), "{proxy}");
            proxies.push(proxy);
        }
        // Each chat is a circuit of its own.
        assert_ne!(proxies[0], proxies[1]);
    }

    #[test]
    fn chat_stops_when_the_window_hangs_up() {
        let bin = TempDir::new();
        let ytdlp = chat_ytdlp(&bin.0, &[fixture(1)], "0", GOES_ON);
        let (_s, base, _d) = serve(ytdlp, |_| {});
        let mut open = Open::new(&base, &format!("v={ID}"));
        assert_eq!(open.line()["author"], "@Invented.Viewer");
        let (pid, private) = hanging_running(&bin.0);
        assert!(alive(&pid));
        // The window closes the Chat tab, or leaves the stream.
        drop(open);
        wait_for("yt-dlp to be gone", Duration::from_secs(1), || !alive(&pid));
        wait_for("its private files to go", Duration::from_secs(5), || {
            !private.exists()
        });
    }

    #[test]
    fn chat_is_refused_when_the_setting_is_off() {
        let bin = TempDir::new();
        let ytdlp = chat_ytdlp(&bin.0, &[fixture(1)], "0", ENDED);
        let (_s, base, _d) = server_with(Config {
            ytdlp,
            live_chat: false,
            ..Config::default()
        });
        let (status, body) = http(&base, "GET", &path(&base, &format!("api/chat?v={ID}")), "");
        assert_eq!(status, 409, "{body}");
        let body: Value = serde_json::from_str(&body).unwrap();
        assert!(body["error"].as_str().is_some(), "{body}");
        assert!(body["code"].as_str().is_some(), "{body}");
        // Nothing was asked of YouTube.
        std::thread::sleep(Duration::from_millis(300));
        assert!(!bin.0.join("pid").exists());
    }

    #[test]
    fn a_silent_chat_sends_a_heartbeat() {
        let bin = TempDir::new();
        let ytdlp = chat_ytdlp(&bin.0, &[], "0", GOES_ON);
        let (_s, base, _d) = serve(ytdlp, |rt| rt.chat_heartbeat = Duration::from_millis(100));
        let mut open = Open::new(&base, &format!("v={ID}"));
        let began = Instant::now();
        for n in 0..3 {
            assert_eq!(open.line(), json!({"heartbeat":true}), "{n}");
        }
        let took = began.elapsed();
        assert!(took >= Duration::from_millis(150), "{took:?}");
        assert!(took < Duration::from_secs(3), "{took:?}");
    }

    #[test]
    fn a_chat_line_written_in_pieces_is_read_whole() {
        // yt-dlp's writes needn't end at the end of a line.
        let bin = TempDir::new();
        let whole = fixture(3);
        let mid = (0..=whole.len() / 2)
            .rev()
            .find(|&i| whole.is_char_boundary(i))
            .unwrap();
        let (a, b) = whole.split_at(mid);
        let ytdlp = script(
            &bin.0.join("yt-dlp"),
            &format!(
                r#"conf=""
while [ $# -gt 0 ]; do [ "$1" = --config-locations ] && conf="$2"; shift; done
part="$(dirname "$conf")/c.live_chat.json.part"
cat > "$part" <<'EOF1'
{a}
EOF1
truncate -s -1 "$part"
sleep 0.6
cat >> "$part" <<'EOF2'
{b}
EOF2
mv "$part" "$(dirname "$conf")/c.live_chat.json"
"#
            ),
        );
        let (_s, base, _d) = serve(ytdlp, |_| {});
        let mut open = Open::new(&base, &format!("v={ID}"));
        assert_eq!(
            open.line(),
            json!({"author":"@StreamHost","text":"thanks for coming 👋","owner":true,"amount":null})
        );
        assert_eq!(open.next(), None);
    }

    #[test]
    fn a_chat_that_cannot_be_read_says_why() {
        let bin = TempDir::new();
        let ytdlp = script(
            &bin.0.join("yt-dlp"),
            "echo 'ERROR: [youtube] x: Sign in to confirm you are not a bot' >&2\nexit 1\n",
        );
        let (_s, base, _d) = serve(ytdlp, |_| {});
        let mut open = Open::new(&base, &format!("v={ID}"));
        let said = open.line();
        assert!(said["error"].as_str().is_some(), "{said}");
        assert_eq!(said["code"], "bot_check", "{said}");
        assert_eq!(open.next(), None);
    }

    #[test]
    fn chat_ends_when_the_route_changes() {
        let bin = TempDir::new();
        let ytdlp = chat_ytdlp(&bin.0, &[fixture(1)], "0", GOES_ON);
        let (_s, base, _d) = serve(ytdlp, |_| {});
        let mut open = Open::new(&base, &format!("v={ID}"));
        assert_eq!(open.line()["author"], "@Invented.Viewer");
        let (pid, _) = hanging_running(&bin.0);
        switch_to_tor(&base);
        let said = open.line();
        assert_eq!(said["code"], "route_changed", "{said}");
        assert_eq!(open.next(), None);
        wait_for("yt-dlp to be gone", Duration::from_secs(1), || !alive(&pid));
    }

    #[test]
    fn chat_ends_when_it_is_turned_off() {
        let bin = TempDir::new();
        let ytdlp = chat_ytdlp(&bin.0, &[fixture(1)], "0", GOES_ON);
        let (_s, base, _d) = serve(ytdlp, |_| {});
        let mut open = Open::new(&base, &format!("v={ID}"));
        assert_eq!(open.line()["author"], "@Invented.Viewer");
        let (pid, _) = hanging_running(&bin.0);
        let (status, body) = http(
            &base,
            "POST",
            &path(&base, "api/config"),
            r#"{"liveChat":false}"#,
        );
        assert_eq!(status, 200, "{body}");
        assert_eq!(open.next(), None);
        wait_for("yt-dlp to be gone", Duration::from_secs(1), || !alive(&pid));
    }

    #[test]
    fn chat_for_something_that_is_not_a_video_says_so() {
        let bin = TempDir::new();
        let ytdlp = chat_ytdlp(&bin.0, &[fixture(1)], "0", ENDED);
        let (_s, base, _d) = serve(ytdlp, |_| {});
        for query in ["v=nope", ""] {
            let mut all = String::new();
            request(&base, query).read_to_string(&mut all).unwrap();
            let body = all.split_once("\r\n\r\n").unwrap().1;
            let lines: Vec<Value> = body
                .lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .collect();
            assert_eq!(lines.len(), 1, "{query}: {lines:?}");
            assert!(lines[0]["error"].as_str().is_some(), "{query}: {lines:?}");
        }
        assert!(!bin.0.join("pid").exists());
    }
}
