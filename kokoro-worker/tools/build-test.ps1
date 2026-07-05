# Build the standalone Phase-1 WebGPU synth test (x64): compiles KokoroText +
# KokoroSynth (WebGPU EP) + the driver, links the prebuilt ORT import lib and the
# x64 espeak import lib, then stages the runtime DLLs next to the exe:
#   - onnxruntime.dll (the WebGPU/Dawn build from the onnxruntime-webgpu wheel, NOT
#     the CPU release DLL) + providers_shared + dxcompiler + dxil
#   - espeak-ng.dll
# The espeak-ng-data dir is passed as an arg at run time (not copied).
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot                 # kokoro-worker/
$tp   = Join-Path $root 'third_party'
$ortInc = Join-Path $tp 'onnxruntime\include'
$ortLib = Join-Path $tp 'onnxruntime\lib\onnxruntime.lib'
$espkInc = Join-Path $tp 'espeak-ng-src\src\include'
$espkLib = Join-Path $tp 'espeak-ng-src\build-x64\src\libespeak-ng\espeak-ng.lib'
$espkDll = Join-Path $tp 'espeak-ng-src\build-x64\src\espeak-ng.dll'
# WebGPU runtime DLLs live in the scratchpad venv's onnxruntime wheel.
$wheelCapi = 'C:\Users\phc260\AppData\Local\Temp\claude\U--Projects-kokoro-kindle-reader\fede82f8-9ea1-46f2-891c-1f5eecbd5a4d\scratchpad\venv\Lib\site-packages\onnxruntime\capi'

$out = Join-Path $root 'build-test'
New-Item -ItemType Directory -Force -Path $out | Out-Null

foreach ($f in @($ortInc, $ortLib, $espkInc, $espkLib, $espkDll, $wheelCapi)) {
    if (-not (Test-Path $f)) { throw "missing prerequisite: $f" }
}

$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$vsPath  = (& $vswhere -latest -products * -property installationPath).Trim()
$vcvars  = Join-Path $vsPath 'VC\Auxiliary\Build\vcvarsall.bat'

# Use a response file so we sidestep cmd/argv quoting (a trailing-backslash path
# before a quote escapes the quote and swallows args). Forward slash on /Fo dir.
$rsp = Join-Path $out 'cl.rsp'
@"
/std:c++17
/EHsc
/O2
/MD
/nologo
/Fo"$out/"
/I "$ortInc"
/I "$espkInc"
"$root\src\KokoroText.cpp"
"$root\src\KokoroSynth.cpp"
"$root\tools\kokoro_synth_test.cpp"
/Fe"$out\kokoro_synth_test.exe"
/link
"$ortLib"
"$espkLib"
"@ | Set-Content -Encoding ascii $rsp

cmd /D /c "`"$vcvars`" x64 && cl @`"$rsp`""
if ($LASTEXITCODE -ne 0) { throw "compile/link failed ($LASTEXITCODE)" }

# Stage runtime DLLs (WebGPU ORT from the wheel).
foreach ($d in @('onnxruntime.dll','onnxruntime_providers_shared.dll','dxcompiler.dll','dxil.dll')) {
    Copy-Item (Join-Path $wheelCapi $d) $out -Force
}
Copy-Item $espkDll $out -Force

Write-Host "`n=== built ==="
Get-ChildItem $out -Include '*.exe','*.dll' -Recurse | Select-Object Name, Length
