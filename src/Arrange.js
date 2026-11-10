.pragma library

// Sorting and filtering a list of videos that is already here. Nothing in
// this asks anybody for anything: it only reorders and leaves out.
//
// A setting is { sort, length, watched }:
//   sort     "" (the order the list came in), "newest", "views", "longest", "shortest"
//   length   "any", "short" (under 10 minutes), "medium" (10 to 30), "long" (over 30)
//   watched  "show" or "hide"

function fresh() { return { sort: "", length: "any", watched: "show" } }

function of(setting) {
    const s = setting || {}
    return { sort: s.sort || "", length: s.length || "any", watched: s.watched || "show" }
}

// Whether it leaves the list as it came.
function plain(setting) {
    const s = of(setting)
    return s.sort === "" && s.length === "any" && s.watched === "show"
}

function stamp(v) { return v.date || v.published || 0 }

// `videos` as the setting has them. `watched` is { id: true } for what is
// known to have been watched. A video whose length isn't known has no place
// under a length filter, and comes last when sorted by length.
function apply(videos, setting, watched) {
    const s = of(setting)
    let out = videos
    if (s.length !== "any")
        out = out.filter(v => {
            const d = v.duration || 0
            if (d <= 0)
                return false
            return s.length === "short" ? d < 600 : s.length === "medium" ? (d >= 600 && d <= 1800) : d > 1800
        })
    if (s.watched === "hide")
        out = out.filter(v => !(watched && watched[v.id]))
    if (s.sort !== "") {
        const key = s.sort === "newest" ? (v => -stamp(v))
                  : s.sort === "views" ? (v => -(v.views || 0))
                  : s.sort === "longest" ? (v => v.duration > 0 ? -v.duration : Infinity)
                  : (v => v.duration > 0 ? v.duration : Infinity)
        // (Kept in the order they came where the key is the same.)
        out = out.map((v, i) => [key(v), i, v]).sort((a, b) => a[0] - b[0] || a[1] - b[1]).map(e => e[2])
    }
    return out
}

// The rows of a list model as an array.
function rows(model) {
    const out = []
    if (model)
        for (let i = 0; i < model.count; i++)
            out.push(model.get(i))
    return out
}

// An array as what a player's "up next" reads: something with count and get.
function listed(videos) {
    return { count: videos.length, get: i => videos[i] }
}
