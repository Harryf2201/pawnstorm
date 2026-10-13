#!/bin/bash
# Starts the server's Lichess bot when the account has been offline for two checks 90 s apart, i.e. the PC
# went off without handing over (tools/bot-host.ps1 oracle). Run every 2 minutes by bot-failover.timer.
# The account name is in ~/lichess-bot/bot-name (kept out of the repo).
systemctl is-active -q lichess-bot && exit 0
name=$(cat ~ubuntu/lichess-bot/bot-name)
offline() {
  # Only a valid answer saying "not online" counts; if Lichess can't be reached, do nothing.
  r=$(curl -s -m 15 "https://lichess.org/api/users/status?ids=$name")
  echo "$r" | grep -qi "\"id\":\"${name,,}\"" && ! echo "$r" | grep -q '"online":true'
}
offline || exit 0
sleep 90
systemctl is-active -q lichess-bot && exit 0
offline || exit 0
logger -t bot-failover "bot offline for 90 s: starting the server's lichess-bot"
systemctl start lichess-bot
