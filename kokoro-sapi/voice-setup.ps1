# Registers/unregisters the x86 SAPI engine for the installer, self-elevating.
#
# The reader app installs per-user (currentUser NSIS), so the installer runs
# UNELEVATED -- but registering the COM server writes HKLM (regsvr32 ->
# WOW6432Node) and the Kindle voice guard does `reg load`, both of which need
# admin. So this script relaunches itself through UAC when it isn't already
# elevated, then does the privileged work. The installer hooks just invoke it
# (see installer-hooks.nsh); it raises one UAC prompt per install/uninstall.
#
#   -Action register     regsvr32 the DLL, then point Kindle's default at Kokoro
#   -Action unregister    revert Kindle to David, then regsvr32 /u the DLL
#
# -ResourcesDir is the bundle's resources\ dir (holds KokoroSapi.dll and
# kindle-voice-guard.ps1); it's passed through the elevation so the admin
# instance finds the files regardless of whose profile UAC lands in.
param(
    [Parameter(Mandatory)][ValidateSet('register', 'unregister')][string]$Action,
    [Parameter(Mandatory)][string]$ResourcesDir
)
$ErrorActionPreference = 'Stop'
$log = Join-Path $env:TEMP 'kokoro-voice-setup.log'
function L($m) { "[{0}] {1}" -f (Get-Date -Format HH:mm:ss), $m | Tee-Object -FilePath $log -Append | Out-Null }

$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) {
    L "not elevated; relaunching via UAC ($Action)"
    Start-Process -Verb RunAs -Wait -FilePath 'powershell' -ArgumentList @(
        '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $PSCommandPath,
        '-Action', $Action, '-ResourcesDir', $ResourcesDir
    )
    exit 0
}

$regsvr = Join-Path $env:WINDIR 'SysWOW64\regsvr32.exe'   # 32-bit: Kindle is x86
$dll = Join-Path $ResourcesDir 'KokoroSapi.dll'
$guard = Join-Path $ResourcesDir 'kindle-voice-guard.ps1'

if ($Action -eq 'register') {
    L "regsvr32 register $dll"
    & $regsvr '/s' $dll
    # Make Kokoro Kindle's default now that the KokoroTTS token exists. Self-skips
    # if Kindle's hive is absent (Kindle not installed).
    & $guard -Set kokoro
} else {
    # Revert Kindle to Microsoft David BEFORE deleting the token, so its hive isn't
    # left pointing DefaultTokenId at a gone KokoroTTS token.
    & $guard -Set david
    L "regsvr32 unregister $dll"
    & $regsvr '/u' '/s' $dll
}
L "done ($Action)"
