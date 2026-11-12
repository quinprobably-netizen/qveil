.pragma library

function clock(seconds) {
    if (!(seconds > 0))
        return "0:00"
    const s = Math.floor(seconds)
    const h = Math.floor(s / 3600), m = Math.floor(s / 60) % 60, r = s % 60
    const pad = n => (n < 10 ? "0" : "") + n
    return h > 0 ? h + ":" + pad(m) + ":" + pad(r) : m + ":" + pad(r)
}

// How wide a row of tabs may let each be, so that they all fit in `room`: the
// short ones keep the width they need, and the long ones share what is left,
// all cut to one width, though never to less than `least`. (Infinity when they
// all fit.) `wanted` is the width each needs.
function tabLimit(wanted, room, least) {
    if (wanted.reduce((sum, w) => sum + w, 0) <= room)
        return Infinity
    const sorted = wanted.slice().sort((a, b) => a - b)
    let left = room
    for (let i = 0; i < sorted.length; i++) {
        const share = left / (sorted.length - i)
        if (sorted[i] > share)
            return Math.max(least, Math.floor(share))
        left -= sorted[i]
    }
    return Infinity
}

// "3 days ago". Whether a date is only approximate is said where it is shown
// (a tooltip), not in the text: a "~" in front reads as noise.
function age(timestamp) {
    if (!timestamp)
        return ""
    const d = Math.max(0, Date.now() / 1000 - timestamp)
    let text
    if (d < 3600)
        text = qsTr("%n min ago", "", Math.max(1, Math.floor(d / 60)))
    else if (d < 86400)
        text = qsTr("%n hours ago", "", Math.floor(d / 3600))
    else if (d < 86400 * 60)
        text = qsTr("%n days ago", "", Math.floor(d / 86400))
    else if (d < 86400 * 730)
        text = qsTr("%n months ago", "", Math.floor(d / (86400 * 30)))
    else
        text = qsTr("%n years ago", "", Math.floor(d / (86400 * 365)))
    return text
}

function views(n) {
    if (n === undefined || n === null)
        return ""
    const short = (v, digits) => v.toLocaleString(Qt.locale(), 'f', digits)
    if (n >= 1e9)
        return qsTr("%1B views").arg(short(n / 1e9, 1))
    if (n >= 1e6)
        return qsTr("%1M views").arg(short(n / 1e6, 1))
    if (n >= 1e3)
        return qsTr("%1K views").arg(short(n / 1e3, 0))
    return qsTr("%1 views").arg(n)
}

// How long until a stream's start (unix seconds): "in 5 min", "in 2 h",
// "in 3 days". Empty once that time has come.
function startsIn(timestamp) {
    const d = (timestamp || 0) - Date.now() / 1000
    if (!timestamp || d <= 0)
        return ""
    const minutes = Math.max(1, Math.round(d / 60))
    if (minutes < 60)
        return qsTr("in %1 min").arg(minutes)
    const hours = Math.max(1, Math.round(d / 3600))
    if (d < 48 * 3600 && hours < 48)
        return qsTr("in %1 h").arg(hours)
    return qsTr("in %n days", "", Math.round(d / 86400))
}

// A stream's start as its day and time, in local time: "Fri 18:00".
function dayTime(timestamp) {
    if (!timestamp)
        return ""
    return Qt.formatDateTime(new Date(timestamp * 1000), "ddd HH:mm")
}

// The line under a card's title. `brief`: the count without the word, for
// where the word won't fit.
function meta(video, date, brief) {
    const parts = []
    if (video.live === "now") {
        parts.push(qsTr("live now"))
        const n = count(video.watching)
        if (n)
            parts.push(brief ? n : qsTr("%1 watching").arg(n))
        return parts.join(" · ")
    }
    if (video.live === "upcoming") {
        const rel = startsIn(video.starts)
        parts.push(rel ? qsTr("Starts %1").arg(rel) : video.starts ? qsTr("Starting soon") : qsTr("Upcoming"))
        const at = dayTime(video.starts)
        if (at)
            parts.push(at)
        return parts.join(" · ")
    }
    const when = age(date)
    if (when)
        parts.push(video.streamed ? qsTr("Streamed %1").arg(when) : when)
    const v = brief ? views(video.views).replace(/\s*\S+$/, "") : views(video.views)
    if (v)
        parts.push(v)
    return parts.join(" · ")
}

// The line under a running stream's title, from the play answer's `live`:
// "1.2K watching · started 2:14:05 ago". `now`: the time, in Unix seconds.
function liveMeta(live, now) {
    const parts = []
    const n = count(live && live.watching)
    if (n)
        parts.push(qsTr("%1 watching").arg(n))
    if (live && live.started > 0)
        parts.push(qsTr("started %1 ago").arg(clock(Math.max(0, now - live.started))))
    return parts.join(" · ")
}

// A thumbnail's address: `base` is the engine's thumbnail base (empty while
// thumbnails are off or unavailable), `batch` the page the card came with.
// The engine puts the thumbnails of one batch on one Tor circuit, as a
// browser's page load would; without one, a thumbnail has a circuit of its own.
function thumb(base, id, batch) {
    if (!base || !id)
        return ""
    return base + id + (batch > 0 ? "?batch=" + batch : "")
}

// A count in few characters: 640, 1.2K, 3.4M.
function count(n) {
    if (n === undefined || n === null)
        return ""
    const short = (v, digits) => v.toLocaleString(Qt.locale(), 'f', digits)
    if (n >= 1e6)
        return short(n / 1e6, 1) + "M"
    if (n >= 1e3)
        return short(n / 1e3, n >= 1e4 ? 0 : 1) + "K"
    return String(n)
}
