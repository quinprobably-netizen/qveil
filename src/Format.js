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

// What the waiting room says under its countdown, for a stream announced
// for `starts` (Unix seconds, 0 when that isn't known) at `now`. Once it is
// late, how often the engine asks follows the engine's schedule: every
// minute, and every 5 minutes once it is half an hour late.
function waitLine(starts, now) {
    if (!(starts > 0))
        return qsTr("Waiting for it to start · checking every minute")
    if (now < starts)
        return qsTr("%1 · plays from the start when it begins").arg(dayTime(starts))
    const late = Math.max(1, Math.floor((now - starts) / 60))
    const ago = late < 60 ? qsTr("%1 min").arg(late) : qsTr("%1 h %2 min").arg(Math.floor(late / 60)).arg(late % 60)
    return now - starts >= 30 * 60 ? qsTr("Should have started %1 ago · checking every 5 minutes").arg(ago)
                                   : qsTr("Should have started %1 ago · checking every minute").arg(ago)
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

// How long ago a stream started, in the words the cards use for time (not a
// clock, which reads as a length): "14 min", "2 h", "2 h 14 min". At least a
// minute, and it changes once a minute. `brief`: whole hours only, past one.
function ranFor(seconds, brief) {
    const minutes = Math.max(1, Math.floor(Math.max(0, seconds) / 60))
    if (minutes < 60)
        return qsTr("%1 min").arg(minutes)
    const h = Math.floor(minutes / 60), m = minutes % 60
    return m > 0 && !brief ? qsTr("%1 h %2 min").arg(h).arg(m) : qsTr("%1 h").arg(h)
}

// The line under a running stream's title, from the play answer's `live`:
// "live now · 1.2K watching · started 2 h 14 min ago", as a card's begins.
// Where that won't fit, `brief`: 1 says whole hours ("started 2 h ago"), 2
// the count without its word too ("1.2K"). `now`: the time, in Unix seconds.
function liveMeta(live, now, brief) {
    const parts = [qsTr("live now")]
    const n = count(live && live.watching)
    if (n)
        parts.push(brief >= 2 ? n : qsTr("%1 watching").arg(n))
    if (live && live.started > 0)
        parts.push(qsTr("started %1 ago").arg(ranFor(now - live.started, brief >= 1)))
    return parts.join(" · ")
}

// What a video says it is, for the engine's lookup: "now" for a stream that
// is running, "upcoming" for one announced, "" for anything else. `video` is
// a card's entry (`live` is the word), or a play answer (`live` is an object
// with its state, and an announced stream's has `waiting`).
function liveHint(video) {
    const live = video && video.live
    if (live === "now" || live === "upcoming")
        return live
    if (live && live.state === "now")
        return "now"
    return video && video.waiting ? "upcoming" : ""
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
