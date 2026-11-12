//! A running stream's live chat, read only and text only.
//!
//! yt-dlp's live-chat reader (`Ytdlp::chat`) writes the chat to a file as it
//! arrives, one JSON line per action YouTube sent. This reads the messages
//! out of those lines (`parse_chat_line`), as the file grows (`Tail`).
//!
//! Only text is kept: an emoji becomes its `:name:`, and no picture (avatar,
//! badge, emoji) is ever fetched, nor is any link in a line followed.

use crate::ytdlp::ChatReader;
use anyhow::Result;
use serde::Serialize;
use serde_json::{Value, json};
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// How much of a message's text is kept, in characters.
const TEXT_MAX: usize = 500;
/// How much of an author's name is kept, in characters. YouTube's are far
/// shorter; this only bounds what a line that isn't YouTube's could hold.
const AUTHOR_MAX: usize = 200;
/// How often the chat file is looked at for what was added to it.
const POLL: Duration = Duration::from_millis(250);
/// The longest line that is read. yt-dlp's are a few kilobytes each (one
/// action); one that is longer than this is skipped, not held in memory.
const LINE_MAX: usize = 1 << 20;
/// How much of the chat file that has been read is let go at a time. The
/// file is in RAM (`$XDG_RUNTIME_DIR`), and a busy chat writes hundreds of
/// megabytes an hour, so what has been read is freed as it is read.
const FREE_EVERY: u64 = 1 << 20;

/// One chat message, as the window shows it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ChatMessage {
    pub author: String,
    pub text: String,
    /// Whether it is the channel's own.
    pub owner: bool,
    /// A Super Chat's amount, as YouTube words it ("$5.00").
    pub amount: Option<String>,
}

/// The messages in one line of yt-dlp's chat file. Anything else a line can
/// hold (a removal, a poll, a banner, a welcome) is no message, and a line
/// that isn't JSON has none either.
pub fn parse_chat_line(line: &str) -> Vec<ChatMessage> {
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return Vec::new();
    };
    let Some(actions) = v["replayChatItemAction"]["actions"].as_array() else {
        return Vec::new();
    };
    actions
        .iter()
        .filter_map(|a| message(&a["addChatItemAction"]["item"]))
        .collect()
}

/// The message an added chat item is, if it is a text message or a Super
/// Chat. A text message without text is none; a Super Chat says its amount
/// even without words.
fn message(item: &Value) -> Option<ChatMessage> {
    let (r, amount) = match (
        &item["liveChatTextMessageRenderer"],
        &item["liveChatPaidMessageRenderer"],
    ) {
        (r @ Value::Object(_), _) => (r, None),
        (_, r @ Value::Object(_)) => {
            let amount = clean(r["purchaseAmountText"]["simpleText"].as_str()?, TEXT_MAX);
            (r, Some(amount).filter(|a| !a.is_empty()))
        }
        _ => return None,
    };
    let text: String = r["message"]["runs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(run_text)
        .collect();
    let text = clean(&text, TEXT_MAX);
    if text.is_empty() && amount.is_none() {
        return None;
    }
    let owner =
        r["authorBadges"].as_array().into_iter().flatten().any(|b| {
            b["liveChatAuthorBadgeRenderer"]["icon"]["iconType"].as_str() == Some("OWNER")
        });
    Some(ChatMessage {
        author: clean(
            r["authorName"]["simpleText"].as_str().unwrap_or_default(),
            AUTHOR_MAX,
        ),
        text,
        owner,
        amount,
    })
}

/// A run of a message as text: its text, or an emoji's name (`:name:`), or
/// failing that the emoji itself (a plain one's id is its character).
fn run_text(run: &Value) -> Option<&str> {
    if let Some(text) = run["text"].as_str() {
        return Some(text);
    }
    let emoji = &run["emoji"];
    emoji["shortcuts"][0]
        .as_str()
        .or_else(|| emoji["emojiId"].as_str())
}

/// `s` without its control characters and bidi overrides and isolates
/// (which would turn the rest of the line around: an author named with
/// U+202E reads backwards), cut at `max` characters.
fn clean(s: &str, max: usize) -> String {
    s.chars()
        .filter(|&c| {
            !c.is_control() && !matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
        })
        .take(max)
        .collect()
}

/// Reads a file that another process is writing a line at a time, as it
/// grows: what `lines` returns is each whole line added since it was last
/// asked. The file is the first of `files` there is, looked for until one
/// is (yt-dlp writes the chat as `.part` and renames it once the stream
/// ends: a file that is open is read on after a rename).
///
/// What has been read is let go of on disk as it is read (see `free`), so
/// the file takes no more room than what is yet to be read, however long
/// the chat goes on.
pub struct Tail {
    files: [PathBuf; 2],
    file: Option<File>,
    /// The start of a line whose end hasn't been written yet.
    pending: Vec<u8>,
    /// Whether the line being read is too long (`LINE_MAX`) and skipped.
    skipping: bool,
    /// How far the file has been read, and how much of that is freed.
    read: u64,
    freed: u64,
    /// Freeing works here (it doesn't on every file system).
    frees: bool,
}

impl Tail {
    pub fn new(files: [PathBuf; 2]) -> Self {
        Tail {
            files,
            file: None,
            pending: Vec::new(),
            skipping: false,
            read: 0,
            freed: 0,
            frees: true,
        }
    }

    /// Lets go of what has been read of the file, once there is
    /// `FREE_EVERY` of it, by punching a hole over it: the pages go, while
    /// the file keeps its length, so the writer's place and the reader's are
    /// where they were, and nothing written meanwhile is lost (truncating
    /// would lose what came between reading and truncating). Whole pages
    /// only; the bytes freed are in hand already (as lines, or `pending`).
    fn free(&mut self, file: &File) {
        const PAGE: u64 = 4096;
        let upto = self.read / PAGE * PAGE;
        if !self.frees || upto < self.freed + FREE_EVERY {
            return;
        }
        let (Ok(at), Ok(len)) = (
            libc::off_t::try_from(self.freed),
            libc::off_t::try_from(upto - self.freed),
        ) else {
            return;
        };
        let mode = libc::FALLOC_FL_PUNCH_HOLE | libc::FALLOC_FL_KEEP_SIZE;
        // SAFETY: fallocate only reads its arguments; the descriptor is open.
        if unsafe { libc::fallocate(file.as_raw_fd(), mode, at, len) } == 0 {
            self.freed = upto;
        } else {
            note!(
                "qveil: the chat file can't be freed as it is read: {}",
                io::Error::last_os_error()
            );
            self.frees = false;
        }
    }

    /// The whole lines added since the last call.
    pub fn lines(&mut self) -> io::Result<Vec<String>> {
        let mut lines = Vec::new();
        if self.file.is_none() {
            for path in &self.files {
                // Open to write as well, which freeing needs; nothing is
                // written to it.
                let opened = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(path)
                    .or_else(|e| match e.kind() {
                        io::ErrorKind::PermissionDenied => File::open(path),
                        _ => Err(e),
                    });
                match opened {
                    Ok(f) => {
                        self.file = Some(f);
                        break;
                    }
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e),
                }
            }
        }
        let Some(mut file) = self.file.take() else {
            return Ok(lines);
        };
        let mut buf = [0u8; 16 << 10];
        let read = loop {
            match file.read(&mut buf) {
                Ok(0) => break Ok(lines),
                Ok(n) => {
                    self.read += n as u64;
                    self.take(&buf[..n], &mut lines);
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => break Err(e),
            }
        };
        self.free(&file);
        self.file = Some(file);
        read
    }

    /// What is left once the writer has ended: the lines added, and the last
    /// one even without its newline.
    pub fn rest(&mut self) -> io::Result<Vec<String>> {
        let mut lines = self.lines()?;
        if !self.skipping && !self.pending.is_empty() {
            lines.push(String::from_utf8_lossy(&self.pending).into_owned());
        }
        self.pending.clear();
        Ok(lines)
    }

    fn take(&mut self, mut chunk: &[u8], lines: &mut Vec<String>) {
        while let Some(end) = chunk.iter().position(|&b| b == b'\n') {
            if !self.skipping {
                self.pending.extend_from_slice(&chunk[..end]);
                lines.push(String::from_utf8_lossy(&self.pending).into_owned());
            }
            self.pending.clear();
            self.skipping = false;
            chunk = &chunk[end + 1..];
        }
        if !self.skipping {
            self.pending.extend_from_slice(chunk);
            if self.pending.len() > LINE_MAX {
                self.pending.clear();
                self.skipping = true;
            }
        }
    }
}

/// Reads the chat that `reader` writes, and says each message as it comes
/// (`say`, a JSON object each), until yt-dlp ends (true) or `wanted` says
/// no (false). The file is looked at every `POLL`, and at once when yt-dlp
/// ends. When nothing has been said for `heartbeat`, `{"heartbeat":true}` is:
/// a quiet chat keeps its connection alive.
pub fn follow(
    reader: &ChatReader,
    heartbeat: Duration,
    wanted: &dyn Fn() -> bool,
    say: &mut dyn FnMut(&Value) -> Result<()>,
) -> Result<bool> {
    let mut tail = Tail::new(reader.files());
    let mut last = Instant::now();
    loop {
        if !wanted() {
            return Ok(false);
        }
        let ended = reader.wait_exit(POLL);
        let lines = if ended { tail.rest()? } else { tail.lines()? };
        for line in lines {
            for m in parse_chat_line(&line) {
                say(&json!(m))?;
                last = Instant::now();
            }
        }
        if ended {
            return Ok(true);
        }
        if last.elapsed() >= heartbeat {
            say(&json!({ "heartbeat": true }))?;
            last = Instant::now();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Chat lines in the shape yt-dlp writes them (measured: see the spec's
    /// appendix), with made-up people in them: an emoji, plain text, a Super
    /// Chat, the channel's owner, and a welcome that is no message.
    const LINES: &str = include_str!("testdata/live-chat.jsonl");

    fn line(n: usize) -> &'static str {
        LINES.lines().nth(n).unwrap()
    }

    fn message(author: &str, text: &str) -> ChatMessage {
        ChatMessage {
            author: author.into(),
            text: text.into(),
            owner: false,
            amount: None,
        }
    }

    #[test]
    fn a_text_message_with_emoji_reads_as_text() {
        assert_eq!(
            parse_chat_line(line(0)),
            [message("@JacksunStone", ":cloud_with_lightning_and_rain:")]
        );
        assert_eq!(
            parse_chat_line(line(1)),
            [message(
                "@Invented.Viewer",
                "first time catching one of these live"
            )]
        );
    }

    #[test]
    fn a_super_chat_carries_its_amount() {
        assert_eq!(
            parse_chat_line(line(2)),
            [ChatMessage {
                amount: Some("$5.00".into()),
                ..message("@TipJarTester", "keep it up :hand-pink-waving:")
            }]
        );
        // One without words of its own is still said.
        let bare = json!({"replayChatItemAction": {"actions": [{"addChatItemAction": {"item": {
            "liveChatPaidMessageRenderer": {
                "authorName": {"simpleText": "@Quiet"},
                "purchaseAmountText": {"simpleText": "€2,00"}
            }
        }}}]}});
        assert_eq!(
            parse_chat_line(&bare.to_string()),
            [ChatMessage {
                amount: Some("€2,00".into()),
                ..message("@Quiet", "")
            }]
        );
    }

    #[test]
    fn the_owners_messages_are_marked() {
        // An emoji without a name is its own character.
        assert_eq!(
            parse_chat_line(line(3)),
            [ChatMessage {
                owner: true,
                ..message("@StreamHost", "thanks for coming 👋")
            }]
        );
        // A member's badge is not the owner's.
        assert!(!parse_chat_line(line(0))[0].owner);
    }

    #[test]
    fn what_is_no_message_is_skipped() {
        // The welcome, and lines that aren't what they should be.
        assert_eq!(parse_chat_line(line(4)), []);
        for odd in [
            "",
            "not json",
            "[]",
            r#"{"replayChatItemAction": {"actions": "x"}}"#,
            r#"{"replayChatItemAction": {"actions": [{"removeChatItemAction": {"targetItemId": "x"}}]}}"#,
            r#"{"replayChatItemAction": {"actions": [{"addChatItemAction": {"item": {"liveChatTextMessageRenderer": {"authorName": {"simpleText": "@A"}}}}}]}}"#,
        ] {
            assert_eq!(parse_chat_line(odd), [], "{odd}");
        }
    }

    #[test]
    fn every_message_in_a_line_is_read() {
        let text = |author: &str, words: &str| {
            json!({"addChatItemAction": {"item": {"liveChatTextMessageRenderer": {
                "authorName": {"simpleText": author},
                "message": {"runs": [{"text": words}]}
            }}}})
        };
        let both =
            json!({"replayChatItemAction": {"actions": [text("@A", "one"), text("@B", "two")]}});
        assert_eq!(
            parse_chat_line(&both.to_string()),
            [message("@A", "one"), message("@B", "two")]
        );
    }

    #[test]
    fn control_characters_go_and_long_text_is_cut_at_500() {
        let said = |author: &str, runs: Value| {
            let line = json!({"replayChatItemAction": {"actions": [{"addChatItemAction": {"item": {
                "liveChatTextMessageRenderer": {
                    "authorName": {"simpleText": author},
                    "message": {"runs": runs}
                }
            }}}]}});
            parse_chat_line(&line.to_string())
        };
        let got = said(
            "@Bell\u{7}Ringer\n",
            json!([{"text": "a\u{0}b\u{1b}[31mc\r\n\t"}, {"text": "x".repeat(600)}]),
        );
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].author, "@BellRinger");
        assert_eq!(got[0].text, format!("ab[31mc{}", "x".repeat(493)));
        assert_eq!(got[0].text.chars().count(), 500);
        // Characters, not bytes.
        let wide = said("@W", json!([{"text": "é".repeat(800)}]));
        assert_eq!(wide[0].text, "é".repeat(500));
        // A message that is nothing but control characters is none.
        assert_eq!(said("@Z", json!([{"text": "\u{0}\u{7}"}])), []);
    }

    #[test]
    fn bidi_overrides_and_isolates_go() {
        let line = json!({"replayChatItemAction": {"actions": [{"addChatItemAction": {"item": {
            "liveChatTextMessageRenderer": {
                "authorName": {"simpleText": "@\u{202E}evil\u{202C}name"},
                "message": {"runs": [{"text": "a\u{202A}b\u{202B}c\u{202D}d\u{2066}e\u{2067}f\u{2068}g\u{2069}h"}]}
            }
        }}}]}});
        let got = parse_chat_line(&line.to_string());
        assert_eq!(got, [message("@evilname", "abcdefgh")]);
        // Right-to-left text itself, and the joiner emoji need, stay.
        assert_eq!(clean("שלום 👩\u{200D}💻", 50), "שלום 👩\u{200D}💻");
    }

    #[test]
    fn what_has_been_read_of_the_chat_file_is_freed() {
        use std::io::Write;
        use std::os::unix::fs::MetadataExt;
        let dir = crate::store::testing::TempDir::new();
        let path = dir.0.join("c.live_chat.json.part");
        let mut writer = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap();
        let mut tail = Tail::new([path.clone(), dir.0.join("c.live_chat.json")]);
        // 8 MiB of chat, read as it is written, a poll every 64 lines.
        let one = format!("{{\"n\":\"{}\"}}", "x".repeat(4000));
        let total = 2048;
        let mut read = Vec::new();
        for n in 1..=total {
            writeln!(writer, "{one}").unwrap();
            if n % 64 == 0 {
                read.extend(tail.lines().unwrap());
            }
        }
        read.extend(tail.lines().unwrap());
        // Every line, whole, and none twice.
        assert_eq!(read.len(), total);
        assert!(read.iter().all(|l| *l == one));
        let meta = std::fs::metadata(&path).unwrap();
        // It still is as long as was written, so the writer goes on where
        // it was ...
        assert_eq!(meta.len(), (total * (one.len() + 1)) as u64);
        // ... but holds no more than about what was left to read.
        assert!(
            meta.blocks() * 512 < 2 * FREE_EVERY,
            "{} bytes held",
            meta.blocks() * 512
        );
        // And what is written after a hole is read as before.
        writeln!(writer, "after").unwrap();
        assert_eq!(tail.lines().unwrap(), ["after"]);
    }
}
