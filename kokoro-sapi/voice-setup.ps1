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
# -ResourcesDir is the bundle's resources\ dir -- the SOURCE of the secured copies, and
# nothing else. Neither action ever regsvr32s or runs a file out of it. (The one elevated
# artifact still read from a user-writable path is THIS script, which the UAC relaunch
# below re-executes by $PSCommandPath -- the entry-point residual that only a signed
# installer closes. It is not a reason to widen the set to the DLL and the guard.)
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
    # Absolute path, not a bare 'powershell': this launch is ELEVATED, and resolving the
    # name through the user-controlled PATH would be a free admin-code-execution hop.
    $ps = Join-Path $PSHOME 'powershell.exe'
    $p = Start-Process -Verb RunAs -Wait -PassThru -FilePath $ps -ArgumentList @(
        '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', $PSCommandPath,
        '-Action', $Action, '-ResourcesDir', $ResourcesDir
    )
    # Propagate the elevated half's result. Exiting 0 unconditionally reported a clean
    # install/uninstall even when every privileged step had failed.
    $rc = if ($null -ne $p -and $null -ne $p.ExitCode) { $p.ExitCode } else { 1 }
    if ($rc) { L "elevated run FAILED ($rc)" }
    exit $rc
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

# Drops exactly what DllUnregisterServer drops, WITHOUT regsvr32-ing the user-writable
# resources\ DLL -- used both to tear down an install that predates the secured copies (see
# the unregister branch) and to roll back a half-finished register. The CLSID and token key
# are permanent COM identities; keep them in sync with CLSID_KOKORO / TOKEN_KEY in
# kokoro-sapi\src\lib.rs. The 32-bit regsvr32 wrote through WOW64 redirection, so clear
# both views (deleting a key that was never written is a no-op).
function Remove-KokoroRegistration {
    $clsid = '{0898F9AB-42C8-4DA5-A54F-520C9DD13C49}'
    @(
        "HKLM:\SOFTWARE\Classes\WOW6432Node\CLSID\$clsid",
        "HKLM:\SOFTWARE\Classes\CLSID\$clsid",
        'HKLM:\SOFTWARE\WOW6432Node\Microsoft\Speech\Voices\Tokens\KokoroTTS',
        'HKLM:\SOFTWARE\Microsoft\Speech\Voices\Tokens\KokoroTTS'
    ) | ForEach-Object {
        if (Test-Path $_) { Remove-Item $_ -Recurse -Force; L "removed $_" }
    }
}

# Companion to Remove-KokoroRegistration on the legacy teardown path: Kindle's package hive
# may still point DefaultTokenId at KokoroTTS, and dropping the token without clearing it
# leaves an older Kindle (pre-1.0.18632.0, which is the only kind that reads it) pointing at
# a voice that no longer exists. Same hive/mount mechanics as kindle-voice-guard.ps1, inline
# so nothing under resources\ has to be executed -- but it DELETES the value rather than
# setting David: with Kokoro uninstalled, falling back to Kindle's own default is the
# correct end state. Best-effort; a locked or absent hive just logs.
#
# Uses reg.exe rather than the HKLM: provider throughout -- a live provider handle into the
# mounted hive is what makes `reg unload` fail.
function Clear-KindleDefaultToken {
    $hive = @(Get-ChildItem "$env:LOCALAPPDATA\Packages" -Directory -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -like '*AMZNKindle*' } |
        ForEach-Object { Join-Path $_.FullName 'SystemAppData\Helium\User.dat' } |
        Where-Object { Test-Path $_ })
    if (-not $hive.Count) { L "Kindle hive not found; no DefaultTokenId to revert"; return }

    if (Get-Process Kindle -ErrorAction SilentlyContinue) {
        L "stopping Kindle to edit its hive"
        Stop-Process -Name Kindle -Force -Confirm:$false
        Start-Sleep -Seconds 2
    }

    $mount = 'HKLM\KokoroVoiceSetup'
    $key   = "$mount\SOFTWARE\Microsoft\Speech\Voices"
    & reg load $mount "$($hive[0])" | Out-Null
    if ($LASTEXITCODE) { L "reg load failed ($LASTEXITCODE); leaving DefaultTokenId alone"; return }
    try {
        # Only touch it if it is actually ours -- a user who picked another voice keeps it.
        $cur = (& reg query $key /v DefaultTokenId) -join ' '
        if ($cur -match 'KokoroTTS') {
            & reg delete $key /v DefaultTokenId /f | Out-Null
            L "cleared Kindle DefaultTokenId (was KokoroTTS)"
        } else {
            L "Kindle DefaultTokenId is not Kokoro; left as-is"
        }
    } finally {
        [gc]::Collect(); Start-Sleep -Milliseconds 400
        & reg unload $mount | Out-Null
    }
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
    if ($LASTEXITCODE) {
        # DllRegisterServer writes the CLSID before the token and bails on the first
        # failure, so a nonzero result can mean "half registered" -- a CLSID pointing at a
        # DLL we are about to delete, with no usable voice token. Undo both before
        # reporting, so a failed install leaves nothing behind to confuse the next one.
        $code = $LASTEXITCODE
        L "regsvr32 register FAILED ($code); rolling back"
        Remove-KokoroRegistration
        Reset-Owner $secureRoot
        if (Test-Path $secureRoot) { Remove-Item $secureRoot -Recurse -Force -ErrorAction SilentlyContinue }
        throw "regsvr32 register failed on $dll ($code)"
    }
    # Make Kokoro Kindle's default now that the KokoroTTS token exists. Self-skips if
    # Kindle's hive is absent (Kindle not installed).
    & $guard -Set kokoro
} else {
    # ONLY ever execute the ACL-locked copies. resources\ lives under %LOCALAPPDATA%, so
    # falling back to it would run attacker-replaceable code as admin: regsvr32 /u calls
    # the DLL's DllUnregisterServer, and the guard is a script -- exactly the local EoP
    # the register branch above stages the locked copies to close, re-opened at uninstall
    # (when resources\ has been sitting user-writable for the life of the install).
    # An older install that never staged them is torn down without executing anything.
    $dll   = Join-Path $secure 'KokoroSapi.dll'
    $guard = Join-Path $secure 'kindle-voice-guard.ps1'

    # Revert Kindle BEFORE dropping the token, so its hive isn't left pointing
    # DefaultTokenId at a gone KokoroTTS token. With no secured guard copy to run, do the
    # revert inline instead -- never out of resources\.
    if (Test-Path $guard) { & $guard -Set david }
    else {
        L "no secured guard copy; reverting DefaultTokenId inline (never run resources\)"
        Clear-KindleDefaultToken
    }

    $rc = 0
    if (Test-Path $dll) {
        L "regsvr32 unregister $dll"
        & $regsvr '/u' '/s' $dll
        $rc = $LASTEXITCODE
        if ($rc) { L "regsvr32 /u FAILED ($rc)" }
    } else {
        L "no secured DLL copy; removing the registration keys directly"
        Remove-KokoroRegistration
    }

    # Drop the secured copies (reclaim ownership first so the delete can't be blocked).
    Reset-Owner $secureRoot
    if (Test-Path $secureRoot) { Remove-Item $secureRoot -Recurse -Force -ErrorAction SilentlyContinue }

    # Reported only after the cleanup above, so a regsvr32 failure can't strand the tree.
    if ($rc) { throw "regsvr32 /u failed on $dll ($rc)" }
}
L "done ($Action)"
