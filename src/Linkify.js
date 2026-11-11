.pragma library

// Descriptions and comments are text that other people wrote. This decides
// what in it can be clicked: a timestamp (seeks), a link to a YouTube video
// or channel (opens here, reduced to what it names), and any other link
// (which can only be copied). Everything else stays words: nothing in the
// text is ever treated as markup.

// A link, or a time that stands as a word of its own: m:ss or h:mm:ss.
const FIND = /(https?:\/\/[^\s<>"']+)|((?:(\d{1,2}):)?(\d{1,2}):(\d{2}))/g
const VIDEO_ID = /^[A-Za-z0-9_-]{11}$/
const CHANNEL_ID = /^UC[A-Za-z0-9_-]{22}$/

function isYouTube(host) {
    return host === "youtu.be" || host === "youtube.com" || host === "www.youtube.com"
        || host === "m.youtube.com" || host === "music.youtube.com"
        // The privacy-enhanced embeds, as the engine reads them too.
        || host === "youtube-nocookie.com" || host === "www.youtube-nocookie.com"
}

// What a link names, if it is YouTube's: { kind: "video" | "channel", id }.
function youtube(url) {
    const m = /^https?:\/\/([^\/?#:]+)(\/[^?#]*)?(?:\?([^#]*))?/.exec(url)
    if (!m || !isYouTube(m[1].toLowerCase()))
        return null
    const path = (m[2] || "/").split("/").filter(p => p !== "")
    if (m[1].toLowerCase() === "youtu.be")
        return VIDEO_ID.test(path[0] || "") ? { kind: "video", id: path[0] } : null
    if (path[0] === "watch") {
        for (const pair of (m[3] || "").split("&")) {
            const eq = pair.indexOf("=")
            if (eq > 0 && pair.slice(0, eq) === "v" && VIDEO_ID.test(pair.slice(eq + 1)))
                return { kind: "video", id: pair.slice(eq + 1) }
        }
        return null
    }
    if ((path[0] === "shorts" || path[0] === "live" || path[0] === "embed") && VIDEO_ID.test(path[1] || ""))
        return { kind: "video", id: path[1] }
    if (path[0] === "channel" && CHANNEL_ID.test(path[1] || ""))
        return { kind: "channel", id: path[1] }
    if (path[0] && path[0].charAt(0) === "@" && /^@[A-Za-z0-9._-]{3,30}$/.test(path[0]))
        return { kind: "channel", id: path[0] }
    return null
}

// The text cut into parts, in order: { kind, text }, with `seconds` for a
// time, `id` for a video or a channel and `url` for any other link.
function parts(text) {
    const out = []
    let from = 0
    const words = s => { if (s !== "") out.push({ kind: "text", text: s }) }
    FIND.lastIndex = 0
    let m
    while ((m = FIND.exec(text)) !== null) {
        let found = m[0]
        let part = null
        if (m[1]) {
            // What ends a sentence isn't part of the address.
            found = found.replace(/[.,;:!?)\]]+$/, "")
            const named = youtube(found)
            part = named ? { kind: named.kind, text: found, id: named.id } : { kind: "link", text: found, url: found }
        } else {
            const before = m.index > 0 ? text.charAt(m.index - 1) : ""
            const after = text.charAt(m.index + found.length)
            const alone = !/[\w:.]/.test(before) && !/[\w:]/.test(after)
            const h = m[3] === undefined ? 0 : parseInt(m[3]), mm = parseInt(m[4]), ss = parseInt(m[5])
            if (alone && ss < 60 && (m[3] === undefined || mm < 60))
                part = { kind: "time", text: found, seconds: h * 3600 + mm * 60 + ss }
        }
        if (part) {
            words(text.slice(from, m.index))
            out.push(part)
            from = m.index + found.length
        }
        FIND.lastIndex = m.index + Math.max(1, found.length)
    }
    words(text.slice(from))
    return out
}

function escape(s) {
    return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;")
            .replace(/\r?\n/g, "<br>")
}

// The same as styled text for a Text item: only the parts above are links,
// and each leads back into the app (see WatchPage.follow).
function html(text, accent) {
    let out = ""
    for (const p of parts(text || "")) {
        const shown = escape(p.text)
        if (p.kind === "time")
            out += '<a href="qveil:time/' + p.seconds + '">' + shown + "</a>"
        else if (p.kind === "video" || p.kind === "channel")
            out += '<a href="qveil:' + p.kind + "/" + encodeURIComponent(p.id) + '">' + shown + "</a>"
        else if (p.kind === "link")
            out += '<a href="qveil:copy/' + encodeURIComponent(p.url) + '">' + shown + "</a>"
        else
            out += shown
    }
    return out
}
