#!/bin/bash
# Generates the fixture WAVs (16 kHz mono s16) from expect.json with macOS `say` + ffmpeg. Idempotent.
set -euo pipefail
cd "$(dirname "$0")"
export PATH="/opt/homebrew/bin:/usr/local/bin:$PATH"
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
n=$(jq '.files | length' expect.json)
for i in $(seq 0 $((n - 1))); do
  f() { jq -r ".files[$i].$1 // empty" expect.json; }
  file=$(f file); voice=$(f voice); text=$(f text)
  ff=(ffmpeg -hide_banner -loglevel error -y)
  if [ -n "$(f silence_s)" ]; then
    "${ff[@]}" -f lavfi -i "anullsrc=r=16000:cl=mono" -t "$(f silence_s)" -c:a pcm_s16le "$file"
  else
    say -v "$voice" -o "$tmp/s.aiff" "$text"
    filt=()
    [ -n "$(f pad_to_s)" ] && filt=(-af apad -t "$(f pad_to_s)")
    [ -n "$(f trim_to_s)" ] && filt=(-t "$(f trim_to_s)")
    "${ff[@]}" -i "$tmp/s.aiff" ${filt[@]+"${filt[@]}"} -ar 16000 -ac 1 -c:a pcm_s16le "$file"
  fi
  printf '%-18s %6.2fs  %s\n' "$file" "$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$file")" "$voice"
done
