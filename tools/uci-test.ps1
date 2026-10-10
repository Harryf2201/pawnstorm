# Sends UCI commands to the engine with a live stdin and prints its output.
# Usage: .\tools\uci-test.ps1 "position startpos" "go movetime 2000"
param([string[]]$Commands, [int]$WaitMs = 3000, [string]$Engine = "$PSScriptRoot\..\target\release\pawnstorm.exe")

$psi = New-Object System.Diagnostics.ProcessStartInfo $Engine
$psi.RedirectStandardInput = $true
$psi.RedirectStandardOutput = $true
$psi.UseShellExecute = $false
$p = [System.Diagnostics.Process]::Start($psi)
$out = New-Object System.Text.StringBuilder
$handler = { if ($EventArgs.Data -ne $null) { [void]$Event.MessageData.AppendLine($EventArgs.Data) } }
$job = Register-ObjectEvent -InputObject $p -EventName OutputDataReceived -Action $handler -MessageData $out
$p.BeginOutputReadLine()
foreach ($c in $Commands) { $p.StandardInput.WriteLine($c) }
Start-Sleep -Milliseconds $WaitMs
$p.StandardInput.WriteLine("quit")
$p.WaitForExit(5000) | Out-Null
Start-Sleep -Milliseconds 200
Unregister-Event -SourceIdentifier $job.Name
$out.ToString()
