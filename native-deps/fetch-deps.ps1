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
    # NB: on THIS wheel's layout - LICENSE, Privacy.md and ThirdPartyNotices.txt live one
    # level down, under onnxruntime\, not at $wex's own root - -Include needs the BARE
    # directory here. Adding the conventional trailing \* silently matches NOTHING in that
    # specific one-level-deeper-plus-Recurse combination on PS 5.1 (measured against the
    # real 1.27.0 wheel: 3 files vs 0). That is a property of the files not sitting
    # directly under the passed path, not a general PS 5.1 -Include rule - the same two
    # forms return identical results when they do.
    $found = @(Get-ChildItem $wex -Recurse -File `
                             -Include 'LICENSE*', 'NOTICE*', 'ThirdPartyNotices*', 'Privacy*')
    if ($found.Count -eq 0) {
        throw ("No licence or notice file found in the onnxruntime-webgpu wheel (looked " +
               "for LICENSE*/NOTICE*/ThirdPartyNotices*/Privacy* under $wex). Shipping " +
               "those DLLs without them is exactly what this step exists to prevent - " +
               "find where upstream moved them and widen the glob.")
    }
    foreach ($f in $found) {
        # Preserve the file's path RELATIVE TO $wex, not just its basename. Two distinct
        # files that share both a basename and their immediate parent directory's name
        # (e.g. two different vendored sub-packages each carrying their own LICENSE, one
        # nested another level deeper) would still collide under a basename-plus-one-level
        # disambiguation scheme, and Copy-Item -Force would silently drop the second one.
        # A full relative path cannot collide, because Expand-Archive already extracted
        # every file in $wex to a distinct path.
        $rel = $f.FullName.Substring($wex.Length).TrimStart('\', '/')
        $dest = Join-Path $notices $rel
        New-Item -ItemType Directory -Force (Split-Path $dest -Parent) | Out-Null
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

# --- 2a. espeak-ng's OWN licence + notice files -----------------------------
# We ship a MODIFIED espeak-ng.dll + espeak-ng-data/ (GPL-3.0-or-later), and parts of the
# espeak-ng tree carry ADDITIONAL licences that must accompany the binaries: COPYING is the
# GPLv3 text, COPYING.APACHE / COPYING.BSD2 cover code shims, and COPYING.UCD covers the
# Unicode Character Database data baked into espeak-ng-data/. COPYING.UCD is NOT the same
# document as licenses/Unicode-3.0.txt (that is the Unicode v3 licence for the unicode-ident
# crate) - the two are distinct and both are required. Provisioned from the exact 1.52.0
# clone we build, so they stay matched to the shipped DLL, the same way the ORT notices are.
# Re-provision when missing (a later addition, like the ORT notices) so an old provision
# doesn't ship the DLL with no espeak notices.
$espkNotices = Join-Path $tp 'espeak-ng-notices'
if ($Force -or -not (Test-Path (Join-Path $espkNotices '*'))) {
    Remove-Item -Recurse -Force $espkNotices -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Force $espkNotices | Out-Null
    # Named files, not a wildcard sweep of the tree: ship exactly the four licence texts,
    # nothing else the clone happens to contain. Each must exist - a modified GPL binary
    # shipped without its licence text is the failure this whole block prevents.
    foreach ($c in 'COPYING', 'COPYING.APACHE', 'COPYING.BSD2', 'COPYING.UCD') {
        $src = Join-Path $espkSrc $c
        if (-not (Test-Path $src)) {
            throw ("espeak-ng licence file $c not found in the 1.52.0 clone at $espkSrc. " +
                   'Shipping the modified espeak-ng.dll without its notices is what this ' +
                   'step exists to prevent - re-clone with -Force.')
        }
        Copy-Item $src $espkNotices -Force
    }
}

Write-Host '==> native-deps provisioned:'
Write-Host ("    runtime DLLs    : {0}" -f (Get-ChildItem $runtime -Filter '*.dll').Count)
# -Recurse: the wheel's notice files sit one level down (under notices\onnxruntime\), so a
# non-recursive count reads 0 and looks like a failure when provisioning actually succeeded.
Write-Host ("    ORT notices     : {0}" -f @(Get-ChildItem $notices -Recurse -File -ErrorAction SilentlyContinue).Count)
Write-Host ("    espeak notices  : {0}" -f @(Get-ChildItem $espkNotices -File -ErrorAction SilentlyContinue).Count)
Write-Host ("    espeak-ng.dll   : {0}" -f (Test-Path $espkDll))
