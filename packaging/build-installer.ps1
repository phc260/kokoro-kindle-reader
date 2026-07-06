# Build the NSIS installer: build the x86 Rust SAPI DLL (kokoro-sapi-rs),
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
$sapiRs = Join-Path $root 'kokoro-sapi-rs'

# 1. Build the x86 SAPI DLL (Kindle is 32-bit, loads it in-process). The Rust engine
#    is connect-only -- it forwards Speak to kokoro-host over the pipe -- so there's no
#    ONNX/espeak dep here.
Write-Host '==> cargo build --release --target i686-pc-windows-msvc (kokoro-sapi-rs)'
Push-Location $sapiRs
cargo build --release --target i686-pc-windows-msvc
if ($LASTEXITCODE) { throw 'SAPI DLL build failed (need the i686-pc-windows-msvc target?)' }
Pop-Location
$sapiDll = Join-Path $sapiRs 'target\i686-pc-windows-msvc\release\KokoroSapi.dll'

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

$res = Join-Path $stage 'resources'
Copy-Item $sapiDll $res
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
