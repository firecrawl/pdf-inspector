#!/bin/bash
# OCR probe: run pdf2md --ocr in the background, watch RSS/CPU, report if it stalls.
# usage: ocr_probe.sh <label> <dpi> <pages> [rayon_threads]
label="$1"; dpi="$2"; pages="$3"; rt="${4:-}"
log="/tmp/ocrprobe-$label.log"
[ -n "$rt" ] && export RAYON_NUM_THREADS="$rt"
echo "run: dpi=$dpi pages=$pages rayon=${rt:-default} cores=12"
"$PWD/target/release/pdf2md" \
  "/Users/aaron/Desktop/Ivan/Key Words For Fluency/Key Words for Fluency Pre-Inter - George Woolard.pdf" \
  "/tmp/ocrprobe-$label.md" --profile book --ocr force --select-pages "$pages" --ocr-dpi "$dpi" \
  >"$log" 2>&1 &
pid=$!
peak=0; stall=0; last_cpu=-1
for i in $(seq 1 60); do
  sleep 5
  if ! ps -p "$pid" >/dev/null 2>&1; then
    echo "== finished after ~$((i*5))s"; break
  fi
  read rss cpu <<<"$(ps -o rss=,%cpu= -p "$pid")"
  [ "$rss" -gt "$peak" ] && peak=$rss
  if [ "${cpu%.*}" = "0" ]; then stall=$((stall+1)); else stall=0; fi
  echo "t=$((i*5))s rss=$((rss/1024))M cpu=$cpu peak=$((peak/1024))M"
  if [ "$stall" -ge 4 ]; then
    echo "== STALLED (0% CPU for 20s) at t=$((i*5))s, rss=$((rss/1024))M -> killing $pid"
    kill -KILL "$pid"; break
  fi
done
if ps -p "$pid" >/dev/null 2>&1; then kill -KILL "$pid"; fi
wait "$pid" 2>/dev/null; echo "exit=$?"
echo "--- log tail:"; tail -6 "$log"; echo "--- md size:"; wc -c "/tmp/ocrprobe-$label.md" 2>/dev/null
