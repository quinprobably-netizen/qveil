//! Parsing of video and channel references.
//!
//! Every URL is reduced to the bare ID, so tracking parameters such as `si=`,
//! `pp=`, `feature=` and `list=` never reach the network. Only the start time
//! is kept.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoRef {
    pub id: String,
    pub start: Option<u64>,
}

impl VideoRef {
    pub fn watch_url(&self) -> String {
        format!("https://www.youtube.com/watch?v={}", self.id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelRef {
    /// A canonical `UC…` channel ID; needs no lookup to fetch its feed.
    Id(String),
    /// A handle or legacy URL that must be resolved to an ID.
    Url(String),
}

impl ChannelRef {
    pub fn url(&self) -> String {
        match self {
            ChannelRef::Id(id) => format!("https://www.youtube.com/channel/{id}"),
            ChannelRef::Url(u) => u.clone(),
        }
    }
}

fn id_chars(s: &str) -> bool {
    s.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

pub fn is_video_id(s: &str) -> bool {
    s.len() == 11 && id_chars(s)
}

pub fn is_channel_id(s: &str) -> bool {
    s.len() == 24 && s.starts_with("UC") && id_chars(s)
}

struct Url<'a> {
    host: String,
    path: Vec<&'a str>,
    query: Vec<(&'a str, &'a str)>,
}

const HOSTS: &[&str] = &["youtube.com", "youtu.be", "youtube-nocookie.com"];

fn parse_url(input: &str) -> Option<Url<'_>> {
    let rest = input
        .strip_prefix("https://")
        .or_else(|| input.strip_prefix("http://"))
        .unwrap_or(input);
    let rest = rest.split('#').next()?;
    let (host_path, query) = rest.split_once('?').unwrap_or((rest, ""));
    let (host, path) = host_path.split_once('/').unwrap_or((host_path, ""));
    let mut host = host.to_ascii_lowercase();
    for prefix in ["www.", "m.", "music."] {
        if let Some(h) = host.strip_prefix(prefix) {
            host = h.to_string();
        }
    }
    if !HOSTS.contains(&host.as_str()) {
        return None;
    }
    Some(Url {
        host,
        path: path.split('/').filter(|s| !s.is_empty()).collect(),
        query: query
            .split('&')
            .filter_map(|kv| kv.split_once('='))
            .collect(),
    })
}

/// Parses `90`, `90s`, `1m30s`, `1h2m3s`.
fn parse_start(t: &str) -> Option<u64> {
    if let Ok(n) = t.parse() {
        return Some(n);
    }
    let (mut total, mut num) = (0u64, String::new());
    for c in t.chars() {
        if c.is_ascii_digit() {
            num.push(c);
            continue;
        }
        let n: u64 = num.parse().ok()?;
        num.clear();
        total += n * match c {
            'h' => 3600,
            'm' => 60,
            's' => 1,
            _ => return None,
        };
    }
    num.is_empty().then_some(total)
}

pub fn parse_video(input: &str) -> Option<VideoRef> {
    let input = input.trim();
    if is_video_id(input) {
        return Some(VideoRef {
            id: input.into(),
            start: None,
        });
    }
    let url = parse_url(input)?;
    let id = match (url.host.as_str(), url.path.as_slice()) {
        ("youtu.be", [id, ..]) => *id,
        (_, ["watch"]) => url.query.iter().find(|(k, _)| *k == "v")?.1,
        (_, ["shorts" | "live" | "embed" | "v", id, ..]) => *id,
        _ => return None,
    };
    if !is_video_id(id) {
        return None;
    }
    let start = url
        .query
        .iter()
        .find(|(k, _)| *k == "t" || *k == "start")
        .and_then(|(_, v)| parse_start(v));
    Some(VideoRef {
        id: id.into(),
        start,
    })
}

pub fn parse_channel(input: &str) -> Option<ChannelRef> {
    let input = input.trim();
    if is_channel_id(input) {
        return Some(ChannelRef::Id(input.into()));
    }
    if let Some(handle) = input.strip_prefix('@') {
        return (!handle.is_empty() && !handle.contains('/'))
            .then(|| ChannelRef::Url(format!("https://www.youtube.com/@{handle}")));
    }
    let url = parse_url(input)?;
    match url.path.as_slice() {
        ["channel", id, ..] if is_channel_id(id) => Some(ChannelRef::Id((*id).into())),
        [h, ..] if h.starts_with('@') => {
            Some(ChannelRef::Url(format!("https://www.youtube.com/{h}")))
        }
        [kind @ ("c" | "user"), name, ..] => Some(ChannelRef::Url(format!(
            "https://www.youtube.com/{kind}/{name}"
        ))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(id: &str, start: Option<u64>) -> Option<VideoRef> {
        Some(VideoRef {
            id: id.into(),
            start,
        })
    }

    #[test]
    fn video_forms() {
        assert_eq!(parse_video("dQw4w9WgXcQ"), v("dQw4w9WgXcQ", None));
        assert_eq!(
            parse_video("https://www.youtube.com/watch?v=dQw4w9WgXcQ&si=TRACKME&pp=abc"),
            v("dQw4w9WgXcQ", None)
        );
        assert_eq!(
            parse_video("https://youtu.be/dQw4w9WgXcQ?si=x&t=90"),
            v("dQw4w9WgXcQ", Some(90))
        );
        assert_eq!(
            parse_video("m.youtube.com/shorts/dQw4w9WgXcQ"),
            v("dQw4w9WgXcQ", None)
        );
        assert_eq!(
            parse_video("https://music.youtube.com/watch?list=PL1&v=dQw4w9WgXcQ&t=1m30s"),
            v("dQw4w9WgXcQ", Some(90))
        );
        assert_eq!(
            parse_video("https://www.youtube-nocookie.com/embed/dQw4w9WgXcQ"),
            v("dQw4w9WgXcQ", None)
        );
    }

    #[test]
    fn video_rejects() {
        assert_eq!(
            parse_video("https://evil.example/watch?v=dQw4w9WgXcQ"),
            None
        );
        assert_eq!(
            parse_video("https://youtube.com.evil.example/watch?v=dQw4w9WgXcQ"),
            None
        );
        assert_eq!(parse_video("https://youtube.com/watch?v=short"), None);
        assert_eq!(parse_video("--exec=rm"), None);
    }

    #[test]
    fn start_times() {
        assert_eq!(parse_start("1h2m3s"), Some(3723));
        assert_eq!(parse_start("45s"), Some(45));
        assert_eq!(parse_start("4x"), None);
        assert_eq!(parse_start("4m5"), None);
    }

    #[test]
    fn channel_forms() {
        let id = "UCsBjURrPoezykLs9EqgamOA";
        assert_eq!(parse_channel(id), Some(ChannelRef::Id(id.into())));
        assert_eq!(
            parse_channel(&format!("https://www.youtube.com/channel/{id}/videos?si=x")),
            Some(ChannelRef::Id(id.into()))
        );
        assert_eq!(
            parse_channel("@NoBoilerplate"),
            Some(ChannelRef::Url(
                "https://www.youtube.com/@NoBoilerplate".into()
            ))
        );
        assert_eq!(
            parse_channel("https://youtube.com/@NoBoilerplate/featured"),
            Some(ChannelRef::Url(
                "https://www.youtube.com/@NoBoilerplate".into()
            ))
        );
        assert_eq!(parse_channel("https://example.com/@x"), None);
        assert_eq!(parse_channel("@a/b"), None);
    }
}
