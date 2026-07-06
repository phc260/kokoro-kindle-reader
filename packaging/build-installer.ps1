# Build the headless-edition NSIS installer: release-builds the tray host + Slint
# panel, stages everything the installer needs (both exes + native runtime DLLs +
# espeak data + the x86 KokoroSapi.dll + guard scripts), then runs makensis.
#
#   packaging\build-installer.ps1            # full: build + stage + makensis
#   packaging\build-installer.ps1 -SkipBuild # reuse existing release binaries
#   packaging\build-installer.ps1 -RustSapi  # bundle the Rust SAPI DLL (prototype)
#
# Output: packaging\kokoro-kindle-reader-<version>[-rust]-setup.exe
param([switch]$SkipBuild, [switch]$RustSapi)
$ErrorActionPreference = 'Stop'

$here = $PSScriptRoot
$root = Split-Path $here -Parent
$hostRel = Join-Path $root 'kokoro-host\target\release'
$panelRel = Join-Path $root 'kokoro-panel\target\release'
$sapi = Join-Path $root 'kokoro-sapi'

# 1. The x86 SAPI DLL. -RustSapi bundles the Rust prototype engine
#    (kokoro-sapi-rs) instead of the C++ one; both export the same COM entry points
#    + CLSID, so the registration flow (voice-setup.ps1) is identical.
if ($RustSapi) {
    Write-Host '==> Building the Rust x86 KokoroSapi.dll (kokoro-sapi-rs)'
    Push-Location (Join-Path $root 'kokoro-sapi-rs')
    cargo build --release --target i686-pc-windows-msvc
    if ($LASTEXITCODE) { throw 'Rust SAPI DLL build failed' }
    Pop-Location
    $sapiDll = Join-Path $root 'kokoro-sapi-rs\target\i686-pc-windows-msvc\release\KokoroSapi.dll'
} else {
    if (-not (Test-Path (Join-Path $sapi 'build\KokoroSapi.dll'))) {
        Write-Host '==> Building x86 KokoroSapi.dll'
        & (Join-Path $sapi 'build.ps1')
    }
    $sapiDll = Join-Path $sapi 'build\KokoroSapi.dll'
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
Copy-Item (Join-Path $root 'icons\icon.ico') (Join-Path $stage 'icon.ico')

$res = Join-Path $stage 'resources'
Copy-Item $sapiDll $res
Copy-Item (Join-Path $sapi 'kindle-voice-guard.ps1') $res
Copy-Item (Join-Path $sapi 'voice-setup.ps1') $res

# 4. Compile the installer.
$makensis = 'C:\Program Files (x86)\NSIS\makensis.exe'
if (-not (Test-Path $makensis)) { throw "makensis not found at $makensis - install NSIS." }
Write-Host '==> makensis'
& $makensis (Join-Path $here 'installer.nsi')
if ($LASTEXITCODE) { throw 'makensis failed' }

$out = Get-ChildItem (Join-Path $here '*-setup.exe') | Where-Object { $_.Name -notlike '*-rust-setup.exe' } | Sort-Object LastWriteTime -Descending | Select-Object -First 1
if ($RustSapi) {
    # Rename so the Rust-engine build doesn't collide with the C++ one.
    $rust = $out.FullName -replace '-setup\.exe$', '-rust-setup.exe'
    Move-Item -Force $out.FullName $rust
    $out = Get-Item $rust
    Write-Host '    (bundled the Rust SAPI engine)'
}
Write-Host "==> Installer: $($out.FullName)  ($([math]::Round($out.Length/1MB,1)) MB)"
