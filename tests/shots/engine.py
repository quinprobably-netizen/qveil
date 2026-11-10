#!/usr/bin/env python3
"""A stand-in for qveil-core, for the window's tests and its stills.

It speaks the engine's loopback API from memory: a fixed feed, search
results, a channel, lenses, comments, resume positions and settings, none of
which comes from anywhere but this file. Nothing here touches the network.

`GET api/_log` lists the requests made so far, so a test can tell what the
window asked for (and what it didn't). With QVEIL_STANDIN_CLIP set to a media
file, a played video's stream is that file; without it a played video has no
stream, so nothing is decoded and nothing is heard.
"""
import http.server
import json
import os
import struct
import sys
import time
import urllib.parse
import zlib

NOW = int(time.time())
HOUR, DAY = 3600, 86400
CLIP = os.environ.get("QVEIL_STANDIN_CLIP", "")

CHANNELS = [
    ("UCaaaaaaaaaaaaaaaaaaaaaa", "Low Level Hours"),
    ("UCbbbbbbbbbbbbbbbbbbbbbb", "Northlight Woodworks"),
    ("UCcccccccccccccccccccccc", "Signal & Noise"),
    ("UCdddddddddddddddddddddd", "Kiln Notes"),
    ("UCeeeeeeeeeeeeeeeeeeeeee", "The Long Ride"),
]


def video(n, title, channel, seconds, age, views):
    cid, name = CHANNELS[channel]
    return {"id": "vid%08d" % n, "title": title, "channel": name, "channel_id": cid,
            "published": NOW - age, "duration": seconds, "views": views}


FEED = [
    video(1, "Why the borrow checker says no", 0, 2463, 10 * HOUR, 59000),
    video(2, "A dovetail box with four hand tools", 1, 1697, 17 * HOUR, 212000),
    video(3, "The 1970s synth that refuses to stay in tune", 2, 1184, DAY, 88000),
    video(4, "Glaze tests: 36 tiles, one kiln load", 3, 950, 2 * DAY, 31000),
    video(5, "Crossing the Pamir on a steel bike, part 3", 4, 3730, 2 * DAY, 140000),
    video(6, "Allocators, from scratch", 0, 5973, 3 * DAY, 103000),
    video(7, "Sharpening a plane iron properly", 1, 725, 3 * DAY, 77000),
    video(8, "Tape loops without a tape machine", 2, 572, 4 * DAY, 45000),
    video(9, "Raku firing in the rain", 3, 1361, 5 * DAY, 12000),
    video(10, "What a linker actually does", 0, 2000, 6 * DAY, 64000),
    video(11, "The cheapest bench that works", 1, 2832, 6 * DAY, 91000),
    video(12, "A packing list for six months", 4, 1089, 8 * DAY, 23000),
]

DESCRIPTION = (
    "The error everyone hits in their first month of Rust, taken apart slowly. We draw "
    "ownership as one picture, then use it to explain reborrowing, why lifetimes are regions "
    "of code and not durations, and what two-phase borrows quietly fix for you.\n\n"
    "Code and slides: https://example.com/borrow-checker\n"
    "The talk this builds on: https://youtu.be/dQw4w9WgXcQ\n\n"
    "0:00 The error everyone hits\n4:12 Ownership in one picture\n12:40 Reborrowing\n"
    "18:20 Lifetimes are regions\n26:05 Two-phase borrows\n33:48 When to reach for Rc\n39:10 Recap"
)
CHAPTERS = [(0, "The error everyone hits"), (252, "Ownership in one picture"), (760, "Reborrowing"),
            (1100, "Lifetimes are regions"), (1565, "Two-phase borrows"), (2028, "When to reach for Rc"),
            (2350, "Recap")]

COMMENTS = [
    {"id": "c1", "author": "Low Level Hours", "text": "Correction: at 18:20 the lifetime on screen should be 'a, not 'b. The code in the description is right.",
     "likes": 640, "timeText": "9 hours ago", "pinned": True, "byUploader": True, "replies": []},
    {"id": "c2", "author": "mara_k", "text": "The diagram at 12:40 finally made reborrowing click for me.",
     "likes": 1200, "timeText": "6 hours ago", "pinned": False, "byUploader": False,
     "replies": [{"id": "c2.1", "author": "jun.h", "text": "Same. I paused there for a while.", "likes": 14,
                  "timeText": "5 hours ago", "pinned": False, "byUploader": False, "replies": []}]},
    {"id": "c3", "author": "oskar.builds", "text": "Watched this twice. The part on two-phase borrows deserves its own video.",
     "likes": 312, "timeText": "4 hours ago", "pinned": False, "byUploader": False, "replies": []},
    {"id": "c4", "author": "tinfoil_tess", "text": "41 minutes and not one sponsor read. Thank you.",
     "likes": 208, "timeText": "3 hours ago", "pinned": False, "byUploader": False, "replies": []},
    {"id": "c5", "author": "perennial_nico", "text": "The recap at 39:10 is the best five minutes on lifetimes I know of. Slides: https://example.com/slides",
     "likes": 41, "timeText": "1 hour ago", "pinned": False, "byUploader": False, "replies": []},
]

PRIVACY = {
    "summary": "Direct: YouTube sees your IP address.",
    # As long as the engine's own, so that what holds them is tried at its real size.
    "points": ["YouTube sees your IP address with every request, which links them all.",
               "qveil's requests carry a browser User-Agent on a TLS handshake that no browser makes, which is probably rare, so a careful observer can link them by that fingerprint, even across circuits.",
               "Refreshing your subscriptions asks for each channel on its own, in random order, skipping channels fetched in the last 15 minutes: that is still a burst of requests for exactly your channels, so timing and the set of IDs can link them.",
               "You are never signed in, and cookies are never stored or read from a browser: yt-dlp sends the one consent cookie it sets for itself, and sends back what YouTube sets during a single call, kept in memory until that call ends.",
               "Comments are fetched only when you open them, as one more lookup of that video; commenters' pictures are never fetched.",
               "A lens is refreshed as one search per topic (a second one for a topic whose filtered search found nothing): on one address their timing links them, and even over Tor a set of related searches can be linked by content.",
               "Subscriptions, lenses, history, resume positions and the caches stay on this computer, in files only you can read, and qveil has no telemetry."],
    "routes": {"direct": "YouTube sees your IP address.",
               "tor": "Fresh SOCKS credentials per request, which Tor puts on separate circuits.",
               "proxy": "A proxy you trust, such as a VPN's: YouTube sees its address, not yours."},
    "pages": {"search": "Straight from YouTube's search, without an account.",
              "subscriptions": "Kept only on this computer.",
              "history_off": "Off: qveil keeps no history.",
              "history_on": "On: kept only on this computer, until you clear it.",
              "topics": "Off: qveil doesn't work out what the channels you follow are about, and searches for nothing by itself.",
              "resume": "On: qveil remembers where you stopped in an unfinished video for 7 days.",
              "comments": "Fetched only when you open them, as one more lookup of that video.",
              "lenses": "A refresh sends one search per topic, each a request of its own. YouTube sees those searches, and a length, date or order rule that it has a filter for is sent with the search as YouTube's own filter; a topic whose filtered search finds nothing is searched a second time without it. Your wording, the other rules and the ranking stay on this computer."},
}

state = {
    "config": {"route": "direct", "proxy": None, "describe": "direct", "isolated": False, "torRunning": True,
               "history": False, "thumbnails": True, "maxHeight": 1080, "resumeDays": 7,
               "topicFeeds": bool(os.environ.get("QVEIL_STANDIN_TOPICS")),
               "configPath": "/dev/null", "privacy": PRIVACY},
    "subs": [{"id": c, "name": n} for c, n in CHANNELS],
    "resume": [
        {"id": "vid00000001", "pos": 845, "dur": 2463, "at": NOW - HOUR, "title": FEED[0]["title"],
         "channel": FEED[0]["channel"], "expires": NOW - HOUR + 7 * DAY},
        {"id": "vid00000005", "pos": 830, "dur": 3730, "at": NOW - DAY, "title": FEED[4]["title"],
         "channel": FEED[4]["channel"], "expires": NOW + 6 * DAY},
        {"id": "vid00000004", "pos": 760, "dur": 950, "at": NOW - 2 * DAY, "title": FEED[3]["title"],
         "channel": FEED[3]["channel"], "expires": NOW + 5 * DAY},
    ],
    "lenses": [
        {"id": "aaaa0001", "text": "slow woodworking with hand tools, no music", "created": NOW - 30 * DAY, "fetchedAt": NOW - 2 * HOUR},
        {"id": "aaaa0002", "text": "systems talks over 30 min, this year", "created": NOW - 20 * DAY, "fetchedAt": NOW - 2 * HOUR},
    ],
    "log": [],
    "next": 3,
}
# Topic feeds: two groups of the channels above, and what a search found
# for each on other channels. `fetched` says whether they were searched for.
TOPICS_OFF = "Off: qveil doesn't work out what the channels you follow are about, and searches for nothing by itself."
TOPICS_ON = ("On: qveil groups the channels you follow by the words that keep coming back in their titles, on this "
             "computer, and searches YouTube by itself for up to three phrases for each of up to four groups.")
state["topics"] = {"fetched": False, "hidden": []}


def topic_groups():
    def found(prefix, phrase, names):
        return [{"id": "%s%07d" % (prefix, i), "title": "%s, number %d" % (phrase, i + 1), "channel": names[i % len(names)],
                 "channel_id": "UCzzzzzzzzzzzzzzzzzzzzz%d" % (i % len(names)), "published": NOW - (i + 1) * DAY,
                 "duration": 900 + 300 * i, "views": 1000 * (i + 1), "why": [phrase]} for i in range(8)]
    groups = [
        {"id": "aa11bb22cc33", "label": "hand plane, dovetail", "channels": ["Northlight Woodworks", "Kiln Notes"],
         "searches": ["hand plane", "dovetail"], "videos": found("tpw", "hand plane", ["Quiet Bench", "Old Iron", "Shavings"])},
        {"id": "dd44ee55ff66", "label": "borrow checker, allocators", "channels": ["Low Level Hours"],
         "searches": ["borrow checker", "allocators"], "videos": found("tps", "borrow checker", ["Tiny Systems", "Ferrous Notes"])},
    ]
    out = []
    for g in groups:
        if g["id"] in state["topics"]["hidden"]:
            continue
        if state["topics"]["fetched"]:
            g["fetchedAt"] = NOW
        else:
            g["videos"] = []
        out.append(g)
    return out


def topics():
    on = state["config"]["topicFeeds"]
    return {"enabled": on, "groups": topic_groups() if on else []}


if os.environ.get("QVEIL_STANDIN_EMPTY"):
    state["resume"], state["lenses"] = [], []
# As many lenses as there can be.
for _i in range(len(state["lenses"]), int(os.environ.get("QVEIL_STANDIN_LENSES", "0"))):
    state["lenses"].append({"id": "aaaa%04d" % (_i + 1), "text": "lens number %d, about something specific and long enough to wrap" % (_i + 1),
                            "created": NOW - DAY, "fetchedAt": NOW - HOUR})


def read(text, lens):
    """A small reading, enough for the window: topics by comma, one length rule, "no …" for a lens."""
    tags, searches, rules, error = [], [], False, None
    for clause in [c.strip() for c in text.split(",") if c.strip()]:
        words = clause.split()
        if lens and words[0] == "no" and len(words) > 1:
            tags.append({"label": "not", "value": " ".join(words[1:])})
            rules = True
            continue
        if "over" in words and words.index("over") + 2 < len(words) + 1 and words[-1] in ("min", "minutes"):
            at = words.index("over")
            tags_after = {"label": "longer than", "value": "%s:00" % words[at + 1]}
            clause, rules = " ".join(words[:at]), True
        else:
            tags_after = None
        if clause:
            searches.append(clause)
            tags.append({"label": "about", "value": clause})
        if tags_after:
            tags.append(tags_after)
    tags.sort(key=lambda t: t["label"] != "about")
    if lens and len(searches) != 1:
        rules = True
    out = {"tags": tags, "searches": searches, "rules": rules}
    if lens and not searches:
        out["error"] = 'Say what it should be about, for example "woodworking".'
    return out


def lens_entry(lens):
    r = read(lens["text"], True)
    out = {"id": lens["id"], "text": lens["text"], "created": lens["created"], "tags": r["tags"],
           "searches": r["searches"], "count": 6 if lens.get("fetchedAt") else 0}
    if lens.get("fetchedAt"):
        out["fetchedAt"] = lens["fetchedAt"]
    return out


def lens_videos(lens):
    r = read(lens["text"], True)
    topics = r["searches"] or ["it"]
    rule = [t["value"] for t in r["tags"] if t["label"] == "longer than"]
    out = []
    for i, v in enumerate(FEED[:6]):
        v = dict(v)
        v["id"] = "len%s%02d" % (lens["id"][-6:], i)
        v["why"] = [topics[i % len(topics)]] + ["over " + x for x in rule]
        out.append(v)
    return out


def png(seed, w=160, h=90):
    """A small picture that is different for every video: bands of two colours."""
    n = sum(ord(c) * (i + 3) for i, c in enumerate(seed))
    a = ((n * 37) % 150 + 50, (n * 59) % 130 + 40, (n * 83) % 150 + 60)
    b = ((n * 17) % 90 + 20, (n * 29) % 90 + 20, (n * 43) % 110 + 30)
    rows = bytearray()
    for y in range(h):
        rows.append(0)
        for x in range(w):
            t = ((x * 2 + y * 3 + n) % 97) / 96.0 if (x // 20 + y // 15 + n) % 3 else 0.2
            rows.extend(int(a[i] * t + b[i] * (1 - t)) for i in range(3))

    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)

    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(bytes(rows))) + chunk(b"IEND", b""))


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.0"

    def send(self, code, body):
        data = json.dumps(body).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def lines(self, items, pause=0.0):
        self.send_response(200)
        self.send_header("Content-Type", "application/x-ndjson")
        self.end_headers()
        for item in items:
            self.wfile.write((json.dumps(item) + "\n").encode())
            self.wfile.flush()
            if pause:
                time.sleep(pause)

    def parts(self):
        u = urllib.parse.urlparse(self.path)
        q = {k: v[0] for k, v in urllib.parse.parse_qs(u.query, keep_blank_values=True).items()}
        path = u.path.split("/t/", 1)[-1]
        if path != "api/_log" and not path.startswith("thumb/"):
            state["log"].append("%s %s%s" % (self.command, path, "?" + u.query if u.query else ""))
        return path, q

    def body(self):
        raw = self.rfile.read(int(self.headers.get("Content-Length") or 0))
        try:
            return json.loads(raw or b"{}")
        except ValueError:
            return {}

    def do_GET(self):
        path, q = self.parts()
        base = "http://%s/t" % self.headers.get("Host", "127.0.0.1")
        if path == "api/_log":
            self.send(200, state["log"])
        elif path == "api/config":
            self.send(200, state["config"])
        elif path == "api/feed":
            feed = {"channels": len(state["subs"]), "entries": FEED, "fallbacks": 0, "errors": [], "fresh": 0}
            if "stream" in q:
                self.lines([{"progress": {"done": 5, "total": 5}}, {"result": feed}])
            else:
                self.send(200, feed)
        elif path == "api/search":
            term = q.get("q", "")
            if term.startswith("code:"):
                self.lines([{"error": "The engine's words for " + term[5:], "code": term[5:]}])
                return
            r = read(term, False)
            found = []
            for i, v in enumerate(FEED[:8]):
                v = dict(v)
                v["id"] = "res%08d" % i
                if r["rules"]:
                    v["why"] = [t["value"] if t["label"] == "about" else "over " + t["value"] for t in r["tags"]]
                found.append(v)
            self.lines(found)
        elif path == "api/read":
            self.send(200, read(q.get("q", ""), q.get("for") == "lens"))
        elif path == "api/channel":
            cid = q.get("id", "")
            # As the engine does: a handle is looked up, and anything else must be a channel's ID.
            if cid.startswith("@"):
                cid = CHANNELS[0][0]
            elif not (cid.startswith("UC") and len(cid) == 24):
                self.send(400, {"error": "%r is not a channel ID" % cid, "code": "other"})
                return
            name = dict(CHANNELS).get(cid, "A channel")
            vids = [v for v in FEED if v["channel_id"] == cid] or FEED[:3]
            self.send(200, {"id": cid, "name": name, "subscribed": any(s["id"] == cid for s in state["subs"]), "videos": vids})
        elif path == "api/subs":
            self.send(200, state["subs"])
        elif path == "api/history":
            self.send(200, [{"id": v["id"], "title": v["title"], "channel": v["channel"], "at": NOW - i * HOUR}
                            for i, v in enumerate(FEED[:4])] if state["config"]["history"] else [])
        elif path == "api/resume":
            self.send(200, state["resume"] if state["config"]["resumeDays"] else [])
        elif path == "api/topics":
            if "refresh" in q and state["config"]["topicFeeds"]:
                state["topics"]["fetched"] = True
            if "stream" in q:
                self.lines([{"progress": {"done": 0, "total": 4}}, {"result": topics()}], 0.05)
            else:
                self.send(200, topics())
        elif path == "api/lenses":
            self.send(200, [lens_entry(l) for l in state["lenses"]])
        elif path == "api/lens":
            lens = next((l for l in state["lenses"] if l["id"] == q.get("id")), None)
            if not lens:
                self.send(500, {"error": "That lens no longer exists.", "code": "other"})
                return
            fetch = q.get("force") == "1" or (q.get("refresh") == "1" and not lens.get("fetchedAt"))
            if fetch:
                lens["fetchedAt"] = NOW
            entry = lens_entry(lens)
            result = {"lens": {"id": lens["id"], "text": lens["text"], "created": lens["created"]},
                      "tags": entry["tags"], "searches": entry["searches"],
                      "videos": lens_videos(lens) if lens.get("fetchedAt") else []}
            if lens.get("fetchedAt"):
                result["fetchedAt"] = lens["fetchedAt"]
            total = len(entry["searches"])
            progress = [{"progress": {"done": i, "total": total}} for i in range(total + 1)] if fetch else []
            if "stream" in q:
                self.lines(progress + [{"result": result}], 0.15 if fetch else 0)
            else:
                self.send(200, result)
        elif path == "api/comments":
            if q.get("v") == "slowslowslo":
                time.sleep(5)
            n = int(q.get("n", "40"))
            found = json.loads(json.dumps(COMMENTS))
            if q.get("replies") != "1":
                for c in found:
                    c["replies"] = []
            if q.get("sort") == "new":
                found.reverse()
            if q.get("v") == "nocomments0":
                found = []
            # More of them when more are asked for, so that "More" shows something.
            extra = [{"id": "x%d" % i, "author": "viewer_%d" % i, "text": "Comment number %d." % i, "likes": i,
                      "timeText": "%d days ago" % (i - 39), "pinned": False, "byUploader": False, "replies": []}
                     for i in range(40, n)] if found else []
            self.send(200, {"sort": q.get("sort", "top"), "count": 1204 if found else 0, "comments": found + extra})
        elif path.startswith("thumb/"):
            data = png(path[6:])
            # Pictures made for stills, when there are some (bin/shot says where).
            pictures = os.environ.get("QVEIL_STANDIN_THUMBS", "")
            if pictures:
                name = path[6:].split("?")[0]
                n = int(name[-2:]) + 1 if name.startswith("len") else int("".join(c for c in name if c.isdigit()) or 1)
                try:
                    with open(os.path.join(pictures, "%d.png" % ((n - 1) % 12 + 1)), "rb") as f:
                        data = f.read()
                except OSError:
                    pass
            self.send_response(200)
            self.send_header("Content-Type", "image/png")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
        elif path.startswith("s/") and CLIP:
            size = os.path.getsize(CLIP)
            self.send_response(200)
            self.send_header("Content-Type", "video/x-matroska")
            self.send_header("Content-Length", str(size))
            self.end_headers()
            with open(CLIP, "rb") as f:
                try:
                    while chunk := f.read(65536):
                        self.wfile.write(chunk)
                except OSError:
                    pass
        else:
            self.send(404, {"error": "not found", "code": "other"})

    def do_POST(self):
        path, q = self.parts()
        body = self.body()
        base = "http://%s/t" % self.headers.get("Host", "127.0.0.1")
        if path == "api/_die":
            # For the tests of what the window does when the engine is gone.
            os._exit(3)
        if path == "api/config":
            cfg = state["config"]
            for key in ("route", "history", "thumbnails", "maxHeight", "resumeDays", "topicFeeds"):
                if key in body:
                    cfg[key] = body[key]
            cfg["resumeDays"] = max(0, min(365, int(cfg["resumeDays"])))
            if cfg["resumeDays"] == 0:
                state["resume"] = []
            days = cfg["resumeDays"]
            cfg["privacy"]["pages"]["resume"] = (
                "Off: qveil doesn't remember where you stopped." if days == 0 else
                "On: qveil remembers where you stopped in an unfinished video for %d day%s." % (days, "" if days == 1 else "s"))
            cfg["privacy"]["pages"]["topics"] = TOPICS_ON if cfg["topicFeeds"] else TOPICS_OFF
            cfg["privacy"]["summary"] = {"direct": "Direct: YouTube sees your IP address.",
                                         "tor": "Through Tor: YouTube sees an exit node's address, not your IP.",
                                         "proxy": "Through your proxy: YouTube sees its address, not your IP."}[cfg["route"]]
            self.send(200, cfg)
        elif path == "api/play":
            v = q.get("v", "")
            if v in ("bot_check", "tor_down", "proxy_refused", "rate_limited", "age_restricted", "route_changed"):
                self.send(500, {"error": "The engine's words for " + v, "code": v})
                return
            stream = base + "/s/x/v" if CLIP else ""
            if v.startswith("bare"):
                self.send(200, {"id": v, "title": "A video with nothing but streams", "video": stream})
                return
            known = next((f for f in FEED if f["id"] == v), FEED[0])
            info = {"id": v, "title": known["title"], "channel": known["channel"], "channelId": known["channel_id"],
                    "duration": 2463, "video": stream, "description": DESCRIPTION, "views": 59000, "likes": 640,
                    "published": NOW - 10 * HOUR, "chapters": [{"start": s, "title": t} for s, t in CHAPTERS]}
            kept = next((r for r in state["resume"] if r["id"] == v), None)
            if kept:
                info["resume"] = kept["pos"]
            self.send(200, info)
        elif path == "api/resume":
            state["resume"] = [r for r in state["resume"] if r["id"] != body.get("v")]
            pos, dur = int(body.get("pos", 0)), int(body.get("dur", 0))
            if dur <= 0 or pos > dur:
                noted = "unknown"
            elif pos < 30:
                noted = "too_early"
            elif pos + 30 >= dur:
                noted = "finished"
            else:
                noted = "kept"
                state["resume"].insert(0, {"id": body.get("v"), "pos": pos, "dur": dur, "at": NOW,
                                           "title": body.get("title", ""), "channel": body.get("channel"),
                                           "expires": NOW + 7 * DAY})
            self.send(200, {"noted": noted})
        elif path == "api/topics/hide":
            state["topics"]["hidden"].append(q.get("id", ""))
            self.send(200, {})
        elif path == "api/resume/rm":
            state["resume"] = [r for r in state["resume"] if r["id"] != q.get("v")]
            self.send(200, {})
        elif path == "api/resume/clear":
            state["resume"] = []
            self.send(200, {})
        elif path == "api/lenses":
            r = read(body.get("text", ""), True)
            if "error" in r:
                self.send(500, {"error": r["error"], "code": "other"})
                return
            lens = {"id": "bbbb%04d" % state["next"], "text": body["text"].strip(), "created": NOW}
            state["next"] += 1
            state["lenses"].append(lens)
            self.send(200, lens_entry(lens))
        elif path.startswith("api/lenses/"):
            rest = path[len("api/lenses/"):].split("/")
            lens = next((l for l in state["lenses"] if l["id"] == rest[0]), None)
            if not lens:
                self.send(500, {"error": "That lens no longer exists.", "code": "other"})
            elif rest[1:] == ["rm"]:
                state["lenses"].remove(lens)
                self.send(200, {})
            else:
                r = read(body.get("text", ""), True)
                if "error" in r:
                    self.send(500, {"error": r["error"], "code": "other"})
                    return
                lens["text"] = body["text"].strip()
                lens.pop("fetchedAt", None)
                self.send(200, lens_entry(lens))
        elif path == "api/subs/add":
            sub = {"id": "UCnnnnnnnnnnnnnnnnnnnnnn", "name": q.get("c", "New")}
            state["subs"].append(sub)
            self.send(200, {"sub": sub, "added": True})
        elif path == "api/subs/rm":
            gone = next((s for s in state["subs"] if s["id"] == q.get("id")), {"name": "?"})
            state["subs"] = [s for s in state["subs"] if s["id"] != q.get("id")]
            self.send(200, gone)
        else:
            self.send(200, {})

    def log_message(self, *args):
        pass


def main():
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    server.daemon_threads = True
    print(json.dumps({"base": "http://127.0.0.1:%d/t" % server.server_port}), flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    sys.exit(main())
