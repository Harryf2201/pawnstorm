# Always-on data generation server

Self-play (currently gen4, see nets/default.gen) on an Oracle Cloud Always Free ARM server (VM.Standard.A1.Flex, 4 OCPUs, 24 GB, Ubuntu 24.04).

Setup on the server (as `ubuntu`):

    sudo apt-get install -y build-essential git
    curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
    git clone https://github.com/Harryf2201/pawnstorm.git ~/pawnstorm
    cd ~/pawnstorm && cargo build --release --bin pawnstorm
    mkdir -p ~/work ~/outbox && cp tools/oracle/run-datagen.sh ~/ && chmod +x ~/run-datagen.sh
    sudo cp tools/oracle/pawnstorm-datagen.service /etc/systemd/system/
    sudo systemctl enable --now pawnstorm-datagen

On the PC, set `$OracleHost` and `$OracleKey` in `tools\engines.local.ps1` (not committed);
`tools\oracle-sync.ps1` (started by lab-start) collects the finished 1-hour segments.
To switch the network it generates with: commit a new `nets/default.bin`, then on the server
`cd ~/pawnstorm && git pull && ~/.cargo/bin/cargo build --release --bin pawnstorm && sudo systemctl restart pawnstorm-datagen`
(the full cargo path matters over a non-interactive ssh), and change oracle-sync.ps1's -Name to the new generation's folder.
