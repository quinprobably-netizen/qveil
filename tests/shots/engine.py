#!/usr/bin/env python3
"""A stand-in for qveil-core, for the window's tests and its stills.

It speaks the engine's loopback API from memory: a fixed feed, search
results, a channel, lenses, comments, resume positions and settings, none of
which comes from anywhere but this file. Nothing here touches the network.

`GET api/_log` lists the requests made so far, so a test can tell what the
window asked for (and what it didn't). With QVEIL_STANDIN_CLIP set to a media
file, a played video's stream is that file; without it a played video has no
stream, so nothing is decoded and nothing is heard.

With QVEIL_STANDIN_LIVECLIP set to a folder of segments (`v/<n>.mkv` and
`a/<n>.m4a`, LIVE_SEGMENT seconds each, numbered from 0), a video whose ID
starts with "live" is a running stream, as the engine plays one: two EVENT
playlists that list LIVE_FIRST segments when it is opened and one more each
LIVE_SEGMENT seconds, and `live/<sid>/state`. `POST api/_live_end` ends it:
the playlists get `#EXT-X-ENDLIST`, and the state says "ended". `POST
api/_live_drop` forgets its session, as an engine that no longer has it.
`POST api/_live_fail` fails it, as an engine that can't go on with it: the
state is a 502 that says why. `POST api/_live_state_hold` holds every answer
to the state until `POST api/_live_state_release`, as a slow engine would.

A video whose ID starts with "waiting" is an announced stream: its play
answers `waiting` and no streams, and `GET api/wait` is its waiting room (see
`Handler.wait`). Once the room has said "now", playing it again answers the
running stream above. `GET api/_waits` says how many rooms are open.

`GET api/chat` is a running stream's live chat (see `Handler.chat`), refused
with 409 while the `liveChat` setting is off. `GET api/_chats` says how many
chats are open.
"""
import http.server
import json
import os
import select
import socket
import struct
import sys
import threading
import time
import urllib.parse
import zlib

NOW = int(time.time())
HOUR, DAY = 3600, 86400
CLIP = os.environ.get("QVEIL_STANDIN_CLIP", "")
LIVECLIP = os.environ.get("QVEIL_STANDIN_LIVECLIP", "")
# (QVEIL_STANDIN_LIVE_FIRST: how many segments it has when it is opened, for
# a stream longer than the player reads ahead. QVEIL_STANDIN_LIVE_EDGE_SKEW:
# seconds its state's `edge` is off from the playlists' own length, as the
# engine's is for a stream whose media clock has drifted from its count of
# segments; so that a test can tell the window follows `edge`.)
LIVE_SEGMENT, LIVE_FIRST = 2, int(os.environ.get("QVEIL_STANDIN_LIVE_FIRST", "8"))
LIVE_EDGE_SKEW = float(os.environ.get("QVEIL_STANDIN_LIVE_EDGE_SKEW", "0"))
# How many segments back from the newest a player joins a running stream.
LIVE_JOIN = 3
# When the running stream was opened (it grows from then), and whether it has ended.
live = {"opened": 0.0, "ended": False, "sid": 0, "final": 0, "failed": None, "hold": False}
# What the engine says when it can't go on with a running stream (see api/_live_fail).
LIVE_FAILED = "The stream changed format while it played; open it again."
# The waiting rooms open now, and the announced streams that have started.
waits = {"open": 0, "started": set(), "lock": threading.Lock()}
# When each announced stream starts, from the moment it is played, by the
# end of its ID: an hour and three quarters ahead, a little late, very late,
# or not known.
WAIT_STARTS = {"late": -12 * 60, "verylate": -40 * 60, "none": None}
# The live chat (`GET api/chat`): its first messages (someone, the channel's
# owner, a Super Chat, and markup that must show as it was typed), and the
# chats open now, with how often each video's was asked for.
CHAT = [
    {"author": "mara_k", "text": "is that cone 10 already?", "owner": False, "amount": None},
    {"author": "Low Level Hours", "text": "not yet, about cone 8. another hour", "owner": True, "amount": None},
    {"author": "ottoleaf", "text": "for the gas bill :fire:", "owner": False, "amount": "$5.00"},
    {"author": "pell", "text": "<b>not bold</b> & <a href='x'>no link</a>", "owner": False, "amount": None},
]
# What a still of the chat shows after those (QVEIL_STANDIN_CHAT_STILL).
CHAT_STILL = [
    ("junebug", "what's the allocator doing on free, exactly?", None),
    ("tomás", "first time catching one of these live", None),
    ("rook_s", "watching from the night shift :wave:", None),
    ("Low Level Hours", "on free it goes back to the size class list, I'll show it in a sec", None),
    ("ivy", "the flame graph on the left is genius", None),
    ("pell", "more of these streams please", "$2.00"),
    ("mara_k", ":clap: :clap:", None),
    ("ottoleaf", "does the arena ever give memory back to the OS?", None),
    ("junebug", "thanks!", None),
]
chats = {"open": 0, "asked": {}, "lock": threading.Lock()}
BOT_CHECK = "YouTube is asking this IP to sign in (bot check). Try another route or try later."
CHAT_OFF = "Live chat is turned off in the settings"
GAVE_UP = "The stream hasn't started, and qveil stopped waiting for it after 6 hours."
MEMBERS_ONLY = "This video is for the channel's members only; qveil never signs in, so it can't be played."


def wait_starts(v):
    """When the announced stream `v` starts, as its play answers it."""
    kind = v[len("waiting"):]
    for name, offset in WAIT_STARTS.items():
        if kind.startswith(name):
            return None if offset is None else int(time.time()) + offset
    return int(time.time()) + HOUR + 42 * 60 + 10


def live_segments():
    """How many segments there are of both tracks."""
    def count(track, ext):
        n = 0
        while os.path.exists(os.path.join(LIVECLIP, track, "%d.%s" % (n, ext))):
            n += 1
        return n
    return min(count("v", "mkv"), count("a", "m4a"))


def live_head():
    """The newest segment the stream has: it grows on a clock, up to the last one there is."""
    if live["ended"]:
        return live["final"]
    grown = int((time.monotonic() - live["opened"]) / LIVE_SEGMENT)
    return min(live_segments() - 1, LIVE_FIRST - 1 + grown)


def live_edge():
    """The end of the newest segment on the player's clock, as the state says it."""
    return LIVE_SEGMENT * (live_head() + 1) + LIVE_EDGE_SKEW


def live_playlist(track, ext):
    out = ["#EXTM3U", "#EXT-X-VERSION:7", "#EXT-X-TARGETDURATION:%d" % LIVE_SEGMENT,
           "#EXT-X-PLAYLIST-TYPE:EVENT", "#EXT-X-MEDIA-SEQUENCE:0"]
    for n in range(live_head() + 1):
        out += ["#EXTINF:%d.000," % LIVE_SEGMENT, "%s/%d.%s" % (track, n, ext)]
    if live["ended"]:
        out.append("#EXT-X-ENDLIST")
    return ("\n".join(out) + "\n").encode()

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

COMMENT_THREADS = [
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

# The engine's own words (core/src/privacy.rs), on one line each so that a
# test of the engine's (`the_stand_in_engine_says_the_same`) can check they
# are the same, and that every page the engine has a text for is here.
SUMMARY_DIRECT = "Direct: YouTube sees your IP address."
SUMMARY_TOR = "Through Tor: YouTube sees an exit node's address, not your IP, but can still link requests by their fingerprint and timing."
ROUTE_DIRECT = "YouTube sees your IP address."
ROUTE_TOR = "Fresh SOCKS credentials per request, which Tor puts on separate circuits (a video, and a page of thumbnails, each share one); requests can still be linked by their fingerprint and timing."
ROUTE_PROXY = "A proxy you trust, such as a VPN's: YouTube sees its address, not yours. A SOCKS proxy gets fresh credentials per request (a separate circuit if it is Tor); a proxy URL with credentials of its own is not isolated."
DIRECT_POINT = "YouTube sees your IP address with every request, which links them all."
FINGERPRINT_POINT = "qveil's requests carry a browser User-Agent on a TLS handshake that no browser makes, which is probably rare, so a careful observer can link them by that fingerprint, even across circuits, and by the one-byte request (bytes=0-0) for the start of a video that keeps a connection ready for a seek, sent at most once every 10 seconds while it plays over HTTP/1.1."
SUBSCRIPTIONS_POINT = "Refreshing your subscriptions asks for each channel on its own, in random order, skipping channels fetched in the last 15 minutes: that is still a burst of requests for exactly your channels, so timing and the set of IDs can link them."
COOKIES_POINT = "You are never signed in, and cookies are never stored or read from a browser: yt-dlp sends the one consent cookie it sets for itself, and sends back what YouTube sets during a single call, kept in memory until that call ends."
COMMENTS_POINT = "Comments are fetched only when you open them, as one more lookup of that video, on a circuit of its own over Tor, which YouTube can still tie to the video by its ID and timing; commenters' pictures are never fetched."
LOOKUP_POINT = "Playing a video, or getting it ready, looks it up the way yt-dlp does by itself: its watch page, and then YouTube's player for its streams, with the same identity. Its chapters, its channel's name and its age come with that lookup, and cost no request of their own. yt-dlp asks the player as one of YouTube's own clients, with that client's User-Agent (with current yt-dlp, Safari's, from its visionOS client) and not qveil's Chrome one, which the video's bytes carry: a lookup and its stream show two browser identities."
LIVE_PLAYBACK_POINT = "Live streams are fetched segment by segment over your route, a few seconds ahead of what plays."
WAITING_POINT = "Waiting for an announced stream asks YouTube once at its start time, then every minute (every 5 minutes after half an hour late), and stops after 6 hours or when you leave. Opening its waiting room also reads its channel's Streams tab once, to list what else the channel has running or announced."
LENSES_POINT = "A lens is refreshed as one search per topic (a second one for a topic whose filtered search found nothing), in a new random order each time and 2 to 8 seconds apart: on one address their timing still links them, and even over Tor a set of related searches can be linked by content."
LOCAL_POINT = "Subscriptions, lenses, history, resume positions and the caches stay on this computer, in files only you can read, and qveil has no telemetry."
CHAT_POINT = "Live chat is read only while its tab is open: one long-running request stream over your route. No pictures (avatars, badges, emoji) are fetched."
SEARCH = "Straight from YouTube's search, without an account; qveil stores no cookies and doesn't keep your searches. A search with a filter in it (\"over 30 min\", \"-word\") is sent as one search per topic, like a lens, and filtered and ordered on this computer."
SUBSCRIPTIONS = "Kept only on this computer. Each channel is fetched on its own, in random order, but a refresh is still a burst of requests for exactly your channels."
HISTORY_OFF = "Off: qveil keeps no history. Where you stopped in unfinished videos is remembered separately, while resume is on."
HISTORY_ON = "On: kept only on this computer, in a file only you can read, until you clear it."
TOPICS_OFF = "Off: qveil doesn't work out what the channels you follow are about, and searches for nothing by itself."
TOPICS_ON = "On: qveil groups the channels you follow by the words that keep coming back in their titles, on this computer, and searches YouTube by itself for up to three of those phrases for each of up to four groups, in a new random order each time and 2 to 8 seconds apart: at most twelve searches in a day, each counted before it is sent, and none for a group whose results are less than a day old, both by this computer's clock (changing the clock, or deleting qveil's data, can start the day over). YouTube sees those searches. They say what the channels you follow are about, and can point to a channel itself when what comes back in its titles is a series or a name. Videos from channels you follow are left out of what is shown. Turned off, what was found is deleted, but when the day's searches were made is kept until each is a day old, so that turning it off and on again doesn't start the count over; each is deleted by the next time qveil starts after that."
COMMENTS = "Fetched only when you open them, as one more lookup of that video, on a circuit of its own over Tor, which YouTube can still tie to the video by its ID and timing. Commenters' pictures are never fetched."
COMMENTS_SHORT = "Loaded on demand, one extra request."
LENSES = "A refresh sends one search per topic, each a request of its own, in a new random order each time and 2 to 8 seconds apart. YouTube sees those searches, and a length, date or order rule that it has a filter for is sent with the search as YouTube's own filter; a topic whose filtered search finds nothing is searched a second time without it. Your wording, the other rules and the ranking stay on this computer."
LENSES_SHORT = "A refresh sends each topic to YouTube as a search of its own, sometimes twice."
PREPARE_OFF = "Off: a video is looked up when you play it, and not before."
PREPARE_ON = "On: in the last 30 seconds of a video you play from a list, qveil looks up the next one in Up next, so that it starts at once when you go on to it. YouTube (over Tor, the exit node too) sees that lookup even if you never watch the video. Over Tor it goes on a circuit of its own, which the video then plays on if you start it within nine minutes. Nothing is looked up because you pointed at it."
KEEP_THUMBS_OFF = "Off: thumbnails are kept in memory while qveil runs, and not on disk."
KEEP_THUMBS_ON = "On: the thumbnails you were shown are kept on disk, in ~/.cache/qveil/thumbs, in files only you can read (at most 64 MiB, the least recently shown going first), so that they aren't fetched again. They are a record, on this computer, of the pictures you were shown. Turning this off, clearing the history or qveil wipe deletes them."
RESUME_OFF = "Off: qveil doesn't remember where you stopped."
RESUME_ON = "On: qveil remembers where you stopped in an unfinished video for {days}, in a file only you can read. After that it is no longer offered, and it is deleted the next time qveil starts or saves a position. A finished video is forgotten at once."



def resume_text(days):
    """What the engine says of resume when positions are kept for `days`."""
    if days == 0:
        return RESUME_OFF
    return RESUME_ON.format(days="1 day" if days == 1 else "%d days" % days)


PRIVACY = {
    "summary": SUMMARY_DIRECT,
    "points": [DIRECT_POINT, FINGERPRINT_POINT, SUBSCRIPTIONS_POINT, COOKIES_POINT, COMMENTS_POINT, LOOKUP_POINT, LIVE_PLAYBACK_POINT, WAITING_POINT, LENSES_POINT, LOCAL_POINT, CHAT_POINT],
    "routes": {"direct": ROUTE_DIRECT,
               "tor": ROUTE_TOR,
               "proxy": ROUTE_PROXY},
    "pages": {"search": SEARCH,
              "subscriptions": SUBSCRIPTIONS,
              "history_off": HISTORY_OFF,
              "history_on": HISTORY_ON,
              "topics": TOPICS_OFF,
              "topics_off": TOPICS_OFF,
              "topics_on": TOPICS_ON,
              "resume": RESUME_ON.format(days="7 days"),
              "comments": COMMENTS,
              "comments_short": COMMENTS_SHORT,
              "lenses": LENSES,
              "lenses_short": LENSES_SHORT,
              "prepare_off": PREPARE_OFF,
              "prepare_on": PREPARE_ON,
              "keep_thumbs_off": KEEP_THUMBS_OFF,
              "keep_thumbs_on": KEEP_THUMBS_ON},
}

state = {
    "config": {"route": "direct", "proxy": None, "describe": "direct", "isolated": False, "torRunning": True,
               "history": False, "thumbnails": True, "maxHeight": 1080, "resumeDays": 7,
               "topicFeeds": bool(os.environ.get("QVEIL_STANDIN_TOPICS")),
               "prepareNext": False,
               "keepThumbnails": False,
               "liveCheck": False,
               "liveChat": True,
               "configPath": "/dev/null", "privacy": PRIVACY,
               # QVEIL_STANDIN_IGNORED: keys of the settings file that were left out.
               "ignoredKeys": [k for k in os.environ.get("QVEIL_STANDIN_IGNORED", "").split(",") if k]},
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
    # The videos got ready (`api/prepare`), which are not looked up again.
    "prepared": set(),
}
# What the topics page says follows the setting, as the engine's does.
PRIVACY["pages"]["topics"] = TOPICS_ON if state["config"]["topicFeeds"] else TOPICS_OFF
# Topic feeds: two groups of the channels above, and what a search found
# for each on other channels. `fetched` says whether they were searched for.
# QVEIL_STANDIN_TOPICS=fetched: they were searched for on an earlier day.
state["topics"] = {"fetched": os.environ.get("QVEIL_STANDIN_TOPICS") == "fetched", "hidden": [], "new": set()}


def topic_groups():
    def found(prefix, phrase, names):
        return [{"id": "%s%07d" % (prefix, i), "title": "%s, number %d" % (phrase, i + 1), "channel": names[i % len(names)],
                 "channel_id": "UCzzzzzzzzzzzzzzzzzzzzz%d" % (i % len(names)), "published": NOW - (i + 1) * DAY,
                 "duration": 900 + 300 * i, "views": 1000 * (i + 1), "why": [phrase if i % 2 == 0 else phrase + " restoration"]} for i in range(8)]
    groups = [
        {"id": "aa11bb22cc33", "label": "hand plane, dovetail", "channels": ["Northlight Woodworks", "Kiln Notes"],
         "searches": ["hand plane", "dovetail"], "videos": found("tpw", "hand plane", ["Quiet Bench", "Old Iron", "Shavings"])},
        {"id": "dd44ee55ff66", "label": "borrow checker, allocators", "channels": ["Low Level Hours"],
         "searches": ["borrow checker", "allocators"], "videos": found("tps", "borrow checker", ["Tiny Systems", "Ferrous Notes"])},
    ]
    # More groups than are shown at once, when asked for: hiding one lets the next in.
    for i in range(2, int(os.environ.get("QVEIL_STANDIN_TOPIC_GROUPS", "2"))):
        groups.append({"id": "%012x" % (i + 1), "label": "topic %d" % (i + 1), "channels": ["Channel %d" % i],
                       "searches": ["topic %d" % (i + 1)], "videos": found("tp%d" % i, "topic %d" % (i + 1), ["Someone"])})
    # With QVEIL_STANDIN_LIVE, the first group has a stream announced for in
    # two hours and one that was streamed three days ago.
    if os.environ.get("QVEIL_STANDIN_LIVE"):
        groups[0]["videos"][0].update({"live": "upcoming", "starts": NOW + 2 * HOUR + 600})
        groups[0]["videos"][2]["streamed"] = True
    out = []
    for g in groups:
        if g["id"] in state["topics"]["hidden"]:
            continue
        if len(out) == 4:
            break
        # A group that was not among those searched for has nothing yet.
        if state["topics"]["fetched"] and g["id"] not in state["topics"]["new"]:
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
        # With QVEIL_STANDIN_LIVE, each lens's first video is live now.
        if i == 0 and os.environ.get("QVEIL_STANDIN_LIVE"):
            v.update({"live": "now", "watching": 863})
        v["id"] = "len%s%02d" % (lens["id"][-6:], i)
        v["why"] = [topics[i % len(topics)]] + ["over " + x for x in rule]
        out.append(v)
    return out


def feed_entries():
    """The feed. With QVEIL_STANDIN_LIVE, the live check has found two
    streams running and three announced: two starting within a day (the
    sooner one later in the feed) and one in three days. Set to "many", it
    has found four running and one announced; set to "upcoming", only the
    three announced ones."""
    which = os.environ.get("QVEIL_STANDIN_LIVE", "")
    if not which:
        return FEED
    running = {"vid00000002": {"live": "now", "watching": 1200},
               "vid00000009": {"live": "now"}}
    announced = {"vid00000003": {"live": "upcoming", "starts": NOW + 5 * HOUR},
                 "vid00000006": {"live": "upcoming", "starts": NOW + 3 * DAY},
                 "vid00000012": {"live": "upcoming", "starts": NOW + HOUR}}
    if which == "upcoming":
        marks = announced
    elif which == "many":
        marks = dict(running, **{"vid00000011": {"live": "now", "watching": 310},
                                 "vid00000012": {"live": "now", "watching": 45},
                                 "vid00000003": announced["vid00000003"]})
    else:
        marks = dict(running, **announced)
    return [dict(v, **marks.get(v["id"], {})) for v in FEED]


def channel_streams(cid):
    """A channel's Streams tab, as the engine answers it: running, then
    upcoming, then past broadcasts."""
    _, name = next((c for c in CHANNELS if c[0] == cid), (cid, "A channel"))
    def stream(n, title, **mark):
        return dict({"id": "str%08d" % n, "title": title, "channel": name, "channel_id": cid,
                     "published": NOW - n * DAY, "duration": 0, "views": 0}, **mark)
    # (QVEIL_STANDIN_NO_LINEUP: nothing running or announced, only past broadcasts.)
    if os.environ.get("QVEIL_STANDIN_NO_LINEUP"):
        return [stream(3, "Live: the long one, part 1", streamed=True, duration=11564, views=40000),
                stream(4, "Live: reading the source", streamed=True, duration=9670, views=21000)]
    return [stream(1, "Live: the long one, part 2", live="now", watching=860),
            stream(2, "Questions, answered live", live="upcoming", starts=NOW + DAY),
            stream(3, "Live: the long one, part 1", streamed=True, duration=11564, views=40000),
            stream(4, "Live: reading the source", streamed=True, duration=9670, views=21000)]


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
        if path not in ("api/_log", "api/_waits", "api/_chats") and not path.startswith("thumb/"):
            state["log"].append("%s %s%s" % (self.command, path, "?" + u.query if u.query else ""))
        return path, q

    def body(self):
        raw = self.rfile.read(int(self.headers.get("Content-Length") or 0))
        try:
            return json.loads(raw or b"{}")
        except ValueError:
            return {}

    def stream(self):
        """The clip, whole or as the `Range: bytes=a-[b]` asks (as the real stream is), so a player can seek in it."""
        size = os.path.getsize(CLIP)
        first, last, code = 0, size - 1, 200
        asked = self.headers.get("Range", "")
        if asked.startswith("bytes=") and "," not in asked:
            a, _, b = asked[len("bytes="):].partition("-")
            if a.isdigit() and (b.isdigit() or not b):
                first, last, code = int(a), min(int(b), size - 1) if b else size - 1, 206
        # Only here is it HTTP/1.1 (the rest are served as 1.0), and each answer closes its connection.
        self.protocol_version = "HTTP/1.1"
        if code == 206 and (first >= size or first > last):
            self.send_response(416)
            self.send_header("Content-Range", "bytes */%d" % size)
            self.send_header("Content-Length", "0")
            self.send_header("Connection", "close")
            self.end_headers()
            return
        self.send_response(code)
        self.send_header("Content-Type", "video/x-matroska")
        self.send_header("Accept-Ranges", "bytes")
        if code == 206:
            self.send_header("Content-Range", "bytes %d-%d/%d" % (first, last, size))
        self.send_header("Content-Length", str(last - first + 1))
        self.send_header("Connection", "close")
        self.end_headers()
        with open(CLIP, "rb") as f:
            f.seek(first)
            left = last - first + 1
            try:
                while left > 0 and (chunk := f.read(min(65536, left))):
                    self.wfile.write(chunk)
                    left -= len(chunk)
            except OSError:
                pass

    def file(self, data, kind):
        self.send_response(200)
        self.send_header("Content-Type", kind)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def hung_up(self):
        """Whether the window has hung up: its side of the connection is closed."""
        try:
            readable, _, _ = select.select([self.connection], [], [], 0)
            return bool(readable) and self.connection.recv(1, socket.MSG_PEEK) == b""
        except OSError:
            return True

    def wait(self, q):
        """`GET api/wait`, as the engine answers it: "upcoming" at once and as
        a heartbeat (every QVEIL_STANDIN_WAIT_BEAT seconds, 25 by default),
        until the window hangs up. With QVEIL_STANDIN_WAIT_NOW_MS set, the
        stream starts that many ms after the room opened: "now", and the job
        ends. A "waitinggaveup…" stream is given up on, and a
        "waitingmembers…" one turns out to be for members only. A
        "waitingbot…" one meets a bot check at every ask: as the engine does
        (core/src/server/wait.rs, whose docs list the lines), the wait goes
        on, and its lines say so (`note`), until the window hangs up."""
        v = q.get("v", "")
        raw = q.get("starts", "")
        starts = int(raw) if raw.lstrip("-").isdigit() else None
        beat = float(os.environ.get("QVEIL_STANDIN_WAIT_BEAT", "25"))
        now_ms = os.environ.get("QVEIL_STANDIN_WAIT_NOW_MS", "")
        self.send_response(200)
        self.send_header("Content-Type", "application/x-ndjson")
        self.end_headers()

        def say(line):
            self.wfile.write((json.dumps(line) + "\n").encode())
            self.wfile.flush()

        with waits["lock"]:
            waits["open"] += 1
        try:
            say({"state": "upcoming", "starts": starts})
            note = {}
            if v.startswith("waitingbot"):
                time.sleep(0.3)
                note = {"note": "bot_check"}
                say(dict({"state": "upcoming", "starts": starts}, **note))
            if v.startswith("waitinggaveup"):
                time.sleep(0.3)
                say({"state": "gave_up", "error": GAVE_UP, "code": "gave_up"})
                return
            if v.startswith("waitingmembers"):
                time.sleep(0.3)
                say({"error": MEMBERS_ONLY, "code": "members_only"})
                return
            opened = last = time.monotonic()
            while not self.hung_up():
                if now_ms and (time.monotonic() - opened) * 1000 >= float(now_ms):
                    with waits["lock"]:
                        waits["started"].add(v)
                    say({"state": "now"})
                    return
                if time.monotonic() - last >= beat:
                    say(dict({"state": "upcoming", "starts": starts}, **note))
                    last = time.monotonic()
                time.sleep(0.05)
        except OSError:
            pass
        finally:
            with waits["lock"]:
                waits["open"] -= 1

    def chat(self, q):
        """`GET api/chat`, as the engine answers it: CHAT at once, then
        QVEIL_STANDIN_CHAT_MORE numbered messages in the same go, then one
        more every QVEIL_STANDIN_CHAT_EVERY seconds (none if unset), with a
        heartbeat every QVEIL_STANDIN_CHAT_BEAT seconds (25 by default) of
        silence (with QVEIL_STANDIN_CHAT_STILL, CHAT_STILL in place of the
        markup), until the window hangs up or the stream ends. A
        "livebotonce…" stream's chat meets a bot check the first time it is
        asked for, and a "livebot…" one every time."""
        if not state["config"]["liveChat"]:
            self.send(409, {"error": CHAT_OFF, "code": "other"})
            return
        v = q.get("v", "")
        more = int(os.environ.get("QVEIL_STANDIN_CHAT_MORE", "0"))
        every = float(os.environ.get("QVEIL_STANDIN_CHAT_EVERY", "0") or 0)
        beat = float(os.environ.get("QVEIL_STANDIN_CHAT_BEAT", "25"))
        with chats["lock"]:
            chats["asked"][v] = chats["asked"].get(v, 0) + 1
            asked = chats["asked"][v]
        self.send_response(200)
        self.send_header("Content-Type", "application/x-ndjson")
        self.end_headers()

        def say(lines):
            self.wfile.write("".join(json.dumps(line) + "\n" for line in lines).encode())
            self.wfile.flush()

        with chats["lock"]:
            chats["open"] += 1
        try:
            if v.startswith("livebotonce") and asked == 1 or v.startswith("livebot") and not v.startswith("livebotonce"):
                time.sleep(0.2)
                say([{"error": BOT_CHECK, "code": "bot_check"}])
                return
            n = 0

            def message():
                return {"author": "viewer%d" % n, "text": "message %d" % n, "owner": False, "amount": None}
            batch = list(CHAT)
            if os.environ.get("QVEIL_STANDIN_CHAT_STILL"):
                batch = batch[:3] + [{"author": a, "text": t, "owner": a == CHANNELS[0][1], "amount": m}
                                     for a, t, m in CHAT_STILL]
            for _ in range(more):
                n += 1
                batch.append(message())
            say(batch)
            last = said = time.monotonic()
            while not self.hung_up() and not live["ended"]:
                now = time.monotonic()
                if every and now - said >= every:
                    n += 1
                    say([message()])
                    said = last = now
                elif now - last >= beat:
                    say([{"heartbeat": True}])
                    last = now
                time.sleep(0.02)
        except OSError:
            pass
        finally:
            with chats["lock"]:
                chats["open"] -= 1

    def live(self, rest):
        """A running stream's playlists, segments and state, as the engine serves them."""
        if not rest or rest[0] != str(live["sid"]):
            self.send(404, {"error": "no such session", "code": "other"})
        elif rest[1:] == ["state"]:
            held = time.monotonic()
            while live["hold"] and not self.hung_up() and time.monotonic() - held < 10:
                time.sleep(0.02)
            if live["failed"]:
                self.send(502, {"error": live["failed"], "code": "other"})
                return
            self.send(200, {"state": "ended" if live["ended"] else "now", "head": live_head(), "oldest": 0,
                            "edge": live_edge()})
        elif rest[1:] in (["video.m3u8"], ["audio.m3u8"]):
            video = rest[1] == "video.m3u8"
            self.file(live_playlist("v" if video else "a", "mkv" if video else "m4a"), "application/vnd.apple.mpegurl")
        elif len(rest) == 3 and rest[1] in ("v", "a"):
            name = os.path.basename(rest[2])
            n = name.split(".")[0]
            if not n.isdigit() or int(n) > live_head():
                self.send(404, {"error": "no such segment", "code": "other"})
                return
            with open(os.path.join(LIVECLIP, rest[1], name), "rb") as f:
                self.file(f.read(), "video/x-matroska" if rest[1] == "v" else "audio/mp4")
        else:
            self.send(404, {"error": "not found", "code": "other"})

    def do_GET(self):
        path, q = self.parts()
        base = "http://%s/t" % self.headers.get("Host", "127.0.0.1")
        if path == "api/_log":
            self.send(200, state["log"])
        elif path == "api/_waits":
            self.send(200, {"open": waits["open"]})
        elif path == "api/wait":
            self.wait(q)
        elif path == "api/_chats":
            self.send(200, {"open": chats["open"]})
        elif path == "api/chat":
            self.chat(q)
        elif path == "api/config":
            self.send(200, state["config"])
        elif path == "api/feed":
            # For the tests of what shows while the feed is still coming.
            time.sleep(float(os.environ.get("QVEIL_STANDIN_SLOW_FEED", "0")))
            feed = {"channels": len(state["subs"]), "entries": feed_entries(), "fallbacks": 0, "errors": [], "fresh": 0}
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
            vids = channel_streams(cid) if q.get("tab") == "streams" else [v for v in FEED if v["channel_id"] == cid] or FEED[:3]
            self.send(200, {"id": cid, "name": name, "subscribed": any(s["id"] == cid for s in state["subs"]), "videos": vids})
        elif path == "api/subs":
            self.send(200, state["subs"])
        elif path == "api/history":
            self.send(200, [{"id": v["id"], "title": v["title"], "channel": v["channel"], "at": NOW - i * HOUR}
                            for i, v in enumerate(FEED[:4])] if state["config"]["history"] else [])
        elif path == "api/resume":
            self.send(200, state["resume"] if state["config"]["resumeDays"] else [])
        elif path == "api/topics":
            refresh = "refresh" in q and state["config"]["topicFeeds"]
            if refresh:
                was_on = True
                time.sleep(float(os.environ.get("QVEIL_STANDIN_SLOW_TOPICS", "0")))
                failing = os.environ.get("QVEIL_STANDIN_TOPICS_FAIL")
                if failing and failing != "group":
                    self.lines([{"error": "The engine's words for a search that failed", "code": "other"}])
                    return
                state["topics"]["fetched"] = True
                state["topics"]["new"] = set()
            if "stream" in q:
                # (A refresh that was running when the setting was turned off
                # still says what it found: the window must not show it.)
                result = topics() if not refresh else {"enabled": True, "groups": topic_groups()}
                # As the engine does when one group's search fails: that one has
                # nothing new and is named in `errors`, and the refresh stops there
                # (the groups after it keep what they had).
                if refresh and os.environ.get("QVEIL_STANDIN_TOPICS_FAIL") == "group":
                    result["groups"][1]["videos"] = []
                    result["groups"][1].pop("fetchedAt", None)
                    result["errors"] = ["borrow checker, allocators: YouTube didn't answer"]
                self.lines([{"progress": {"done": 0, "total": 4}}, {"result": result}], 0.05)
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
        elif path == "api/chapters":
            # Looked up apart for a video whose play said `chaptersLater`, or
            # `detailsLater` (with the channel's name and the upload date).
            found = [] if q.get("v") == "laternone00" else [{"start": s, "title": t} for s, t in CHAPTERS]
            self.send(200, {"chapters": found, "channel": "The page's channel", "published": NOW - 3 * DAY})
        elif path == "api/comments":
            if q.get("v") == "slowslowslo":
                time.sleep(5)
            n = int(q.get("n", "40"))
            found = json.loads(json.dumps(COMMENT_THREADS))
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
            self.stream()
        elif path.startswith("live/") and LIVECLIP:
            self.live(path.split("/")[1:])
        else:
            self.send(404, {"error": "not found", "code": "other"})

    def do_POST(self):
        path, q = self.parts()
        body = self.body()
        base = "http://%s/t" % self.headers.get("Host", "127.0.0.1")
        if path == "api/_die":
            # For the tests of what the window does when the engine is gone.
            os._exit(3)
        if path == "api/_live_end":
            # The running stream ends: its playlists are closed with what they have.
            live.update(final=live_head(), ended=True)
            self.send(200, {})
            return
        if path == "api/_live_fail":
            # The engine can't go on with the running stream, and says why.
            live.update(failed=LIVE_FAILED)
            self.send(200, {})
            return
        if path in ("api/_live_state_hold", "api/_live_state_release"):
            live.update(hold=path == "api/_live_state_hold")
            self.send(200, {})
            return
        if path == "api/_live_drop":
            # The engine no longer has the running stream's session: all of it is 404.
            live.update(sid=live["sid"] + 1000)
            self.send(200, {})
            return
        if path == "api/config":
            cfg = state["config"]
            # What was got ready belongs to the route it was looked up on.
            if body.get("route", cfg["route"]) != cfg["route"]:
                state["prepared"].clear()
            for key in ("route", "history", "thumbnails", "maxHeight", "resumeDays", "topicFeeds", "prepareNext", "keepThumbnails",
                    "liveCheck", "liveChat"):
                if key in body:
                    cfg[key] = body[key]
            # The engine saves the keys it has no setting for as they were,
            # and still says which they are: ignoredKeys stays as it is.
            cfg["resumeDays"] = max(0, min(365, int(cfg["resumeDays"])))
            if cfg["resumeDays"] == 0:
                state["resume"] = []
            days = cfg["resumeDays"]
            cfg["privacy"]["pages"]["resume"] = resume_text(days)
            cfg["privacy"]["pages"]["topics"] = TOPICS_ON if cfg["topicFeeds"] else TOPICS_OFF
            cfg["privacy"]["summary"] = {"direct": SUMMARY_DIRECT,
                                         "tor": SUMMARY_TOR,
                                         "proxy": "Through your proxy: YouTube sees its address, not your IP."}[cfg["route"]]
            self.send(200, cfg)
        elif path == "api/prepare":
            # Getting the next video ready: only while that is on, as the engine does.
            if state["config"]["prepareNext"]:
                # QVEIL_STANDIN_SLOW_PREPARE: seconds the lookup ahead takes.
                time.sleep(float(os.environ.get("QVEIL_STANDIN_SLOW_PREPARE", "0")))
                # As the engine answers: looked up only the first time.
                v = q.get("v", "")
                self.send(200, {"lookedUp": v not in state["prepared"]})
                state["prepared"].add(v)
            else:
                self.send(403, {"error": "getting the next video ready is off", "code": "other"})
        elif path == "api/play":
            v = q.get("v", "")
            # QVEIL_STANDIN_PLAY_FAILS: video IDs that can't be opened, for the tests of what a failed opening leaves behind.
            if v in ("bot_check", "tor_down", "proxy_refused", "rate_limited", "age_restricted", "route_changed") \
                    or v in os.environ.get("QVEIL_STANDIN_PLAY_FAILS", "").split(","):
                self.send(500, {"error": "The engine's words for " + v, "code": v})
                return
            # An announced stream that hasn't started yet: no streams, only when.
            if v.startswith("waiting") and v not in waits["started"]:
                self.send(200, {"id": v, "title": "Questions, answered live", "channel": CHANNELS[0][1],
                                "channelId": CHANNELS[0][0], "description": "Bring your questions.",
                                "waiting": {"starts": wait_starts(v)}})
                return
            if (v.startswith("live") or v in waits["started"]) and LIVECLIP:
                live.update(opened=time.monotonic(), ended=False, failed=None, sid=live["sid"] + 1)
                at = "%s/live/%d/" % (base, live["sid"])
                cid, name = CHANNELS[0]
                self.send(200, {"id": v, "title": "Live: the long one, part 2", "channel": name, "channelId": cid,
                                "video": at + "video.m3u8", "audio": at + "audio.m3u8", "start": -float(LIVE_JOIN * LIVE_SEGMENT),
                                "description": "Questions from chat, and the long one.",
                                "live": {"state": "now", "segment": float(LIVE_SEGMENT), "join": float(LIVE_JOIN * LIVE_SEGMENT),
                                         "started": NOW - 2 * HOUR - 14 * 60 - 5, "rewind": True, "watching": 1200}})
                return
            stream = base + "/s/x/v" if CLIP else ""
            if v.startswith("bare"):
                self.send(200, {"id": v, "title": "A video with nothing but streams", "video": stream})
                return
            # Looked up without the watch page: no channel's name, no age,
            # and its chapters asked for apart (see `api/chapters`).
            if v.startswith("later"):
                self.send(200, {"id": v, "title": "A video looked up without its page", "video": stream,
                                "duration": 2463, "description": DESCRIPTION, "views": 59000, "chaptersLater": True,
                                "detailsLater": True})
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
            # For the tests of a window that quits while the engine is not answering.
            time.sleep(float(os.environ.get("QVEIL_STANDIN_SLOW_RESUME", "0")))
            # As the engine judges it: with resume off nothing is stored, a
            # position it can't judge changes nothing, and any other one replaces
            # what was kept for the video (or, if it isn't worth keeping, removes it).
            days = state["config"]["resumeDays"]
            pos, dur = int(body.get("pos", 0)), int(body.get("dur", 0))
            if days == 0:
                self.send(200, {"noted": "off"})
                return
            if dur <= 0 or pos > dur:
                self.send(200, {"noted": "unknown"})
                return
            state["resume"] = [r for r in state["resume"] if r["id"] != body.get("v")]
            if pos < 30:
                noted = "too_early"
            elif pos + 30 >= dur or pos * 100 >= dur * 95:
                noted = "finished"
            else:
                noted = "kept"
                state["resume"].insert(0, {"id": body.get("v"), "pos": pos, "dur": dur, "at": NOW,
                                           "title": body.get("title", ""), "channel": body.get("channel"),
                                           "expires": NOW + days * DAY})
            self.send(200, {"noted": noted})
        elif path == "api/topics/hide":
            state["topics"]["hidden"].append(q.get("id", ""))
            # Whatever comes in in its place has not been searched for.
            shown = [g["id"] for g in topic_groups()]
            state["topics"]["new"] = set(shown[3:]) if os.environ.get("QVEIL_STANDIN_TOPIC_GROUPS") else set()
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
