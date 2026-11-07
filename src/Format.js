.pragma library

function clock(seconds) {
    if (!(seconds > 0))
        return "0:00"
    const s = Math.floor(seconds)
    const h = Math.floor(s / 3600), m = Math.floor(s / 60) % 60, r = s % 60
    const pad = n => (n < 10 ? "0" : "") + n
    return h > 0 ? h + ":" + pad(m) + ":" + pad(r) : m + ":" + pad(r)
}

// "3 days ago"; estimated dates (RSS was down, or a channel's Videos tab)
// get a leading "~".
function age(timestamp, estimated) {
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
    return (estimated ? "~" : "") + text
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

function meta(video, date, estimated) {
    const parts = []
    const when = age(date, estimated)
    if (when)
        parts.push(when)
    const v = views(video.views)
    if (v)
        parts.push(v)
    return parts.join("  ·  ")
}
