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
use std::fs::File;
use std::io::{self, Read};
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

/// `s` without its control characters, cut at `max` characters.
fn clean(s: &str, max: usize) -> String {
    s.chars().filter(|c| !c.is_control()).take(max).collect()
}

/// Reads a file that another process is writing a line at a time, as it
/// grows: what `lines` returns is each whole line added since it was last
/// asked. The file is the first of `files` there is, looked for until one
/// is (yt-dlp writes the chat as `.part` and renames it once the stream
/// ends: a file that is open is read on after a rename).
pub struct Tail {
    files: [PathBuf; 2],
    file: Option<File>,
    /// The start of a line whose end hasn't been written yet.
    pending: Vec<u8>,
    /// Whether the line being read is too long (`LINE_MAX`) and skipped.
    skipping: bool,
}

impl Tail {
    pub fn new(files: [PathBuf; 2]) -> Self {
        Tail {
            files,
            file: None,
            pending: Vec::new(),
            skipping: false,
        }
    }

    /// The whole lines added since the last call.
    pub fn lines(&mut self) -> io::Result<Vec<String>> {
        let mut lines = Vec::new();
        if self.file.is_none() {
            for path in &self.files {
                match File::open(path) {
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
                Ok(n) => self.take(&buf[..n], &mut lines),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => break Err(e),
            }
        };
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
}
