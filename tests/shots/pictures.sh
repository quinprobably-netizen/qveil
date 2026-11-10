#!/bin/sh
# Makes twelve pictures to stand in for thumbnails in stills, into the
# directory given: something with the look of a thumbnail (a ground of
# colour, big words), since a still of a video site with no pictures says
# little about how it will look. Needs ImageMagick.
set -eu
out="${1:?usage: pictures.sh <directory>}"
mkdir -p "$out"
cd "$out"
font=$(fc-match -f '%{file}' 'sans:bold')
mk() {
  magick -size 640x360 -seed "$1" plasma:"$2"-"$3" -blur 0x9 -modulate 100,115 \
    \( -size 640x360 radial-gradient:none-'#000000aa' \) -composite \
    \( -size 640x360 xc:none -fill "$4" -draw "rectangle 0,300 640,306" \) -composite \
    -font "$font" -pointsize 62 -fill white -stroke '#000000cc' -strokewidth 3 -gravity northwest \
    -annotate +30+150 "$5" -stroke none -annotate +30+150 "$5" \
    -pointsize 40 -fill "$4" -stroke '#000000cc' -strokewidth 3 -annotate +30+226 "$6" -stroke none -annotate +30+226 "$6" \
    "$1.png"
}
mk 1 '#101b3d' '#5a3fb0' '#ffb347' 'BORROW CHECKER' 'says no. here is why'
mk 2 '#3b2412' '#c9843a' '#ffe2a8' 'DOVETAIL BOX' '4 hand tools only'
mk 3 '#0f2d2a' '#27a08e' '#ff7aa5' '1970s SYNTH' 'will not stay in tune'
mk 4 '#2a1838' '#a04c8e' '#9be7c4' '36 TILES' 'one kiln load'
mk 5 '#16324f' '#6fb0e0' '#ffd166' 'PAMIR' 'part 3 · steel bike'
mk 6 '#101014' '#56609a' '#7ae582' 'ALLOCATORS' 'from scratch'
mk 7 '#2e1f14' '#a9703a' '#f4d58d' 'SHARP' 'a plane iron, properly'
mk 8 '#231942' '#7c6fc0' '#ffcad4' 'TAPE LOOPS' 'no tape machine'
mk 9 '#3a0f0f' '#d24a5a' '#fcb9b2' 'RAKU' 'firing in the rain'
mk 10 '#0b132b' '#2c4a80' '#5bc0be' 'LINKERS' 'what they actually do'
mk 11 '#283618' '#7c8a48' '#fefae0' 'CHEAP BENCH' 'that works'
mk 12 '#14213d' '#4d74a8' '#fca311' 'PACKING' 'for six months'
