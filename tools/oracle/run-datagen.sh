#!/bin/bash
# Pawnstorm gen2 self-play on this server, in 1-hour segments. Each finished segment is gzipped and
# moved to ~/outbox/seg-<time>/ (data_*.txt.gz + status.txt); the PC collects and deletes them.
# A segment interrupted by a reboot is discarded (at most an hour of data).
rm -rf ~/work/seg-*
while true; do
  ts=$(date -u +%Y%m%d-%H%M%S)
  d=~/work/seg-$ts
  mkdir -p "$d"
  ~/pawnstorm/target/release/pawnstorm datagen 4 1000000000 5000 "$d" 3600 > "$d/stdout.txt" 2>&1
  gzip "$d"/data_*.txt
  mv "$d" ~/outbox/seg-$ts
done
