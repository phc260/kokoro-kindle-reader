# Reproducibly provision native-deps/ (the runtime DLLs the host stages +
# the espeak import lib it links) with no manual venv / hardcoded paths, so a fresh
# clone or CI runner can build the synth. Populates:
#
#   native-deps/runtime/*.dll               (Dawn/WebGPU onnxruntime.dll +
#                                            providers_shared + dxcompiler + dxil,
#                                            from the onnxruntime-webgpu wheel;
#                                            espeak-ng.dll from the espeak build)
#   native-deps/runtime/notices/            (that wheel's OWN licence + notice files,
#                                            kept because we redistribute its binaries)
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

# --- 1. onnxruntime-webgpu wheel: the Dawn runtime DLLs + their notices ------
$runtime = Join-Path $tp 'runtime'
$notices = Join-Path $runtime 'notices'
New-Item -ItemType Directory -Force $runtime | Out-Null
# Re-fetch when EITHER half is missing, not just the DLLs. The notices are a later
# addition, so an existing provision has the DLLs and not them, and gating that on -Force
# is how an installer build ends up staging licence text that was never fetched.
if ($Force -or -not (Test-Path (Join-Path $runtime 'onnxruntime.dll')) -or
    -not (Test-Path (Join-Path $notices '*'))) {
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

    # Keep the wheel's OWN licence + notice files, and keep them next to the DLLs they
    # describe. We redistribute four binaries out of this wheel, and dxcompiler.dll's
    # licence (University of Illinois/NCSA) requires its notice accompany them - as does
    # everything ORT links statically, which is far more than this script can enumerate.
    # ORT's ThirdPartyNotices.txt is the authoritative record of all of it, and taking it
    # from the wheel keeps it matched to the exact build being shipped; a copy transcribed
    # into the repo by hand would silently stop being true at the next version bump.
    #
    # Globbed RECURSIVELY rather than by a fixed path: the wheel's internal layout is
    # upstream's to change, and a path that quietly stops matching would ship the binaries
    # with none of their notices - which is the failure this whole block exists to fix.
    # Hence the throw: nothing found is never "fine, carry on".

    # Start from empty so a rename upstream cannot leave a stale notice behind, describing
    # a version that is no longer the one being shipped. Named directory, not a wildcard.
    Remove-Item -Recurse -Force $notices -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Force $notices | Out-Null
    # NB: -Include takes the BARE directory here. Adding the conventional trailing \* -
    # which is what makes -Include work when there is no -Recurse - silently matches
    # NOTHING in this combination on PS 5.1. Measured, not assumed: 0 files vs 4.
    $found = @(Get-ChildItem $wex -Recurse -File `
                             -Include 'LICENSE*', 'NOTICE*', 'ThirdPartyNotices*', 'Privacy*')
    if ($found.Count -eq 0) {
        throw ("No licence or notice file found in the onnxruntime-webgpu wheel (looked " +
               "for LICENSE*/NOTICE*/ThirdPartyNotices*/Privacy* under $wex). Shipping " +
               "those DLLs without them is exactly what this step exists to prevent - " +
               "find where upstream moved them and widen the glob.")
    }
    foreach ($f in $found) {
        # Plain names in the normal case; disambiguate only on a collision, so two
        # different LICENSE files can never overwrite each other down to one.
        $dest = Join-Path $notices $f.Name
        if (Test-Path $dest) { $dest = Join-Path $notices ('{0}-{1}' -f $f.Directory.Name, $f.Name) }
        Copy-Item $f.FullName $dest -Force
    }
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
Write-Host ("    ORT notices     : {0}" -f @(Get-ChildItem $notices -File -ErrorAction SilentlyContinue).Count)
Write-Host ("    espeak-ng.dll   : {0}" -f (Test-Path $espkDll))
