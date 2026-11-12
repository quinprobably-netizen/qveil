//! Plain-language versions of the failures people actually hit. The desktop
//! app and the command line both print these, so a message reads the same
//! wherever it surfaces. The app also gets a code for each, so it can offer
//! what helps (another route, a wait, an update) without parsing the text.

use crate::net::PROXY_MARK;
use serde_json::{Value, json};

/// What kind of failure an error is, as the API reports it in `code`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Code {
    BotCheck,
    AgeRestricted,
    TorDown,
    ProxyRefused,
    RouteChanged,
    Live,
    Private,
    RateLimited,
    YtdlpOutdated,
    DenoMissing,
    MembersOnly,
    Region,
    NoRecording,
    GaveUp,
    Other,
}

impl Code {
    #[cfg(test)]
    pub const ALL: [Code; 15] = [
        Code::BotCheck,
        Code::AgeRestricted,
        Code::TorDown,
        Code::ProxyRefused,
        Code::RouteChanged,
        Code::Live,
        Code::Private,
        Code::RateLimited,
        Code::YtdlpOutdated,
        Code::DenoMissing,
        Code::MembersOnly,
        Code::Region,
        Code::NoRecording,
        Code::GaveUp,
        Code::Other,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Code::BotCheck => "bot_check",
            Code::AgeRestricted => "age_restricted",
            Code::TorDown => "tor_down",
            Code::ProxyRefused => "proxy_refused",
            Code::RouteChanged => "route_changed",
            Code::Live => "live",
            Code::Private => "private",
            Code::RateLimited => "rate_limited",
            Code::YtdlpOutdated => "ytdlp_outdated",
            Code::DenoMissing => "deno_missing",
            Code::MembersOnly => "members_only",
            Code::Region => "region",
            Code::NoRecording => "no_recording",
            Code::GaveUp => "gave_up",
            Code::Other => "other",
        }
    }
}

/// An error in words a person can use, and its kind.
#[derive(Debug, PartialEq)]
pub struct Explained {
    pub text: String,
    pub code: Code,
}

impl Explained {
    fn new(code: Code, text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            code,
        }
    }
}

/// The engine's own words for an announced stream that never started: the
/// waiting room stops looking 6 hours after the time it was announced for.
pub const GAVE_UP: &str =
    "The stream hasn't started, and qveil stopped waiting for it after 6 hours.";

/// Whether `err` says the video is gone: removed by its uploader or by
/// YouTube, or unavailable, in yt-dlp's words.
pub fn unavailable(err: &str) -> bool {
    const GONE: &[&str] = &[
        "Video unavailable",
        "This video is unavailable",
        "This video has been removed",
        "This video is no longer available",
    ];
    GONE.iter().any(|needle| err.contains(needle))
}

/// Whether asking about the video again can't change the answer `err` is:
/// it is private, members-only, age-restricted, not offered in this
/// route's region, gone, or a stream that ended without a recording. A bot
/// check, a limit on how fast, a stalled circuit or a timeout may well be
/// different the next time.
pub fn for_good(err: &str) -> bool {
    matches!(
        explain(err).code,
        Code::Private | Code::MembersOnly | Code::Region | Code::AgeRestricted | Code::NoRecording
    ) || unavailable(err)
}

/// The body of an error reply: `{"error": <text>, "code": <code>}`.
pub fn body(err: &str) -> Value {
    let e = explain(err);
    json!({ "error": e.text, "code": e.code.as_str() })
}

/// Maps a raw error to something readable, or returns it unchanged.
pub fn friendly(err: &str) -> String {
    explain(err).text
}

/// Like `friendly`, with the kind of failure. The replacements never contain
/// the text they match, so applying this twice (once where yt-dlp fails, once
/// where the error is finally shown) is safe, and an error that has been
/// mapped already is still recognised by its replacement's own words (see
/// `already_explained`), so the code survives the second pass too.
pub fn explain(err: &str) -> Explained {
    // yt-dlp spells it "Connection refused", other libraries in lower case,
    // so compare without regard to case.
    let refused = err.to_ascii_lowercase().contains("connection refused");
    // Both parts are needed: an error that merely mentions the address (a
    // `socks5://127.0.0.1:9050` the user typed, which is refused for another
    // reason) is not Tor being down. `Net::explain` adds the proxy's address
    // to refusals, because the libraries don't say where they connected.
    if refused && err.contains("127.0.0.1:9050") {
        return Explained::new(
            Code::TorDown,
            "The proxy refused the connection (Tor listens on 127.0.0.1:9050). Start it, or switch the route to Direct.",
        );
    }
    if refused && let Some((_, rest)) = err.split_once(PROXY_MARK) {
        let addr = rest.split(']').next().unwrap_or_default();
        return Explained::new(
            Code::ProxyRefused,
            format!(
                "The proxy at {addr} refused the connection. Start it, or switch the route to Direct."
            ),
        );
    }
    // Only the bot check is about the IP. yt-dlp's words for it are "Sign in
    // to confirm you're not a bot", with a curly apostrophe these days, so
    // this matches the part after it. The broader "Sign in to confirm" would
    // also catch the age check below, which no other route can help with.
    if err.contains("not a bot") {
        return Explained::new(
            Code::BotCheck,
            "YouTube is asking this IP to sign in (bot check). Try another route or try later.",
        );
    }
    // An age-restricted video asks to sign in too ("Sign in to confirm your
    // age"), and qveil never has an account, so it can't be played.
    const AGE_GATE: &[&str] = &["confirm your age", "inappropriate for some users"];
    if AGE_GATE.iter().any(|needle| err.contains(needle)) {
        return Explained::new(
            Code::AgeRestricted,
            "This video is age-restricted; YouTube requires an account to play it.",
        );
    }
    // yt-dlp says "Private video. Sign in if you've been granted access", and
    // without an account there is no access to be granted.
    if err.contains("Private video") {
        return Explained::new(
            Code::Private,
            "This video is private; only the people it was shared with can watch it.",
        );
    }
    // A channel's members-only video or stream. yt-dlp says "Join this
    // channel to get access to members-only content", or names the level of
    // membership it is for. qveil never has an account.
    const MEMBERS: &[&str] = &["members-only content", "channel's members on level"];
    if MEMBERS.iter().any(|needle| err.contains(needle)) {
        return Explained::new(
            Code::MembersOnly,
            "This video is for the channel's members only; qveil never signs in, so it can't be played.",
        );
    }
    // "The uploader has not made this video available in your country": the
    // country is the route's, so another route may have it.
    if err.contains("available in your country") {
        return Explained::new(
            Code::Region,
            "YouTube doesn't offer this video in this route's region. Try another route.",
        );
    }
    // A stream that has ended, and that YouTube kept nothing of (or keeps
    // only as a manifestless post-live stream, which yt-dlp can't fetch).
    const NO_RECORDING: &[&str] = &[
        "This live stream recording is not available",
        "Post-Live Manifestless",
    ];
    if NO_RECORDING.iter().any(|needle| err.contains(needle)) {
        return Explained::new(
            Code::NoRecording,
            "The stream has ended, and YouTube kept no recording of it.",
        );
    }
    // HTTP 429, as yt-dlp (`HTTP Error 429: Too Many Requests`) and reqwest
    // (`429 Too Many Requests`) spell it, and yt-dlp's own words for a
    // session that YouTube has throttled.
    const THROTTLED: &[&str] = &[
        "HTTP Error 429",
        "429 Too Many Requests",
        "Too Many Requests",
        "rate-limited by YouTube",
    ];
    if THROTTLED.iter().any(|needle| err.contains(needle)) {
        return Explained::new(
            Code::RateLimited,
            "YouTube is limiting how fast this address may ask. Wait a while, or try another route.",
        );
    }
    // A stream or a premiere that hasn't started: there is nothing to play
    // yet. A play answers one with when it starts (`waiting`), so this is
    // only what is left when that couldn't be found out.
    const NOT_YET: &[&str] = &["live event will begin", "Premieres in"];
    if NOT_YET.iter().any(|needle| err.contains(needle)) {
        return Explained::new(
            Code::Live,
            "This is a live stream or premiere that hasn't started yet.",
        );
    }
    // yt-dlp needs one to solve YouTube's challenges, and it fails like
    // an outdated yt-dlp does when there is none, so say what is missing.
    if err.contains("No supported JavaScript runtime") {
        return Explained::new(
            Code::DenoMissing,
            "yt-dlp needs a JavaScript runtime to read YouTube. Install deno (sudo pacman -S deno).",
        );
    }
    // yt-dlp breaks whenever YouTube changes something, and the fix is almost
    // always a newer yt-dlp. These are the shapes that breakage takes.
    const OUTDATED: &[&str] = &[
        "Unable to extract",
        "please report this issue",
        "nsig extraction failed",
        "Signature extraction failed",
        "challenge solving failed",
    ];
    if OUTDATED.iter().any(|needle| err.contains(needle)) {
        return Explained::new(
            Code::YtdlpOutdated,
            "yt-dlp couldn't read YouTube's reply; it's probably out of date. Update it: sudo pacman -Syu yt-dlp",
        );
    }
    if err.contains("Requested format is not available") {
        return Explained::new(
            Code::YtdlpOutdated,
            "yt-dlp found no downloadable format for this video. Update it: sudo pacman -Syu yt-dlp",
        );
    }
    Explained::new(already_explained(err), err)
}

/// The kind of an error that `explain` has mapped already, or that the engine
/// worded itself, recognised by the words of its message. The unmapped rest
/// is `Other`.
fn already_explained(err: &str) -> Code {
    // The route changing is the engine's own message (`inflight::ROUTE_CHANGED`
    // and its variants), never mapped from anything.
    const KNOWN: &[(&str, Code)] = &[
        ("YouTube is asking this IP to sign in", Code::BotCheck),
        ("(Tor listens on 127.0.0.1:9050)", Code::TorDown),
        (
            "refused the connection. Start it, or switch",
            Code::ProxyRefused,
        ),
        ("This video is age-restricted", Code::AgeRestricted),
        ("This video is private", Code::Private),
        ("YouTube is limiting how fast", Code::RateLimited),
        ("for the channel's members only", Code::MembersOnly),
        ("in this route's region", Code::Region),
        ("YouTube kept no recording", Code::NoRecording),
        ("qveil stopped waiting for it", Code::GaveUp),
        (crate::live::NO_FRAGMENTS, Code::Live),
        (crate::ytdlp::NOT_PLAIN, Code::Live),
        ("a live stream or premiere that hasn't started", Code::Live),
        ("yt-dlp needs a JavaScript runtime", Code::DenoMissing),
        ("yt-dlp couldn't read YouTube's reply", Code::YtdlpOutdated),
        ("yt-dlp found no downloadable format", Code::YtdlpOutdated),
        ("The route changed", Code::RouteChanged),
    ];
    KNOWN
        .iter()
        .find(|(words, _)| err.contains(words))
        .map_or(Code::Other, |(_, code)| *code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_answers_that_can_not_change_are_for_good() {
        for err in [
            "ERROR: [youtube] x: Private video. Sign in if you've been granted access",
            "ERROR: [youtube] x: Join this channel to get access to members-only content",
            "ERROR: [youtube] x: The uploader has not made this video available in your country",
            "ERROR: [youtube] x: Sign in to confirm your age",
            "ERROR: [youtube] x: Video unavailable. This video has been removed by the uploader",
            "ERROR: [youtube] x: This video is no longer available because the account was terminated",
            "ERROR: [youtube] x: This live stream recording is not available.",
            // And once already explained.
            "This video is private; only the people it was shared with can watch it.",
        ] {
            assert!(for_good(err), "{err}");
        }
        for err in [
            "ERROR: [youtube] x: Sign in to confirm you're not a bot",
            "ERROR: [youtube] x: HTTP Error 429: Too Many Requests",
            "yt-dlp timed out after 90 s",
            "The proxy refused the connection (Tor listens on 127.0.0.1:9050).",
            crate::live::NO_FRAGMENTS,
            "ERROR: [youtube] x: This live event will begin in 2 hours.",
        ] {
            assert!(!for_good(err), "{err}");
        }
    }

    #[test]
    fn tor_is_only_blamed_for_a_refusal_at_its_own_address() {
        for raw in [
            "socks connect error: Connection refused (os error 111) [proxy 127.0.0.1:9050]",
            "yt-dlp: [youtube] x: Unable to download API page: SocksHTTPSConnection(host='www.youtube.com', port=443): Failed to establish a new connection: [Errno 111] Connection refused [proxy 127.0.0.1:9050]",
            "connect to 127.0.0.1:9050 failed: connection refused",
        ] {
            let msg = friendly(raw);
            assert!(msg.contains("switch the route to Direct"), "{raw} -> {msg}");
            assert!(msg.contains("Tor listens"), "{raw} -> {msg}");
            assert!(msg.len() < 120, "{msg}");
        }
        // Mentioning the address isn't enough: the connection has to have been refused.
        for raw in [
            "connect to 127.0.0.1:9050 timed out",
            "socks5h://127.0.0.1:9050 replied: general failure",
        ] {
            assert_eq!(friendly(raw), raw);
        }
        // And a refusal elsewhere isn't Tor's doing.
        assert_eq!(
            friendly("socks connect error: Connection refused (os error 111)"),
            "socks connect error: Connection refused (os error 111)"
        );
        let msg = friendly("Connection refused (os error 111) [proxy 10.0.0.1:3128]");
        assert!(
            msg.contains("10.0.0.1:3128") && !msg.contains("Tor"),
            "{msg}"
        );
    }

    #[test]
    fn a_validation_error_comes_through_unchanged() {
        // This one names 127.0.0.1:9050 too, and is about DNS, not a refusal.
        let err = crate::net::Net::new(Some("socks5://127.0.0.1:9050".into()), true)
            .unwrap_err()
            .to_string();
        assert!(err.contains("127.0.0.1:9050") && err.contains("socks5h://"));
        assert_eq!(friendly(&err), err);
        let err = crate::net::Net::new(Some(crate::net::TOR_PROXY.into()), false)
            .unwrap_err()
            .to_string();
        assert_eq!(friendly(&err), err);
    }

    #[test]
    fn the_bot_check_and_the_age_check_are_told_apart() {
        // Both of yt-dlp's apostrophes.
        for raw in [
            "ERROR: [youtube] abc: Sign in to confirm you're not a bot. Use --cookies-from-browser or --cookies for the authentication.",
            "ERROR: [youtube] abc: Sign in to confirm you\u{2019}re not a bot. Use --cookies-from-browser or --cookies for the authentication.",
            "yt-dlp: Sign in to confirm you're not a bot",
        ] {
            let msg = friendly(raw);
            assert!(msg.contains("bot check"), "{raw} -> {msg}");
            assert!(!msg.contains("age"), "{msg}");
        }
        for raw in [
            "ERROR: [youtube] abc: Sign in to confirm your age. This video may be inappropriate for some users.",
            "yt-dlp: Sign in to confirm your age",
            "This video may be inappropriate for some users.",
        ] {
            assert_eq!(
                friendly(raw),
                "This video is age-restricted; YouTube requires an account to play it.",
                "{raw}"
            );
        }
        // Any other sign-in request is neither, and is left as it is.
        assert_eq!(
            friendly("Sign in to confirm something else"),
            "Sign in to confirm something else"
        );
    }

    #[test]
    fn maps_the_common_failures() {
        assert!(friendly("ERROR: Sign in to confirm you're not a bot").contains("bot check"));
        for raw in [
            "ERROR: [youtube] abc: Unable to extract yt initial data; please report this issue on https://github.com/yt-dlp/yt-dlp/issues",
            "ERROR: [youtube] abc: nsig extraction failed: Some formats may be missing",
            "ERROR: [youtube] abc: Requested format is not available. Use --list-formats",
        ] {
            let msg = friendly(raw);
            assert!(msg.contains("sudo pacman -Syu yt-dlp"), "{raw} -> {msg}");
            assert!(msg.len() < 120, "messages stay short: {msg}");
        }
        assert_eq!(friendly("HTTP Error 404"), "HTTP Error 404");
    }

    #[test]
    fn a_missing_javascript_runtime_is_not_blamed_on_yt_dlp() {
        let msg = friendly("WARNING: [youtube] No supported JavaScript runtime could be found.");
        assert!(msg.contains("Install deno (sudo pacman -S deno)"), "{msg}");
        assert!(!msg.contains("out of date"), "{msg}");
    }

    #[test]
    fn mapping_twice_changes_nothing() {
        for raw in [
            "connect to 127.0.0.1:9050 failed: connection refused",
            "Connection refused [proxy 10.0.0.1:3128]",
            "Sign in to confirm you're not a bot",
            "Sign in to confirm your age",
            "Unable to extract player response",
            "No supported JavaScript runtime could be found",
            "Requested format is not available",
            "something else",
        ] {
            let once = friendly(raw);
            assert_eq!(friendly(&once), once);
        }
    }

    /// What the window is told to expect (the API's contract).
    #[test]
    fn the_codes_are_the_ones_the_window_knows() {
        let names: Vec<_> = Code::ALL.iter().map(|c| c.as_str()).collect();
        assert_eq!(
            names,
            [
                "bot_check",
                "age_restricted",
                "tor_down",
                "proxy_refused",
                "route_changed",
                "live",
                "private",
                "rate_limited",
                "ytdlp_outdated",
                "deno_missing",
                "members_only",
                "region",
                "no_recording",
                "gave_up",
                "other"
            ]
        );
    }

    #[test]
    fn every_failure_is_worded_and_has_its_code() {
        for (raw, code) in [
            (
                "ERROR: [youtube] abc: Sign in to confirm you\u{2019}re not a bot.",
                Code::BotCheck,
            ),
            (
                "ERROR: [youtube] abc: Sign in to confirm your age.",
                Code::AgeRestricted,
            ),
            (
                "socks connect error: Connection refused (os error 111) [proxy 127.0.0.1:9050]",
                Code::TorDown,
            ),
            (
                "Connection refused (os error 111) [proxy 10.0.0.1:3128]",
                Code::ProxyRefused,
            ),
            (
                "ERROR: [youtube] abc: Private video. Sign in if you've been granted access to this video",
                Code::Private,
            ),
            (
                "ERROR: [youtube] abc: Unable to download webpage: HTTP Error 429: Too Many Requests (caused by <HTTPError 429: Too Many Requests>)",
                Code::RateLimited,
            ),
            (
                "HTTP status client error (429 Too Many Requests) for url (https://i.ytimg.com/vi/x/mqdefault.jpg)",
                Code::RateLimited,
            ),
            (
                "ERROR: [youtube] abc: This content isn't available, try again later. The current session has been rate-limited by YouTube for up to an hour.",
                Code::RateLimited,
            ),
            (
                "ERROR: [youtube] abc: This live event will begin in 2 hours.",
                Code::Live,
            ),
            (
                "WARNING: [youtube] No supported JavaScript runtime could be found.",
                Code::DenoMissing,
            ),
            (
                "ERROR: [youtube] abc: nsig extraction failed: Some formats may be missing",
                Code::YtdlpOutdated,
            ),
            (
                "ERROR: [youtube] abc: Requested format is not available.",
                Code::YtdlpOutdated,
            ),
            ("The route changed; try again", Code::RouteChanged),
            (
                "The route changed while opening the video; try again",
                Code::RouteChanged,
            ),
            (crate::live::NO_FRAGMENTS, Code::Live),
            (crate::ytdlp::NOT_PLAIN, Code::Live),
            (
                "ERROR: [youtube] abc: Join this channel to get access to members-only content like this video.",
                Code::MembersOnly,
            ),
            (
                "ERROR: [youtube] abc: This video is available to this channel's members on level: Fan",
                Code::MembersOnly,
            ),
            (
                "ERROR: [youtube] abc: The uploader has not made this video available in your country",
                Code::Region,
            ),
            (
                "ERROR: [youtube] abc: This live stream recording is not available.",
                Code::NoRecording,
            ),
            (GAVE_UP, Code::GaveUp),
            ("HTTP Error 404", Code::Other),
            ("something nobody foresaw", Code::Other),
        ] {
            let said = explain(raw);
            assert_eq!(said.code, code, "{raw} -> {said:?}");
            // The reader never sees yt-dlp's own words for a failure that has a code.
            // (Those are the engine's own words.)
            if !matches!(
                code,
                Code::Other | Code::RouteChanged | Code::Live | Code::GaveUp
            ) {
                assert_ne!(said.text, raw);
            }
            // Mapping what was mapped changes neither the words nor the code.
            assert_eq!(explain(&said.text), said, "{raw}");
            // Nor does the context that an error picks up on its way (`{:#}`).
            assert_eq!(
                explain(&format!("RSS failed ({}), fallback failed too", said.text)).code,
                code,
                "{raw}"
            );
        }
    }

    #[test]
    fn no_recording_is_explained() {
        for raw in [
            "ERROR: [youtube] abc: This live stream recording is not available.",
            "ERROR: [youtube] abc: Post-Live Manifestless mode is not supported",
        ] {
            let said = explain(raw);
            assert_eq!(said.code, Code::NoRecording, "{raw}");
            assert!(said.text.contains("no recording"), "{}", said.text);
            assert_eq!(explain(&said.text), said);
        }
    }

    #[test]
    fn members_only_is_explained() {
        let raw = "ERROR: [youtube] abc: Join this channel to get access to members-only content like this video, and other exclusive perks.";
        let said = explain(raw);
        assert_eq!(said.code, Code::MembersOnly);
        assert!(said.text.contains("members"), "{}", said.text);
        assert!(!said.text.contains("Join"), "{}", said.text);
        assert_eq!(explain(&said.text), said);
    }

    #[test]
    fn region_is_explained() {
        let raw =
            "ERROR: [youtube] abc: The uploader has not made this video available in your country";
        let said = explain(raw);
        assert_eq!(said.code, Code::Region);
        assert!(
            said.text.contains("region") && said.text.contains("another route"),
            "{}",
            said.text
        );
        assert_eq!(explain(&said.text), said);
    }

    #[test]
    fn giving_up_on_a_stream_that_never_started_has_its_code() {
        assert_eq!(explain(GAVE_UP).code, Code::GaveUp);
        assert!(GAVE_UP.contains("6 hours"), "{GAVE_UP}");
    }

    #[test]
    fn rate_limits_and_private_videos_say_what_to_do() {
        let limited = friendly("HTTP Error 429: Too Many Requests");
        assert!(
            limited.contains("Wait") && limited.contains("another route"),
            "{limited}"
        );
        let private =
            friendly("ERROR: [youtube] x: Private video. Sign in if you've been granted access");
        assert!(
            private.contains("private") && !private.contains("Sign in"),
            "{private}"
        );
    }

    #[test]
    fn an_error_reply_carries_the_text_and_the_code() {
        assert_eq!(
            body("ERROR: Sign in to confirm you're not a bot"),
            json!({
                "error": "YouTube is asking this IP to sign in (bot check). Try another route or try later.",
                "code": "bot_check"
            })
        );
        assert_eq!(body("nope"), json!({ "error": "nope", "code": "other" }));
    }
}
