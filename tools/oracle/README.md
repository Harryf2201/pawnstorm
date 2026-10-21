# Always-on data generation server

Self-play (currently gen4, see nets/default.gen) on an Oracle Cloud Always Free ARM server (VM.Standard.A1.Flex, 4 OCPUs, 24 GB, Ubuntu 24.04).

Setup on the server (as `ubuntu`):

    sudo apt-get install -y build-essential git
    curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
    git clone https://github.com/Harryf2201/pyther.git ~/pawnstorm
    cd ~/pawnstorm && cargo build --release --bin pawnstorm
    mkdir -p ~/work ~/outbox && cp tools/oracle/run-datagen.sh ~/ && chmod +x ~/run-datagen.sh
    sudo cp tools/oracle/pawnstorm-datagen.service /etc/systemd/system/
    sudo systemctl enable --now pawnstorm-datagen

On the PC, set `$OracleHost` and `$OracleKey` in `tools\engines.local.ps1` (not committed);
`tools\oracle-sync.ps1` (started by lab-start) collects the finished 1-hour segments.
To switch the network it generates with: commit a new `nets/default.bin`, then on the server
`cd ~/pawnstorm && git pull && ~/.cargo/bin/cargo build --release --bin pawnstorm && sudo systemctl restart pawnstorm-datagen`
(the full cargo path matters over a non-interactive ssh), and change oracle-sync.ps1's -Name to the new generation's folder.

## Lichess bot

The server can also run the Lichess bot while the PC is off (1 game at a time, 15 threads, Hash 2048). `datagen-yield.service`
(`datagen-yield.sh`) pauses self-play (SIGSTOP) while a bot engine process exists, i.e. during a game, and resumes it
between games, so the game gets every core (low priority alone, Nice=19 + SCHED_IDLE via a systemd drop-in, still
halved the search speed). With fewer cores (4 OCPU after Nov 8) set Threads in config.yml to nproc - 1.

- `~/pawnstorm-bot`: a checkout of the branch the bot plays (built with `~/.cargo/bin/cargo build --release --bin pawnstorm`),
  linked as `~/lichess-bot/engines/pawnstorm`; network in `~/lichess-bot/engines/`, Syzygy 3-4-5 in `engines/syzygy`.
- `~/lichess-bot`: lichess-bot with a venv; `config.yml` (holds the token, chmod 600, never committed) is the PC's
  with server paths, Threads 15, Hash 2048, concurrency 1. The account name is in `~/lichess-bot/bot-name`.
- `lichess-bot.service` (not enabled at boot). `tools\bot-host.ps1 pc|oracle` on the PC moves the bot between the two;
  `lab-stop.ps1` hands it to the server. `bot-failover.timer` (enabled) starts it when the account has been offline
  for 90 s, e.g. the PC was switched off without lab-stop.
