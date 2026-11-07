#!/bin/sh
# A stand-in for yt-dlp that answers the engine from canned JSON (see
# engine_tests.cpp). The engine puts what it wants fetched (a URL or a search)
# as the last line of the config file named by --config-locations.
#
# The tests steer it with files next to the script (the test copies the script
# into a directory of its own, so every engine has its own steering):
#   calls            every run is appended: "<kind> <target>", kind being
#                    resolve, search, info (a channel's name) or channel
#   <kind>-delay     seconds to wait before answering that kind
#   <kind>-delay-once  the same, for the next run only
#   <kind>-error     a line to fail with on stderr instead of answering
here=$(dirname "$0")
config=
range=
while [ $# -gt 0 ]; do
  case "$1" in
  --config-locations) config="$2" ;;
  -I) range="$2" ;;
  esac
  shift
done
target=$(tail -n 1 "$config")
case "$target" in
*ytsearch*) kind=search ;;
*watch*) kind=resolve ;;
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
resolve)
  echo '{"title":"Canned video","channel":"Alpha","channel_id":"UCaaaaaaaaaaaaaaaaaaaaaa","duration":61,"requested_formats":[{"url":"https://upstream.invalid/video","protocol":"https","vcodec":"avc1"},{"url":"https://upstream.invalid/audio","protocol":"https","vcodec":"none"}]}'
  ;;
esac
