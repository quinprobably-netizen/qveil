#!/bin/sh
# A stand-in for yt-dlp that answers the engine from canned JSON (see
# engine_tests.cpp). The engine puts the call's own options in the config
# file named by --config-locations, one to a line, and what it wants fetched
# (a URL or a search) as its last line.
#
# The tests steer it with files next to the script (the test copies the script
# into a directory of its own, so every engine has its own steering):
#   calls            every run is appended: "<kind> <target>", kind being
#                    resolve, comments, search, info (a channel's name) or channel
#   <kind>-delay     seconds to wait before answering that kind
#   <kind>-delay-once  the same, for the next run only
#   <kind>-error     a line to fail with on stderr instead of answering
here=$(dirname "$0")
config=
while [ $# -gt 0 ]; do
  [ "$1" = --config-locations ] && config="$2"
  shift
done
range=$(sed -n "s/^-I '\(.*\)'\$/\1/p" "$config")
comments=
grep -qx -- --write-comments "$config" && comments=1
target=$(tail -n 1 "$config")
case "$target" in
*ytsearch* | *results\?*) kind=search ;;
*watch*) if [ -n "$comments" ]; then kind=comments; else kind=resolve; fi ;;
*) if [ "$range" = 0 ]; then kind=info; else kind=channel; fi ;;
esac
echo "$kind $target" >>"$here/calls"
delay=
if [ -f "$here/$kind-delay-once" ]; then
  delay=$(cat "$here/$kind-delay-once")
  rm -f "$here/$kind-delay-once"
elif [ -f "$here/$kind-delay" ]; then
  delay=$(cat "$here/$kind-delay")
fi
if [ -n "$delay" ]; then
  # Not in the foreground: a sleep that outlives the script would hold the
  # engine's pipes open after the engine has killed the script.
  sleep "$delay" >/dev/null 2>&1 </dev/null &
  wait $!
fi
if [ -f "$here/$kind-error" ]; then
  cat "$here/$kind-error" >&2
  exit 1
fi
cid=$(printf '%s' "$target" | grep -o 'UC[A-Za-z0-9_-]\{22\}' | head -n 1)
# A handle is the third channel's.
case "$target" in */@*) cid=UCcccccccccccccccccccccc ;; esac
case "$kind" in
search)
  echo '{"id":"SSSSSSSSSS1","title":"First result","channel":"Alpha","channel_id":"UCaaaaaaaaaaaaaaaaaaaaaa","view_count":1500,"duration":61,"timestamp":1700000000}'
  echo '{"id":"SSSSSSSSSS2","title":"Second result","channel":"Beta","channel_id":"UCbbbbbbbbbbbbbbbbbbbbbb","view_count":20}'
  ;;
info)
  echo "{\"id\":\"$cid\",\"channel_id\":\"$cid\",\"channel\":\"Named $cid\"}"
  ;;
channel)
  case "$cid" in
  UCaaaaaaaaaaaaaaaaaaaaaa)
    # Each of the two has a Short, which the feed leaves out unless asked.
    echo '{"entries":[{"id":"AAAAAAAAAA2","title":"Alpha, newer","timestamp":1700004000},{"id":"ASHORTSSSS1","title":"Alpha, short","url":"https://www.youtube.com/shorts/ASHORTSSSS1","timestamp":1700003000},{"id":"AAAAAAAAAA1","title":"Alpha, older","timestamp":1700001000}]}'
    ;;
  UCbbbbbbbbbbbbbbbbbbbbbb)
    echo '{"entries":[{"id":"BBBBBBBBBB2","title":"Beta, newer","timestamp":1700005000},{"id":"BSHORTSSSS1","title":"Beta, short","url":"https://www.youtube.com/shorts/BSHORTSSSS1","timestamp":1700002500},{"id":"BBBBBBBBBB1","title":"Beta, older","timestamp":1700002000}]}'
    ;;
  UC*)
    # Any other channel has one video, whose ID is made from the channel's.
    echo "{\"entries\":[{\"id\":\"v$(printf '%s' "$cid" | cut -c3-12)\",\"title\":\"A video of $cid\",\"timestamp\":1700006000}]}"
    ;;
  *)
    echo "unexpected target: $target" >&2
    exit 1
    ;;
  esac
  ;;
comments)
  # Three threads, the first pinned by the uploader; the second has a reply,
  # sent only when replies were asked for (the last number of max_comments).
  reply=
  if ! grep -q "max_comments=[0-9]*,[0-9]*,0,0" "$config"; then
    reply=',{"id":"c2.1","parent":"c2","author":"@gamma","text":"Agreed"}'
  fi
  echo "{\"comment_count\":1204,\"comments\":[{\"id\":\"c1\",\"parent\":\"root\",\"author\":\"@alpha\",\"text\":\"Correction at 0:10\",\"like_count\":640,\"_time_text\":\"9 hours ago\",\"is_pinned\":true,\"author_is_uploader\":true},{\"id\":\"c2\",\"parent\":\"root\",\"author\":\"@beta\",\"text\":\"Nice\",\"like_count\":12,\"_time_text\":\"6 hours ago\"}$reply,{\"id\":\"c3\",\"parent\":\"root\",\"author\":\"@delta\",\"text\":\"See https://example.com/x\",\"_time_text\":\"1 hour ago\"}]}"
  ;;
resolve)
  # The streams are on a port where nothing listens: the engine fetches
  # their start as soon as a video is opened, and that must fail at once,
  # without even a DNS lookup.
  echo '{"title":"Canned video","channel":"Alpha","channel_id":"UCaaaaaaaaaaaaaaaaaaaaaa","duration":61,"description":"About this video.\\n0:00 Intro","view_count":59000,"timestamp":1762700000,"chapters":[{"start_time":0,"title":"Intro"},{"start_time":60,"title":"Middle"}],"requested_formats":[{"url":"https://127.0.0.1:1/video","protocol":"https","vcodec":"avc1"},{"url":"https://127.0.0.1:1/audio","protocol":"https","vcodec":"none"}]}'
  ;;
esac
