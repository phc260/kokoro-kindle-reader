# Gates Kokoro TTS in Kindle on the reader app's lifetime.
#
# Kindle (MSIX) takes its SAPI default from its package hive (Helium\User.dat),
# not HKCU, and only re-reads it on launch -- so switching the voice means
# editing that hive with Kindle stopped, then relaunching it. Must run elevated
# (reg load needs admin).
#
# Two modes:
#   -Set kokoro|david        one-shot: switch Kindle's voice now and exit
#   -AppPid <pid>            guard: switch to Kokoro, wait for that process to
#                            exit (or die), then switch back to David
#
# The reader app launches the guard at startup (see lib.rs); waiting on the PID
# means a crash still restores David.
param(
    [int]$AppPid = 0,
    [ValidateSet('kokoro', 'david')][string]$Set,
    [string]$StartVoice = 'kokoro',
    [string]$EndVoice   = 'david'
)
$ErrorActionPreference = 'Stop'
$log = Join-Path $env:TEMP 'kindle-voice-guard.log'
function L($m) { "[{0}] {1}" -f (Get-Date -Format HH:mm:ss), $m | Tee-Object -FilePath $log -Append | Out-Null }

$tokens = @{
    kokoro = 'HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\Speech\Voices\Tokens\KokoroTTS'
    david  = 'HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\Speech\Voices\Tokens\TTS_MS_EN-US_DAVID_11.0'
}

function Set-KindleVoice([string]$which) {
    $token = $tokens[$which]
    if (-not $token) { throw "unknown voice '$which'" }

    $hive = @(Get-ChildItem "$env:LOCALAPPDATA\Packages" -Directory -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -like '*AMZNKindle*' } |
        ForEach-Object { Join-Path $_.FullName 'SystemAppData\Helium\User.dat' } |
        Where-Object { Test-Path $_ })
    if (-not $hive.Count) { L "hive not found; skipping"; return }
    $hive = $hive[0]

    $wasRunning = [bool](Get-Process Kindle -ErrorAction SilentlyContinue)
    if ($wasRunning) {
        L "stopping Kindle to edit hive"
        Stop-Process -Name Kindle -Force -Confirm:$false
        Start-Sleep -Seconds 2
    }

    $mount = 'HKLM\KindleVoiceGuard'
    reg load $mount "$hive" | Out-Null
    try {
        reg add "$mount\SOFTWARE\Microsoft\Speech\Voices" /v DefaultTokenId /t REG_SZ /d $token /f | Out-Null
        L "DefaultTokenId -> $which"
    } finally {
        [gc]::Collect(); Start-Sleep -Milliseconds 400
        reg unload $mount | Out-Null
    }

    # NOTE: do NOT relaunch Kindle from here -- this script runs elevated, and
    # starting an MSIX app from an elevated process yields a broken/silent
    # session. Kindle only re-reads the voice on launch, so the user must
    # reopen it normally (Start menu) to pick up $which.
    if ($wasRunning) { L "Kindle was stopped to edit the hive; reopen it to apply '$which'" }
}

if ($Set) {
    L "one-shot: $Set"
    Set-KindleVoice $Set
    exit 0
}

if ($AppPid -le 0) { throw "guard mode needs -AppPid <pid> (or use -Set kokoro|david)" }

L "guard start (AppPid=$AppPid)"
Set-KindleVoice $StartVoice
try { Wait-Process -Id $AppPid -ErrorAction SilentlyContinue } catch {}
L "app $AppPid exited; restoring"
Set-KindleVoice $EndVoice
L "guard done"
