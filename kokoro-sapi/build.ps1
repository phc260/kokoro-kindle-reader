# Builds the Kokoro SAPI engine (x86). Kindle.exe is a 32-bit process and loads
# the engine in-process via ISpVoice, so the DLL must be x86. The engine is
# connect-only — it delegates synthesis to the kokoro-reader app over a named
# pipe — so there is no x64 worker and no ONNX/espeak dependency to build.
#
# Usage:
#   .\build.ps1                  # configure + build (Release)
#   .\build.ps1 -Config Debug
param(
    [string]$Config = 'Release'
)
$ErrorActionPreference = 'Stop'

$root = $PSScriptRoot

# Locate Visual Studio's environment script via vswhere.
$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
if (-not (Test-Path $vswhere)) { throw "vswhere not found: $vswhere" }
$vsPath = (& $vswhere -latest -products * -property installationPath).Trim()
if (-not $vsPath) { throw "No Visual Studio installation found." }
$vcvars = Join-Path $vsPath 'VC\Auxiliary\Build\vcvarsall.bat'
if (-not (Test-Path $vcvars)) { throw "vcvarsall.bat not found: $vcvars" }

$buildDir = Join-Path $root 'build'
# Wipe a stale cache so a previous generator/arch choice can't stick.
if (Test-Path (Join-Path $buildDir 'CMakeCache.txt')) {
    $cached = Select-String -Path (Join-Path $buildDir 'CMakeCache.txt') -Pattern 'CMAKE_SIZEOF_VOID_P' -ErrorAction SilentlyContinue
    if ($cached -and $cached.Line -notmatch '=4$') { Remove-Item -Recurse -Force $buildDir }
}

$cmd = "`"$vcvars`" x86 && cmake -S `"$root`" -B `"$buildDir`" -G `"NMake Makefiles`" -DCMAKE_BUILD_TYPE=$Config && cmake --build `"$buildDir`""
cmd /c $cmd
if ($LASTEXITCODE -ne 0) { throw "x86 build failed ($LASTEXITCODE)" }

$dll = Join-Path $buildDir 'KokoroSapi.dll'
Write-Host ""
Write-Host "Built: $dll"
Write-Host ""
Write-Host "Register the voice (ELEVATED prompt; 32-bit regsvr32):"
Write-Host "    C:\Windows\SysWOW64\regsvr32.exe `"$dll`""
Write-Host "Unregister:"
Write-Host "    C:\Windows\SysWOW64\regsvr32.exe /u `"$dll`""
Write-Host ""
Write-Host "Synthesis requires the kokoro-reader app to be running (it serves the pipe)."
