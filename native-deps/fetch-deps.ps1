# Reproducibly provision native-deps/ (the runtime DLLs the host stages +
# the espeak import lib it links) with no manual venv / hardcoded paths, so a fresh
# clone or CI runner can build the synth. Populates:
#
#   native-deps/runtime/*.dll               (Dawn/WebGPU onnxruntime.dll +
#                                            providers_shared + dxcompiler + dxil,
#                                            from the onnxruntime-webgpu wheel;
#                                            espeak-ng.dll from the espeak build)
#   native-deps/espeak-ng-src/...           (espeak-ng 1.52.0 x64 + horse-hoarse
#                                            revert + import lib, via build-espeak.ps1)
#
# The ONNX model runs on the `ort` crate's WebGPU EP via load-dynamic, so onnxruntime.dll
# is loaded at runtime (not linked) - no ORT headers/import lib needed.
#
# Requires: Python+pip (for `pip download` of the wheel), CMake + MSVC (espeak),
# and network. Idempotent: pass -Force to re-provision.
param(
    [string]$OrtVersion = '1.27.0',
    [switch]$Force
)
$ErrorActionPreference = 'Stop'
$tp   = $PSScriptRoot                       # native-deps/
New-Item -ItemType Directory -Force $tp | Out-Null

$ProgressPreference = 'SilentlyContinue'   # fast Invoke-WebRequest

# --- 1. onnxruntime-webgpu wheel: the Dawn runtime DLLs ----------------------
$runtime = Join-Path $tp 'runtime'
New-Item -ItemType Directory -Force $runtime | Out-Null
if ($Force -or -not (Test-Path (Join-Path $runtime 'onnxruntime.dll'))) {
    Write-Host "==> Fetching onnxruntime-webgpu $OrtVersion wheel (Dawn DLLs)"
    $wdir = Join-Path $env:TEMP "ort-webgpu-$OrtVersion"
    Remove-Item -Recurse -Force $wdir -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Force $wdir | Out-Null
    # pip resolves the right cpXX wheel for the runner's Python.
    & python -m pip download "onnxruntime-webgpu==$OrtVersion" --only-binary=:all: --no-deps -d $wdir
    if ($LASTEXITCODE) { throw 'pip download onnxruntime-webgpu failed' }
    $whl = Get-ChildItem $wdir -Filter '*.whl' | Select-Object -First 1
    $zip = [System.IO.Path]::ChangeExtension($whl.FullName, '.zip')
    Copy-Item $whl.FullName $zip -Force
    $wex = Join-Path $wdir 'x'
    Expand-Archive $zip -DestinationPath $wex -Force
    $capi = Join-Path $wex 'onnxruntime\capi'
    # The Dawn onnxruntime.dll + providers_shared + dxcompiler + dxil ship here.
    Get-ChildItem $capi -Filter '*.dll' | ForEach-Object { Copy-Item $_.FullName $runtime -Force }
}

# --- 2. espeak-ng x64 (clone + build) ---------------------------------------
# build-espeak.ps1 needs the source clone to exist (it's gitignored, so a fresh
# checkout / CI runner won't have it). Clone the 1.52.0 tag before building; the
# build script does the tag checkout + horse-hoarse revert on top of it.
$espkSrc = Join-Path $tp 'espeak-ng-src'
if (-not (Test-Path (Join-Path $espkSrc '.git'))) {
    Write-Host '==> Cloning espeak-ng (tag 1.52.0)'
    & git clone --branch 1.52.0 --depth 1 https://github.com/espeak-ng/espeak-ng.git $espkSrc
    if ($LASTEXITCODE) { throw 'git clone espeak-ng failed' }
}

$espkDll = Join-Path $tp 'espeak-ng-src\build-x64\src\espeak-ng.dll'
if ($Force -or -not (Test-Path $espkDll)) {
    Write-Host '==> Building espeak-ng (x64, 1.52.0 + horse-hoarse revert)'
    & (Join-Path $PSScriptRoot 'build-espeak.ps1')
    if ($LASTEXITCODE) { throw 'build-espeak.ps1 failed' }
}
Copy-Item $espkDll $runtime -Force

Write-Host '==> native-deps provisioned:'
Write-Host ("    runtime DLLs    : {0}" -f (Get-ChildItem $runtime -Filter '*.dll').Count)
Write-Host ("    espeak-ng.dll   : {0}" -f (Test-Path $espkDll))
