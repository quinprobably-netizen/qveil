//! Plain-language versions of the failures people actually hit. The desktop
//! app and the command line both print these, so a message reads the same
//! wherever it surfaces.

use crate::net::PROXY_MARK;

/// Maps a raw error to something readable, or returns it unchanged. The
/// replacements never contain the text they match, so applying this twice
/// (once where yt-dlp fails, once where the error is finally shown) is safe.
pub fn friendly(err: &str) -> String {
    // yt-dlp spells it "Connection refused", other libraries in lower case,
    // so compare without regard to case.
    let refused = err.to_ascii_lowercase().contains("connection refused");
    // Both parts are needed: an error that merely mentions the address (a
    // `socks5://127.0.0.1:9050` the user typed, which is refused for another
    // reason) is not Tor being down. `Net::explain` adds the proxy's address
    // to refusals, because the libraries don't say where they connected.
    if refused && err.contains("127.0.0.1:9050") {
        return "The proxy refused the connection (Tor listens on 127.0.0.1:9050). Start it, or switch the route to Direct."
            .into();
    }
    if refused && let Some((_, rest)) = err.split_once(PROXY_MARK) {
        let addr = rest.split(']').next().unwrap_or_default();
        return format!(
            "The proxy at {addr} refused the connection. Start it, or switch the route to Direct."
        );
    }
    // Only the bot check is about the IP. yt-dlp's words for it are "Sign in
    // to confirm you're not a bot", with a curly apostrophe these days, so
    // this matches the part after it. The broader "Sign in to confirm" would
    // also catch the age check below, which no other route can help with.
    if err.contains("not a bot") {
        return "YouTube is asking this IP to sign in (bot check). Try another route or try later."
            .into();
    }
    // An age-restricted video asks to sign in too ("Sign in to confirm your
    // age"), and qveil never has an account, so it can't be played.
    const AGE_GATE: &[&str] = &["confirm your age", "inappropriate for some users"];
    if AGE_GATE.iter().any(|needle| err.contains(needle)) {
        return "This video is age-restricted; YouTube requires an account to play it.".into();
    }
    // yt-dlp needs one to solve YouTube's challenges, and it fails like
    // an outdated yt-dlp does when there is none, so say what is missing.
    if err.contains("No supported JavaScript runtime") {
        return "yt-dlp needs a JavaScript runtime to read YouTube. Install deno (sudo pacman -S deno)."
            .into();
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
        return "yt-dlp couldn't read YouTube's reply; it's probably out of date. Update it: sudo pacman -Syu yt-dlp"
            .into();
    }
    if err.contains("Requested format is not available") {
        return "yt-dlp found no downloadable format for this video. Update it: sudo pacman -Syu yt-dlp"
            .into();
    }
    err.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
