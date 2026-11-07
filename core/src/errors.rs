//! Plain-language versions of the failures people actually hit. The desktop
//! app and the command line both print these, so a message reads the same
//! wherever it surfaces.

/// Maps a raw error to something readable, or returns it unchanged. The
/// replacements never contain the text they match, so applying this twice
/// (once where yt-dlp fails, once where the error is finally shown) is safe.
pub fn friendly(err: &str) -> String {
    // yt-dlp spells it "SocksHTTPSConnection … Connection refused", other
    // libraries in lower case, so compare without regard to case.
    let lower = err.to_ascii_lowercase();
    if err.contains("127.0.0.1:9050")
        || (lower.contains("socks") && lower.contains("connection refused"))
    {
        return "The proxy refused the connection (Tor listens on 127.0.0.1:9050). Start it, or switch the route to Direct."
            .into();
    }
    if err.contains("Sign in to confirm") {
        return "YouTube is asking this IP to sign in (bot check). Try another route or try later."
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
        "No supported JavaScript runtime",
    ];
    if OUTDATED.iter().any(|needle| err.contains(needle)) {
        return "yt-dlp couldn't read YouTube's reply; it's probably out of date. Try updating yt-dlp."
            .into();
    }
    if err.contains("Requested format is not available") {
        return "yt-dlp found no downloadable format for this video. Try updating yt-dlp.".into();
    }
    err.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_the_common_failures() {
        for raw in [
            "socks connect error: Connection refused",
            "yt-dlp: [youtube] x: Unable to download API page: SocksHTTPSConnection(host='www.youtube.com', port=443): Failed to establish a new connection: [Errno 111] Connection refused",
        ] {
            let msg = friendly(raw);
            assert!(msg.contains("switch the route to Direct"), "{raw} -> {msg}");
            assert!(msg.len() < 120, "{msg}");
        }
        assert!(friendly("ERROR: Sign in to confirm you're not a bot").contains("bot check"));
        for raw in [
            "ERROR: [youtube] abc: Unable to extract yt initial data; please report this issue on https://github.com/yt-dlp/yt-dlp/issues",
            "ERROR: [youtube] abc: nsig extraction failed: Some formats may be missing",
            "ERROR: [youtube] abc: Requested format is not available. Use --list-formats",
        ] {
            let msg = friendly(raw);
            assert!(msg.contains("Try updating yt-dlp"), "{raw} -> {msg}");
            assert!(msg.len() < 100, "messages stay short: {msg}");
        }
        assert_eq!(friendly("HTTP Error 404"), "HTTP Error 404");
    }

    #[test]
    fn mapping_twice_changes_nothing() {
        for raw in [
            "connect to 127.0.0.1:9050 failed",
            "Sign in to confirm you're not a bot",
            "Unable to extract player response",
            "Requested format is not available",
            "something else",
        ] {
            let once = friendly(raw);
            assert_eq!(friendly(&once), once);
        }
    }
}
