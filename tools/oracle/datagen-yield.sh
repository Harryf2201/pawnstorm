#!/bin/bash
# Gives the Lichess bot the whole server during a game: while any bot engine is running (lichess-bot starts one per
# game and quits it when the game ends) the self-play processes are paused (SIGSTOP); between games they carry on.
# Run by datagen-yield.service. On exit, anything paused is resumed.
paused=""
resume() { [ -n "$paused" ] && pkill -CONT -f "^/home/ubuntu/pawnstorm/target/release/pawnstorm datagen "; paused=""; }
trap 'resume; exit 0' EXIT INT TERM
while true; do
  if pgrep -f "^[^ ]*lichess-bot/engines/pawnstorm$" >/dev/null; then
    # checked every loop: a new 1-hour segment starts a new datagen process
    pkill -STOP -f "^/home/ubuntu/pawnstorm/target/release/pawnstorm datagen " && [ -z "$paused" ] && { paused=1; logger -t datagen-yield "game on: datagen paused"; }
  elif [ -n "$paused" ]; then
    resume; logger -t datagen-yield "no game: datagen resumed"
  fi
  sleep 1
done
