#!/bin/sh
# A stand-in for yt-dlp that answers the engine from canned JSON (see
# engine_tests.cpp). The engine puts what it wants fetched (a URL or a search)
# as the last line of the config file named by --config-locations.
config=
while [ $# -gt 0 ]; do
  [ "$1" = --config-locations ] && config="$2"
  shift
done
target=$(tail -n 1 "$config")
case "$target" in
*ytsearch*)
  echo '{"id":"SSSSSSSSSS1","title":"First result","channel":"Alpha","channel_id":"UCaaaaaaaaaaaaaaaaaaaaaa","view_count":1500,"duration":61,"timestamp":1700000000}'
  echo '{"id":"SSSSSSSSSS2","title":"Second result","channel":"Beta","channel_id":"UCbbbbbbbbbbbbbbbbbbbbbb","view_count":20}'
  ;;
*UCaaaaaaaaaaaaaaaaaaaaaa*)
  echo '{"entries":[{"id":"AAAAAAAAAA2","title":"Alpha, newer","timestamp":1700004000},{"id":"AAAAAAAAAA1","title":"Alpha, older","timestamp":1700001000}]}'
  ;;
*UCbbbbbbbbbbbbbbbbbbbbbb*)
  echo '{"entries":[{"id":"BBBBBBBBBB2","title":"Beta, newer","timestamp":1700005000},{"id":"BBBBBBBBBB1","title":"Beta, older","timestamp":1700002000}]}'
  ;;
*watch*)
  echo '{"title":"Canned video","channel":"Alpha","channel_id":"UCaaaaaaaaaaaaaaaaaaaaaa","duration":61,"requested_formats":[{"url":"https://upstream.invalid/video","protocol":"https","vcodec":"avc1"},{"url":"https://upstream.invalid/audio","protocol":"https","vcodec":"none"}]}'
  ;;
*)
  echo "unexpected target: $target" >&2
  exit 1
  ;;
esac
