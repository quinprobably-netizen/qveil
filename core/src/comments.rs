//! A video's comments, as yt-dlp returns them: a flat list in which a reply
//! names its parent. They are only ever read, never written: there is no
//! account to write them from.

use serde::Serialize;
use serde_json::Value;

/// How much of one comment is kept, in characters. YouTube allows 10 000.
pub const TEXT_MAX: usize = 10_000;

/// The order YouTube is asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Sort {
    Top,
    New,
}

impl Sort {
    /// The word yt-dlp's `comment_sort` takes.
    pub fn as_str(self) -> &'static str {
        match self {
            Sort::Top => "top",
            Sort::New => "new",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Comment {
    pub id: String,
    pub author: String,
    pub text: String,
    pub likes: Option<u64>,
    /// Unix seconds, worked out from `time_text`, so only approximate.
    pub time: Option<i64>,
    /// What YouTube says: "3 days ago".
    #[serde(rename = "timeText")]
    pub time_text: Option<String>,
    pub pinned: bool,
    #[serde(rename = "byUploader")]
    pub by_uploader: bool,
    pub replies: Vec<Comment>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Comments {
    pub sort: Sort,
    /// How many comments the video has in all, when YouTube says.
    pub count: Option<u64>,
    pub comments: Vec<Comment>,
}

/// The comments in yt-dlp's answer for a video, as threads. A reply whose
/// thread isn't in the list (it was cut off by the limit) is left out.
/// yt-dlp can't tell a video without comments from one whose comments are
/// turned off, so neither is an error: both are an empty list.
pub fn parse(v: &Value, sort: Sort) -> Comments {
    let mut threads: Vec<Comment> = Vec::new();
    for c in v["comments"].as_array().into_iter().flatten() {
        let (Some(id), Some(text)) = (c["id"].as_str(), c["text"].as_str()) else {
            continue;
        };
        let comment = Comment {
            id: id.into(),
            author: c["author"].as_str().unwrap_or_default().into(),
            text: text.chars().take(TEXT_MAX).collect(),
            likes: c["like_count"].as_u64(),
            time: c["timestamp"].as_i64(),
            time_text: c["_time_text"].as_str().map(String::from),
            pinned: c["is_pinned"].as_bool() == Some(true),
            by_uploader: c["author_is_uploader"].as_bool() == Some(true),
            replies: Vec::new(),
        };
        match c["parent"].as_str() {
            None | Some("root") => threads.push(comment),
            Some(parent) => {
                if let Some(thread) = threads.iter_mut().find(|t| t.id == parent) {
                    thread.replies.push(comment);
                }
            }
        }
    }
    Comments {
        sort,
        count: v["comment_count"].as_u64(),
        comments: threads,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_flat_list_becomes_threads() {
        let v: Value = serde_json::from_str(
            r#"{"comment_count":1204,"comments":[
              {"id":"a","parent":"root","author":"@chan","text":"Fixed","like_count":640,"timestamp":1762700000,"_time_text":"9 hours ago","is_pinned":true,"author_is_uploader":true},
              {"id":"b","parent":"root","author":"@mara","text":"Nice"},
              {"id":"b.1","parent":"b","author":"@x","text":"Agreed","like_count":3},
              {"id":"z.1","parent":"z","author":"@lost","text":"orphan"}]}"#,
        )
        .unwrap();
        let c = parse(&v, Sort::Top);
        assert_eq!((c.count, c.comments.len()), (Some(1204), 2));
        assert!(c.comments[0].pinned && c.comments[0].by_uploader);
        assert_eq!(
            (c.comments[0].likes, c.comments[0].time_text.as_deref()),
            (Some(640), Some("9 hours ago"))
        );
        assert_eq!(c.comments[0].time, Some(1762700000));
        assert!(!c.comments[1].pinned && !c.comments[1].by_uploader);
        assert_eq!(c.comments[1].replies.len(), 1);
        assert_eq!(c.comments[1].replies[0].text, "Agreed");
    }

    #[test]
    fn nothing_there_is_an_empty_answer_and_long_text_is_cut() {
        assert_eq!(
            parse(&json!({}), Sort::New),
            Comments {
                sort: Sort::New,
                count: None,
                comments: vec![]
            }
        );
        let v = json!({"comments":[{"id":"a","parent":"root","author":"@a","text":"é".repeat(TEXT_MAX + 5)}]});
        assert_eq!(
            parse(&v, Sort::Top).comments[0].text.chars().count(),
            TEXT_MAX
        );
    }

    #[test]
    fn the_json_is_what_the_window_reads() {
        let v = json!({"comments":[{"id":"a","parent":"root","author":"@a","text":"t","_time_text":"now","author_is_uploader":true}]});
        let out = serde_json::to_value(parse(&v, Sort::Top)).unwrap();
        assert_eq!(out["sort"], "top");
        assert_eq!(out["comments"][0]["timeText"], "now");
        assert_eq!(out["comments"][0]["byUploader"], true);
        assert_eq!(out["comments"][0]["replies"], json!([]));
    }

    #[test]
    fn a_comment_without_text_or_id_is_left_out() {
        let v = json!({"comments":[{"parent":"root","text":"no id"},{"id":"b","parent":"root"},{"id":"c","parent":"root","text":"kept"}]});
        let c = parse(&v, Sort::Top);
        assert_eq!(c.comments.len(), 1);
        // No name is still a comment.
        assert_eq!(
            (c.comments[0].text.as_str(), c.comments[0].author.as_str()),
            ("kept", "")
        );
    }
}
