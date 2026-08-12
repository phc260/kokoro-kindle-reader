# Build the NSIS installer: build the x86 Rust SAPI DLL (kokoro-sapi),
# release-build the tray host + Slint panel, stage everything the installer needs
# (both exes + native runtime DLLs + espeak data + the x86 KokoroSapi.dll + guard
# scripts), then run makensis.
#
#   packaging\build-installer.ps1            # full: build + stage + makensis
#   packaging\build-installer.ps1 -SkipBuild # reuse existing release binaries
#
# Output: packaging\kokoro-kindle-reader-<version>-setup.exe
param([switch]$SkipBuild)
$ErrorActionPreference = 'Stop'

$here = $PSScriptRoot
$root = Split-Path $here -Parent
$hostRel = Join-Path $root 'kokoro-host\target\release'
$panelRel = Join-Path $root 'kokoro-panel\target\release'
$sapiRs = Join-Path $root 'kokoro-sapi'

# 1. Build the x86 SAPI DLL (Kindle is 32-bit, loads it in-process). The Rust engine
#    is connect-only -- it forwards Speak to kokoro-host over the pipe -- so there's no
#    ONNX/espeak dep here.
Write-Host '==> cargo build --release --target i686-pc-windows-msvc (kokoro-sapi)'
Push-Location $sapiRs
cargo build --release --target i686-pc-windows-msvc
if ($LASTEXITCODE) { throw 'SAPI DLL build failed (need the i686-pc-windows-msvc target?)' }
Pop-Location
$sapiDll = Join-Path $sapiRs 'target\i686-pc-windows-msvc\release\KokoroSapi.dll'

# 1b. Build the x86 Kindle-hook DLL + injector (Kindle is 32-bit). The host's watcher spawns
#     the injector, which LoadLibrary-loads the hook into Kindle to force the Kokoro voice.
foreach ($c in 'kokoro-hook', 'kokoro-inject') {
    Write-Host "==> cargo build --release --target i686-pc-windows-msvc ($c)"
    Push-Location (Join-Path $root $c)
    cargo build --release --target i686-pc-windows-msvc
    if ($LASTEXITCODE) { throw "$c build failed (need the i686-pc-windows-msvc target?)" }
    Pop-Location
}
$hookDll = Join-Path $root 'kokoro-hook\target\i686-pc-windows-msvc\release\kokoro_hook.dll'
$injectExe = Join-Path $root 'kokoro-inject\target\i686-pc-windows-msvc\release\kokoro-inject.exe'

# 2. Release-build both Rust crates (each stages its own runtime next to the exe).
if (-not $SkipBuild) {
    Write-Host '==> cargo build --release (kokoro-host)'
    Push-Location (Join-Path $root 'kokoro-host'); cargo build --release; if ($LASTEXITCODE) { throw 'host build failed' }; Pop-Location
    Write-Host '==> cargo build --release (kokoro-panel)'
    Push-Location (Join-Path $root 'kokoro-panel'); cargo build --release; if ($LASTEXITCODE) { throw 'panel build failed' }; Pop-Location
}

# 3. Stage the bundle.
$stage = Join-Path $here 'staging'
Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $stage, (Join-Path $stage 'resources') | Out-Null

Copy-Item (Join-Path $hostRel 'kokoro-host.exe') $stage
Copy-Item (Join-Path $panelRel 'kokoro-panel.exe') $stage
foreach ($d in 'onnxruntime.dll', 'onnxruntime_providers_shared.dll', 'dxcompiler.dll', 'dxil.dll', 'espeak-ng.dll') {
    Copy-Item (Join-Path $hostRel $d) $stage
}
Copy-Item -Recurse (Join-Path $hostRel 'espeak-ng-data') $stage
Copy-Item (Join-Path $root 'icons\icon.ico') (Join-Path $stage 'icon.ico')

# 3a. The Cloud Reader OCR models (9.80 MB). Staged from native-deps rather than from the
#     build output: they are loaded at RUN time, so nothing in the build copies them next to
#     the exe, and a host that ships without them answers every /ocr with `missing`. Failing
#     here is the point - the alternative is an installer that looks complete and cannot read
#     a page.
#     All THREE are verified against their pinned digests, by the fetch script's own
#     -VerifyOnly mode - not by an existence check on one of them, and not by a second
#     copy of the pins here. An interrupted download leaves one file present and another
#     absent or truncated, which an existence check waves through: the build then
#     succeeds, the installer looks complete, and the host reports 'missing' on the
#     first page.
#     The three files are then copied BY NAME, not as a directory. A recursive copy would
#     stage whatever else is sitting in native-deps\ocr - a stale model from an older pin,
#     a scratch file - and ship it verified by nothing.
$ocrSrc = Join-Path $root 'native-deps\ocr'
& (Join-Path $root 'native-deps\fetch-ocr-models.ps1') -VerifyOnly
$ocrStage = Join-Path $stage 'ocr'
New-Item -ItemType Directory -Force $ocrStage | Out-Null
foreach ($f in 'det.onnx', 'rec.onnx', 'en_dict.txt') {
    Copy-Item (Join-Path $ocrSrc $f) $ocrStage
}

# 3b. License texts. The bundle links espeak-ng (GPL-3.0-or-later, and MODIFIED -- see
#     native-deps\build-espeak.ps1) and Slint under its GPL-3.0-only option, so the
#     installed app as a whole is conveyed under GPLv3: the notices + the GPL text must
#     ship WITH the binaries, not just live in the repo. THIRD_PARTY_NOTICES.md links
#     LICENSE and licenses\*, so keep all three together and keep the layout.
Copy-Item (Join-Path $root 'LICENSE') $stage
Copy-Item (Join-Path $root 'THIRD_PARTY_NOTICES.md') $stage
Copy-Item -Recurse (Join-Path $root 'licenses') $stage

#     ONNX Runtime's own licence + notice set, staged from native-deps rather than kept in
#     the repo, so it stays matched to the exact wheel the shipped DLLs came out of. Four
#     of the binaries we install come from that wheel, and dxcompiler.dll's licence
#     (University of Illinois/NCSA) requires its notice accompany them.
#     Throwing beats shipping without: an installer missing licence text looks complete and
#     is not, which is the same trap as the OCR models above.
$ortNotices = Join-Path $root 'native-deps\runtime\notices'
if (-not (Test-Path (Join-Path $ortNotices '*'))) {
    throw ("No ONNX Runtime notices at $ortNotices - run native-deps\fetch-deps.ps1 " +
           '(it fetches them alongside the runtime DLLs).')
}
$ortStage = Join-Path $stage 'licenses\onnxruntime'
New-Item -ItemType Directory -Force $ortStage | Out-Null
Copy-Item (Join-Path $ortNotices '*') $ortStage -Force

$res = Join-Path $stage 'resources'
Copy-Item $sapiDll $res
Copy-Item $hookDll $res
Copy-Item $injectExe $res
Copy-Item (Join-Path $sapiRs 'kindle-voice-guard.ps1') $res
Copy-Item (Join-Path $sapiRs 'voice-setup.ps1') $res

# 4. Compile the installer.
$makensis = 'C:\Program Files (x86)\NSIS\makensis.exe'
if (-not (Test-Path $makensis)) { throw "makensis not found at $makensis - install NSIS." }
Write-Host '==> makensis'
& $makensis (Join-Path $here 'installer.nsi')
if ($LASTEXITCODE) { throw 'makensis failed' }

$out = Get-ChildItem (Join-Path $here '*-setup.exe') | Sort-Object LastWriteTime -Descending | Select-Object -First 1
Write-Host "==> Installer: $($out.FullName)  ($([math]::Round($out.Length/1MB,1)) MB)"
