# Moves the Lichess bot between this PC and the Oracle server (only one may be connected at a time).
# Usage: .\tools\bot-host.ps1 pc       PC plays (stronger: 3 games x 6 threads); the server bot stops
#        .\tools\bot-host.ps1 oracle   server plays (1 game x 4 threads, always on); the PC bot stops
#        .\tools\bot-host.ps1          show where it's running
# Games in progress carry over: lichess-bot picks up ongoing games when it starts.
# The server also takes over by itself if the bot has been offline for a few minutes
# (~/bot-failover.sh, run by a systemd timer), so a PC switched off without this still hands over.
param([ValidateSet('pc', 'oracle', '')][string]$To = '')

. "$PSScriptRoot\paths.ps1"
$bot = Join-Path $Parent 'lichess-bot'
$ssh = @('-i', $OracleKey, '-o', 'BatchMode=yes', '-o', 'ConnectTimeout=20')

function Remote([string]$cmd) {
    $psi = [Diagnostics.ProcessStartInfo]::new("$env:WINDIR\System32\OpenSSH\ssh.exe")
    $psi.Arguments = (($ssh + $OracleHost + $cmd) | ForEach-Object { if ($_ -match '\s') { "`"$_`"" } else { $_ } }) -join ' '
    $psi.UseShellExecute = $false; $psi.RedirectStandardInput = $true; $psi.RedirectStandardOutput = $true; $psi.CreateNoWindow = $true
    $p = [Diagnostics.Process]::Start($psi); $p.StandardInput.Close()
    $out = $p.StandardOutput.ReadToEnd()
    if (-not $p.WaitForExit(60000)) { $p.Kill() }
    $out.Trim()
}
function Stop-PcBot {
    # lichess-bot runs several python.exe workers; all must go (nothing else here uses Python).
    Get-Process python -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    Start-Sleep 2
}

$pcOn = [bool](Get-Process python -ErrorAction SilentlyContinue)
switch ($To) {
    'pc' {
        "  server bot: $(Remote 'sudo systemctl stop lichess-bot; systemctl is-active lichess-bot')"
        Stop-PcBot
        Start-Process "$bot\venv\Scripts\python.exe" -ArgumentList 'lichess-bot.py', '-v', '--logfile', 'bot.log' -WorkingDirectory $bot -WindowStyle Hidden
        "  PC bot started"
    }
    'oracle' {
        if ($pcOn) { Stop-PcBot; "  PC bot stopped" }
        "  server bot: $(Remote 'sudo systemctl start lichess-bot; systemctl is-active lichess-bot')"
    }
    default {
        "  PC bot:     $(if ($pcOn) { 'running' } else { 'off' })"
        "  server bot: $(Remote 'systemctl is-active lichess-bot')"
    }
}
