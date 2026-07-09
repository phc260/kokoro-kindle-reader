# Registers/unregisters the x86 SAPI engine for the installer, self-elevating.
#
# The reader app installs per-user (currentUser NSIS), so the installer runs
# UNELEVATED -- but registering the COM server writes HKLM (regsvr32 ->
# WOW6432Node) and the Kindle voice guard does `reg load`, both of which need
# admin. So this script relaunches itself through UAC when it isn't already
# elevated, then does the privileged work. The installer hooks just invoke it
# (see installer.nsi); it raises one UAC prompt per install/uninstall.
#
# SECURITY (local EoP): the two artifacts that run ELEVATED here -- KokoroSapi.dll
# (regsvr32 calls its DllRegisterServer) and kindle-voice-guard.ps1 -- must NOT be
# executed from a user-writable directory. The bundle's resources\ dir lives under
# %LOCALAPPDATA% (writable by the -- possibly lower-integrity -- user), so a same-user
# process could swap either file and have its code run as admin at the next
# install/uninstall. To close that, on register we copy both into an admin-owned,
# ACL-locked dir under %ProgramData% and register/run THOSE copies; Kindle then also
# loads the locked DLL. Removed again on unregister. (Fully closing first-install
# tampering of the resources\ SOURCE needs a signed installer; this removes the
# persistent user-writable elevated-artifact window, which is the exploitable part.)
#
#   -Action register     regsvr32 the DLL, then point Kindle's default at Kokoro
#   -Action unregister    revert Kindle to David, then regsvr32 /u the DLL
#
# -ResourcesDir is the bundle's resources\ dir -- the SOURCE of the secured copies,
# and the fallback for unregistering an older install that registered it directly.
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

# Admin-owned, non-user-writable home for the elevated-executed copies. Well-known
# SIDs keep the ACL locale-independent: SYSTEM (S-1-5-18) + Administrators
# (S-1-5-32-544) get Full; Users (S-1-5-32-545) get Read/Execute so Kindle can still
# load the DLL as the user; no one else can write.
$secureRoot = Join-Path $env:ProgramData 'Kokoro Kindle Reader'
$secure     = Join-Path $secureRoot 'engine'

# Force admin-group ownership of an existing tree so we can always rebuild/remove it,
# even one a lower-privileged user pre-created to squat the path.
function Reset-Owner([string]$path) {
    if (Test-Path $path) { & icacls $path /setowner '*S-1-5-32-544' /T /C /Q | Out-Null }
}

if ($Action -eq 'register') {
    # Rebuild from scratch each time: reclaim ownership of any pre-existing (possibly
    # squatted) dir, wipe it, then recreate and lock BEFORE trusting anything in it.
    Reset-Owner $secure
    if (Test-Path $secure) { Remove-Item $secure -Recurse -Force }
    New-Item -ItemType Directory -Path $secure -Force | Out-Null

    & icacls $secure /inheritance:r /grant:r '*S-1-5-18:(OI)(CI)(F)' '*S-1-5-32-544:(OI)(CI)(F)' '*S-1-5-32-545:(OI)(CI)(RX)' | Out-Null
    if ($LASTEXITCODE) { throw "icacls lock failed on $secure ($LASTEXITCODE)" }

    Copy-Item (Join-Path $ResourcesDir 'KokoroSapi.dll')         $secure -Force
    Copy-Item (Join-Path $ResourcesDir 'kindle-voice-guard.ps1') $secure -Force

    # Own the copies with the Administrators GROUP (not the elevating user) so that,
    # if the user is a local admin, their own medium-integrity process still can't use
    # owner-implicit WRITE_DAC to reopen the ACL. Fail closed if this can't be set.
    & icacls $secure /setowner '*S-1-5-32-544' /T /C | Out-Null
    if ($LASTEXITCODE) { throw "icacls setowner failed on $secure ($LASTEXITCODE)" }

    $dll   = Join-Path $secure 'KokoroSapi.dll'
    $guard = Join-Path $secure 'kindle-voice-guard.ps1'
    L "regsvr32 register $dll"
    & $regsvr '/s' $dll
    # Make Kokoro Kindle's default now that the KokoroTTS token exists. Self-skips if
    # Kindle's hive is absent (Kindle not installed).
    & $guard -Set kokoro
} else {
    # Prefer the secured copies; fall back to resources\ for an older install that
    # registered/ran those directly (so an upgrade-then-uninstall still cleans up).
    $dll   = Join-Path $secure 'KokoroSapi.dll'
    $guard = Join-Path $secure 'kindle-voice-guard.ps1'
    if (-not (Test-Path $dll))   { $dll   = Join-Path $ResourcesDir 'KokoroSapi.dll' }
    if (-not (Test-Path $guard)) { $guard = Join-Path $ResourcesDir 'kindle-voice-guard.ps1' }

    # Revert Kindle to Microsoft David BEFORE deleting the token, so its hive isn't
    # left pointing DefaultTokenId at a gone KokoroTTS token.
    & $guard -Set david
    L "regsvr32 unregister $dll"
    & $regsvr '/u' '/s' $dll

    # Drop the secured copies (reclaim ownership first so the delete can't be blocked).
    Reset-Owner $secureRoot
    if (Test-Path $secureRoot) { Remove-Item $secureRoot -Recurse -Force -ErrorAction SilentlyContinue }
}
L "done ($Action)"
