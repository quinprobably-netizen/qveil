//! Subscriptions are a local list of channel IDs. Imports read Google Takeout
//! CSV and NewPipe/FreeTube JSON offline, without any network requests.

use crate::ids::{self, ChannelRef};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sub {
    pub id: String,
    pub name: String,
}

#[derive(Default, Serialize, Deserialize)]
pub struct Subs {
    pub channels: Vec<Sub>,
}

impl Subs {
    /// Returns false if already subscribed.
    pub fn add(&mut self, sub: Sub) -> bool {
        if self.channels.iter().any(|s| s.id == sub.id) {
            return false;
        }
        self.channels.push(sub);
        self.channels.sort_by_key(|s| s.name.to_lowercase());
        true
    }

    /// Removes by channel ID, or by exact (case-insensitive) name.
    pub fn remove(&mut self, query: &str) -> Option<Sub> {
        let q = query.trim();
        let id = match ids::parse_channel(q) {
            Some(ChannelRef::Id(id)) => Some(id),
            _ => None,
        };
        let i = self
            .channels
            .iter()
            .position(|s| Some(&s.id) == id.as_ref() || s.name.eq_ignore_ascii_case(q))?;
        Some(self.channels.remove(i))
    }

    pub fn export_newpipe(&self) -> Value {
        json!({
            "app_version": "0.0.0",
            "app_version_int": 0,
            "subscriptions": self.channels.iter().map(|s| json!({
                "service_id": 0,
                "url": format!("https://www.youtube.com/channel/{}", s.id),
                "name": s.name,
            })).collect::<Vec<_>>(),
        })
    }
}

pub fn parse_import(text: &str) -> Result<Vec<Sub>> {
    let text = text.trim_start_matches('\u{feff}').trim();
    let subs = if text.starts_with('{') || text.starts_with('[') {
        // NewPipe exports one JSON object; FreeTube exports one profile object per line.
        let values: Vec<Value> = match serde_json::from_str(text) {
            Ok(v) => vec![v],
            Err(_) => text
                .lines()
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect(),
        };
        values.iter().flat_map(json_subs).collect::<Vec<_>>()
    } else {
        text.lines().filter_map(csv_sub).collect()
    };
    if subs.is_empty() {
        bail!(
            "no YouTube channels found (expected a Takeout subscriptions.csv or a NewPipe/FreeTube export)"
        );
    }
    Ok(subs)
}

fn json_subs(v: &Value) -> Vec<Sub> {
    let items = v["subscriptions"].as_array().or(v.as_array());
    items
        .into_iter()
        .flatten()
        .filter(|s| s["service_id"].as_i64().unwrap_or(0) == 0)
        .filter_map(|s| {
            let raw = s["url"].as_str().or(s["id"].as_str())?;
            let Some(ChannelRef::Id(id)) = ids::parse_channel(raw) else {
                return None;
            };
            let name = s["name"]
                .as_str()
                .filter(|n| !n.is_empty())
                .unwrap_or(&id)
                .to_string();
            Some(Sub { id, name })
        })
        .collect()
}

/// Takeout format: `Channel Id,Channel Url,Channel Title`.
fn csv_sub(line: &str) -> Option<Sub> {
    let f = csv_fields(line.trim_end_matches('\r'));
    let id = f.first()?.trim();
    if !ids::is_channel_id(id) {
        return None; // header row, blank line, or garbage
    }
    let name = f
        .get(2)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or(id);
    Some(Sub {
        id: id.into(),
        name: name.into(),
    })
}

fn csv_fields(line: &str) -> Vec<String> {
    let (mut out, mut cur, mut quoted) = (Vec::new(), String::new(), false);
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match (c, quoted) {
            ('"', true) if chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            ('"', _) => quoted = !quoted,
            (',', false) => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takeout_csv() {
        let csv = "Channel Id,Channel Url,Channel Title\r\n\
            UCsBjURrPoezykLs9EqgamOA,http://www.youtube.com/channel/UCsBjURrPoezykLs9EqgamOA,Fireship\r\n\
            UCUMwY9iS8oMyWDYIe6_RmoA,http://www.youtube.com/channel/UCUMwY9iS8oMyWDYIe6_RmoA,\"No Boilerplate, \"\"quoted\"\"\"\r\n";
        let subs = parse_import(csv).unwrap();
        assert_eq!(subs.len(), 2);
        assert_eq!(subs[1].name, "No Boilerplate, \"quoted\"");
    }

    #[test]
    fn newpipe_roundtrip_and_freetube() {
        let mut s = Subs::default();
        s.add(Sub {
            id: "UCsBjURrPoezykLs9EqgamOA".into(),
            name: "Fireship".into(),
        });
        let exported = s.export_newpipe().to_string();
        assert_eq!(parse_import(&exported).unwrap(), s.channels);

        let freetube = r#"{"_id":"allChannels","name":"All","subscriptions":[{"id":"UCsBjURrPoezykLs9EqgamOA","name":"Fireship"}]}
{"_id":"x","name":"Other","subscriptions":[]}"#;
        assert_eq!(parse_import(freetube).unwrap(), s.channels);
        assert!(parse_import("nothing here").is_err());
    }

    #[test]
    fn add_remove() {
        let mut s = Subs::default();
        let f = Sub {
            id: "UCsBjURrPoezykLs9EqgamOA".into(),
            name: "Fireship".into(),
        };
        assert!(s.add(f.clone()));
        assert!(!s.add(f.clone()));
        assert_eq!(s.remove("fireship"), Some(f.clone()));
        s.add(f.clone());
        assert_eq!(s.remove("UCsBjURrPoezykLs9EqgamOA"), Some(f));
        assert_eq!(s.remove("nope"), None);
    }
}
