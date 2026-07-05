# Build the headless-edition NSIS installer: release-builds the tray host + Slint
# panel, stages everything the installer needs (both exes + native runtime DLLs +
# espeak data + the x86 KokoroSapi.dll + guard scripts), then runs makensis.
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
$sapi = Join-Path $root 'kokoro-sapi'

# 1. The x86 SAPI DLL must exist (its build is separate — NMake/x86).
if (-not (Test-Path (Join-Path $sapi 'build\KokoroSapi.dll'))) {
    Write-Host '==> Building x86 KokoroSapi.dll'
    & (Join-Path $sapi 'build.ps1')
}

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
Copy-Item (Join-Path $root 'src-tauri\icons\icon.ico') (Join-Path $stage 'icon.ico')

$res = Join-Path $stage 'resources'
Copy-Item (Join-Path $sapi 'build\KokoroSapi.dll') $res
Copy-Item (Join-Path $sapi 'kindle-voice-guard.ps1') $res
Copy-Item (Join-Path $sapi 'voice-setup.ps1') $res

# 4. Compile the installer.
$makensis = 'C:\Program Files (x86)\NSIS\makensis.exe'
if (-not (Test-Path $makensis)) { throw "makensis not found at $makensis - install NSIS." }
Write-Host '==> makensis'
& $makensis (Join-Path $here 'installer.nsi')
if ($LASTEXITCODE) { throw 'makensis failed' }

$out = Get-ChildItem (Join-Path $here '*-setup.exe') | Sort-Object LastWriteTime -Descending | Select-Object -First 1
Write-Host "==> Installer: $($out.FullName)  ($([math]::Round($out.Length/1MB,1)) MB)"
