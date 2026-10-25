#!/usr/bin/env pwsh
<#
.SYNOPSIS
    Runs the loopback harness: the sender captures the cable, a tone feeds it, and a
    mock receiver on 127.0.0.1 checks what arrives.

.DESCRIPTION
    Nothing is sent to a real receiver and nothing is audible — but only if the
    daemon is not running. The daemon captures the same endpoint, and if it is up it
    will forward the test tone to the real device. This script therefore refuses to
    run while the daemon is alive, and it flushes the cable with silence afterwards:
    VB-CABLE hands its last buffer to a fresh capture client, so without the flush the
    next daemon start plays a burst of whatever was last rendered.

.EXAMPLE
    pwsh tools/loopback-test.ps1
    pwsh tools/loopback-test.ps1 -Seconds 30 -Port 5000
#>
[CmdletBinding()]
param(
    [int]$Seconds = 20,
    [int]$Port = 5000,
    [string]$CaptureDevice = "CABLE Input",
    [string]$Python = "python",
    [string]$Sender = ""
)

$ErrorActionPreference = "Stop"
$repo = Split-Path -Parent $PSScriptRoot
if (-not $Sender) {
    $Sender = Join-Path $repo "target\release\raop-sender.exe"
}
$mock = Join-Path $PSScriptRoot "mock-receiver.py"
$feed = Join-Path $repo "target\release\examples\tonefeed.exe"

foreach ($path in @($mock, $Sender, $feed)) {
    if (-not (Test-Path $path)) {
        throw "missing $path — build first: cargo build --release; cargo build --release --example tonefeed"
    }
}

# Safety: the daemon must not be running, or the tone goes to the real receiver.
$daemon = Get-Process -Name "raop-daemon" -ErrorAction SilentlyContinue
if ($daemon) {
    throw "the daemon is running (pid $($daemon.Id)). Stop it first:
    Stop-ScheduledTask -TaskName 'AirPlay Sender'
The test tone would otherwise be streamed to the real receiver."
}

$mockLog = Join-Path $env:TEMP "raop-mock.log"
$senderLog = Join-Path $env:TEMP "raop-sender.log"

Write-Host "mock receiver on 127.0.0.1:$Port for $($Seconds + 12)s"
$mockProc = Start-Process -FilePath $Python `
    -ArgumentList @($mock, "--port", $Port, "--seconds", ($Seconds + 12)) `
    -RedirectStandardOutput $mockLog -PassThru -WindowStyle Hidden
Start-Sleep -Seconds 2

Write-Host "tone into '$CaptureDevice' for $Seconds s"
$feedProc = Start-Process -FilePath $feed `
    -ArgumentList @("--device", $CaptureDevice, "--seconds", $Seconds) `
    -RedirectStandardOutput (Join-Path $env:TEMP "raop-tone.log") -PassThru -WindowStyle Hidden
Start-Sleep -Seconds 1

Write-Host "sender streaming for $Seconds s"
$senderProc = Start-Process -FilePath $Sender `
    -ArgumentList @("--play", "--system", "--host", "127.0.0.1:$Port") `
    -RedirectStandardOutput $senderLog -PassThru -WindowStyle Hidden

Start-Sleep -Seconds ($Seconds + 2)

foreach ($proc in @($senderProc, $feedProc)) {
    if ($proc -and -not $proc.HasExited) { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue }
}
$mockProc.WaitForExit(20000) | Out-Null
if (-not $mockProc.HasExited) { Stop-Process -Id $mockProc.Id -Force -ErrorAction SilentlyContinue }

$stats = Get-Content $senderLog -ErrorAction SilentlyContinue
$underruns = ($stats | Select-String "underruns" | ForEach-Object {
    if ($_ -match "(\d+) underruns") { [int]$Matches[1] } }) | Measure-Object -Maximum
Write-Host ""
Write-Host "--- mock receiver ---"
Get-Content $mockLog -ErrorAction SilentlyContinue | Select-String "summary:|integrity:|PASS|FAIL"
Write-Host "--- sender ---"
Write-Host ("worst underruns seen: {0}" -f ($(if ($underruns.Maximum) { $underruns.Maximum } else { 0 })))

# Flush the tone out of the cable buffer so the next daemon start is silent.
Write-Host "flushing the cable with silence"
$flush = Start-Process -FilePath $feed `
    -ArgumentList @("--device", $CaptureDevice, "--seconds", 2, "--amplitude", 0) `
    -PassThru -WindowStyle Hidden
$flush.WaitForExit(10000) | Out-Null

Write-Host ""
Write-Host "Done. Start the daemon again with: Start-ScheduledTask -TaskName 'AirPlay Sender'"
