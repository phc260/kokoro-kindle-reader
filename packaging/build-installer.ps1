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
$nativeRuntime = Join-Path $root 'native-deps\runtime'
$hostRel = Join-Path $root 'kokoro-host\target\release'
$panelRel = Join-Path $root 'kokoro-panel\target\release'
$sapiRs = Join-Path $root 'kokoro-sapi'

function Get-ProjectSourceManifestLines([string]$RepositoryRoot) {
    $tracked = @(& git -C $RepositoryRoot ls-files)
    if ($LASTEXITCODE -ne 0 -or $tracked.Count -eq 0) {
        throw 'git ls-files failed while recording installer source provenance.'
    }
    foreach ($relative in @($tracked | Sort-Object)) {
        $path = Join-Path $RepositoryRoot $relative
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
            throw "Tracked build input is missing: $relative"
        }
        "{0}  {1}" -f (Get-FileHash $path -Algorithm SHA256).Hash.ToLower(), $relative
    }
}

function Get-NormalizedTextSha256([string]$Path) {
    $encoding = New-Object System.Text.UTF8Encoding($false, $true)
    $text = $encoding.GetString([System.IO.File]::ReadAllBytes($Path))
    $normalized = $text.Replace("`r`n", "`n").Replace("`r", "`n")
    $algorithm = [System.Security.Cryptography.SHA256]::Create()
    try {
        return [System.BitConverter]::ToString(
            $algorithm.ComputeHash($encoding.GetBytes($normalized))
        ).Replace('-', '').ToLower()
    } finally {
        $algorithm.Dispose()
    }
}

# 0. Fail fast on inventory drift: the in-repo assets pinned in components.toml (the five
#    Material Symbols SVGs compiled into kokoro-panel.exe) must still hash to their recorded
#    SHA-256. license-check.yml runs this on PRs, but a tag/manual build can start from a
#    commit that never went through a PR - so run it here too, before the long build, rather
#    than shipping a stale inventory. (verify-installer-notices.ps1 later proves the notice
#    tree is IN the built -setup.exe.)
Write-Host '==> Verifying non-Cargo component hashes (components.toml)'
& (Join-Path $here 'verify-component-hashes.ps1')  # throws on drift ($ErrorActionPreference=Stop)

#    The static licence texts are pinned separately from the component payloads. A file can
#    be present and non-empty while still being truncated or copied from the wrong upstream
#    revision; verify their reviewed content before spending time on any build.
Write-Host '==> Verifying checked-in licence texts'
& (Join-Path $here 'verify-license-texts.ps1')

#    Check the installer toolchain before compiling any Rust output. Its stub,
#    COPYING, and corresponding-source archive must all describe NSIS 3.12.
Write-Host '==> Checking NSIS toolchain (3.12)'
$makensis = 'C:\Program Files (x86)\NSIS\makensis.exe'
if (-not (Test-Path $makensis)) { throw "makensis not found at $makensis - install NSIS." }
$nsisVersion = (& $makensis /VERSION)
$nsisVersionText = (($nsisVersion | Out-String).Trim())
if ($LASTEXITCODE -ne 0 -or $nsisVersionText -cne 'v3.12') {
    throw ("NSIS version mismatch at $makensis (found '$nsisVersionText', expected " +
           "'v3.12'). Install the pinned 3.12 toolchain so the stub and staged COPYING match " +
           'packaging/components.toml and the corresponding-source instructions.')
}
$nsisCopying = Join-Path (Split-Path $makensis -Parent) 'COPYING'
if (-not (Test-Path -LiteralPath $nsisCopying -PathType Leaf) -or
    (Get-Item -LiteralPath $nsisCopying -ErrorAction SilentlyContinue).Length -eq 0) {
    throw ("NSIS COPYING not found at $nsisCopying - the installed NSIS is missing its " +
           'licence file; the LZMA-compressed stub must ship NSIS''s licence terms.')
}

#    Provisioned notices must be current too. Check the canonical ORT anchors before any
#    cargo build so a stale dependency cache costs seconds, not a full release build.
$ortNotices = Join-Path $nativeRuntime 'notices'
$ortProvision = Join-Path $nativeRuntime 'ORT-PROVISION.txt'
$ortProvisionExpected = ('onnxruntime-webgpu=1.27.0' + "`n" +
                         'wheel=onnxruntime_webgpu-1.27.0-cp312-cp312-win_amd64.whl' + "`n" +
                         'wheel-sha256=7ef99275b13e8cb9584bd0db7a6f00ebf76095601eeccf7d34749b89ee991c19')
$ortProvisionActual = if (Test-Path -LiteralPath $ortProvision -PathType Leaf) {
    [System.IO.File]::ReadAllText($ortProvision).Replace("`r`n", "`n").Replace("`r", "`n").TrimEnd("`n")
} else { '' }
if (-not (Test-Path -LiteralPath $ortProvision -PathType Leaf) -or
    $ortProvisionActual -cne $ortProvisionExpected) {
    throw ("ONNX Runtime provision is not $ortProvisionExpected - run " +
           'native-deps\fetch-deps.ps1 so the binaries and notices match components.toml.')
}
$espkProvision = Join-Path $nativeRuntime 'ESPEAK-PROVISION.txt'
$espkBuildScriptHash = Get-NormalizedTextSha256 (Join-Path $root 'native-deps\build-espeak.py')
$espkProvisionExpected = ('espeak-ng=1.52.0+horse-hoarse-revert;' +
                          'base=4870adfa25b1a32b4361592f1be8a40337c58d6c' + "`n" +
                          "build-script-sha256=$espkBuildScriptHash")
$espkProvisionActual = if (Test-Path -LiteralPath $espkProvision -PathType Leaf) {
    [System.IO.File]::ReadAllText($espkProvision).Replace("`r`n", "`n").Replace("`r", "`n").TrimEnd("`n")
} else { '' }
$espkSourceManifest = Join-Path $nativeRuntime 'espeak-ng-source.SHA256SUMS.txt'
$espkRuntimeData = Join-Path $nativeRuntime 'espeak-ng-data'
if (-not (Test-Path -LiteralPath $espkProvision -PathType Leaf) -or
    $espkProvisionActual -cne $espkProvisionExpected -or
    -not (Test-Path -LiteralPath $espkSourceManifest -PathType Leaf) -or
    (Get-Item -LiteralPath $espkSourceManifest -ErrorAction SilentlyContinue).Length -eq 0 -or
    -not (Test-Path -LiteralPath $espkRuntimeData -PathType Container) -or
    $null -eq (Get-ChildItem -LiteralPath $espkRuntimeData -Recurse -File |
               Select-Object -First 1)) {
    throw ("espeak-ng provision is not $espkProvisionExpected with a source manifest - run " +
           'native-deps\fetch-deps.ps1 so the binary and corresponding source stay paired.')
}
$missingRuntimeDlls = @()
foreach ($dllName in 'onnxruntime.dll', 'onnxruntime_providers_shared.dll', 'dxcompiler.dll',
                      'dxil.dll', 'espeak-ng.dll') {
    $dllPath = Join-Path $nativeRuntime $dllName
    if (-not (Test-Path -LiteralPath $dllPath -PathType Leaf) -or
        (Get-Item -LiteralPath $dllPath -ErrorAction SilentlyContinue).Length -eq 0) {
        $missingRuntimeDlls += $dllName
    }
}
if ($missingRuntimeDlls.Count) {
    throw ("Native runtime provision is incomplete at $nativeRuntime (missing or empty: " +
           "$($missingRuntimeDlls -join ', ')) - run native-deps\fetch-deps.ps1.")
}
$missingOrtNotices = @()
foreach ($noticeName in 'ORT-LICENSE.txt', 'ORT-ThirdPartyNotices.txt') {
    $noticePath = Join-Path $ortNotices $noticeName
    if (-not (Test-Path -LiteralPath $noticePath -PathType Leaf) -or
        (Get-Item -LiteralPath $noticePath -ErrorAction SilentlyContinue).Length -eq 0) {
        $missingOrtNotices += $noticeName
    }
}
if ($missingOrtNotices.Count) {
    throw ("ONNX Runtime notice provision is missing or stale at $ortNotices " +
           "($($missingOrtNotices -join ', ')) - run native-deps\fetch-deps.ps1. " +
           'It provisions the canonical files from the same wheel as the runtime DLLs.')
}

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

# `-SkipBuild` is safe only when the reusable host/panel outputs were built from this exact
# tracked tree and Rust toolchain. Without these records, a clean release checkout could pair
# old target/ binaries with newer corresponding source while every Git check still passed.
$projectBuildManifest = Join-Path $hostRel 'kkr-project-source.SHA256SUMS.txt'
$rustBuildRecord = Join-Path $hostRel 'kkr-build-rustc.txt'
$outputBuildRecord = Join-Path $hostRel 'kkr-build-outputs.SHA256SUMS.txt'
$currentProjectManifest = [string[]](Get-ProjectSourceManifestLines $root)
$currentRustc = @(& rustc --version --verbose)
if ($LASTEXITCODE -ne 0 -or $currentRustc.Count -eq 0) { throw 'rustc --version --verbose failed.' }
# A standalone cargo build can replace either exe without touching the source/toolchain
# records. Bind those records to the actual outputs before accepting -SkipBuild.
$currentOutputManifest = [string[]]@(
    foreach ($exePath in (Join-Path $hostRel 'kokoro-host.exe'),
                        (Join-Path $panelRel 'kokoro-panel.exe')) {
        "{0}  {1}" -f (Get-FileHash -LiteralPath $exePath -Algorithm SHA256).Hash.ToLower(),
                       (Split-Path $exePath -Leaf)
    }
)
if ($SkipBuild) {
    if (-not (Test-Path -LiteralPath $projectBuildManifest -PathType Leaf) -or
        -not (Test-Path -LiteralPath $rustBuildRecord -PathType Leaf) -or
        -not (Test-Path -LiteralPath $outputBuildRecord -PathType Leaf)) {
        throw '-SkipBuild requires provenance from a prior successful full build.'
    }
    $builtProjectText = ([System.IO.File]::ReadAllLines($projectBuildManifest) -join "`n")
    $currentProjectText = ($currentProjectManifest -join "`n")
    $builtRustcText = ([System.IO.File]::ReadAllLines($rustBuildRecord) -join "`n")
    $currentRustcText = ($currentRustc -join "`n")
    $builtOutputText = ([System.IO.File]::ReadAllLines($outputBuildRecord) -join "`n")
    if ($builtProjectText -cne $currentProjectText -or $builtRustcText -cne $currentRustcText -or
        $builtOutputText -cne ($currentOutputManifest -join "`n")) {
        throw '-SkipBuild provenance does not match this source tree/toolchain/output; run a full build.'
    }
} else {
    [System.IO.File]::WriteAllLines(
        $projectBuildManifest,
        $currentProjectManifest,
        [System.Text.Encoding]::ASCII
    )
    [System.IO.File]::WriteAllLines(
        $rustBuildRecord,
        [string[]]$currentRustc,
        [System.Text.Encoding]::ASCII
    )
    [System.IO.File]::WriteAllLines(
        $outputBuildRecord,
        $currentOutputManifest,
        [System.Text.Encoding]::ASCII
    )
}

# 3. Stage the bundle.
$stage = Join-Path $here 'staging'
Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $stage, (Join-Path $stage 'resources') | Out-Null

Copy-Item (Join-Path $hostRel 'kokoro-host.exe') $stage
Copy-Item (Join-Path $panelRel 'kokoro-panel.exe') $stage
# Installer staging reads the provision directly, not the host target directory. With
# -SkipBuild, the latter can contain DLLs copied by an older host build and therefore bypass
# the version markers checked above.
foreach ($d in 'onnxruntime.dll', 'onnxruntime_providers_shared.dll', 'dxcompiler.dll', 'dxil.dll', 'espeak-ng.dll') {
    Copy-Item (Join-Path $nativeRuntime $d) $stage
}
Copy-Item -Recurse $espkRuntimeData $stage
Copy-Item (Join-Path $root 'icons\icon.ico') (Join-Path $stage 'icon.ico')

# Freeze the build records beside this staging tree. Source packaging must describe this
# installer even if another build or native provision replaces target/runtime in between.
# This directory is packaging metadata; installer.nsi does not install it.
$stageProvenance = Join-Path $stage 'provenance'
New-Item -ItemType Directory -Force $stageProvenance | Out-Null
foreach ($record in $projectBuildManifest, $outputBuildRecord, $espkProvision, $espkSourceManifest) {
    Copy-Item -LiteralPath $record -Destination $stageProvenance -Force
}

# 3a. The Cloud Reader OCR models are NOT bundled. Like the Kokoro voice model, they are
#     DOWNLOADED at first run - by the panel, into <app_data>/ocr/, per ocr-manifest.json and
#     SHA-256-verified (kokoro-panel::download). So there is nothing to stage here: a fresh
#     install ships no ocr\ dir, the host answers /ocr with `missing` until the download
#     lands, and the browser extension surfaces that state. This keeps ~10 MB of a
#     browser-only asset out of the installer (and out of every Kindle-only user's download).
#     fetch-ocr-models.py still provisions them for DEV (`cargo run` reads native-deps\ocr).

# 3b. License texts. The bundle links espeak-ng (GPL-3.0-or-later, and MODIFIED -- see
#     native-deps\build-espeak.py) and Slint under its GPL-3.0-only option, so the
#     installed app as a whole is conveyed under GPLv3: the notices + the GPL text must
#     ship WITH the binaries, not just live in the repo. THIRD_PARTY_NOTICES.md links
#     LICENSE and licenses\*; the tray/panel open legal.html alongside them.
Copy-Item (Join-Path $root 'LICENSE') $stage
Copy-Item (Join-Path $root 'THIRD_PARTY_NOTICES.md') $stage
Copy-Item (Join-Path $root 'legal.html') $stage
Copy-Item -Recurse (Join-Path $root 'licenses') $stage

#     ONNX Runtime's own licence + notice set, staged from native-deps rather than kept in
#     the repo, so it stays matched to the exact wheel the shipped DLLs came out of. Four
#     of the binaries we install come from that wheel, and dxcompiler.dll's licence
#     (University of Illinois/NCSA) requires its notice accompany them.
#     The fail-fast check above already proved the canonical files exist and are non-empty.
#     Throwing beats shipping without: an installer missing licence text looks complete and
#     is not, which is the same trap as the OCR models above.
$ortStage = Join-Path $stage 'licenses\onnxruntime'
New-Item -ItemType Directory -Force $ortStage | Out-Null
# -Recurse: fetch-deps.ps1 now preserves the wheel's own directory structure under
# native-deps\runtime\notices\ (collision-proofing - see its comment), so a flat
# wildcard copy would silently drop everything inside a subdirectory.
Copy-Item (Join-Path $ortNotices '*') $ortStage -Force -Recurse

#     espeak-ng's own COPYING* set (GPLv3 + Apache + BSD2 + UCD), provisioned by
#     fetch-deps.ps1 from the exact 1.52.0 clone we build. We ship a MODIFIED espeak-ng.dll
#     + espeak-ng-data/, and COPYING.UCD in particular covers the Unicode data baked into
#     espeak-ng-data/ and is a DIFFERENT document from licenses\Unicode-3.0.txt. Same
#     fail-loud contract as the ORT notices: missing text looks complete and is not.
$espkNotices = Join-Path $root 'native-deps\espeak-ng-notices'
$missingEspkNotices = @()
foreach ($noticeName in 'COPYING', 'COPYING.APACHE', 'COPYING.BSD2', 'COPYING.UCD') {
    $noticePath = Join-Path $espkNotices $noticeName
    if (-not (Test-Path -LiteralPath $noticePath -PathType Leaf) -or
        (Get-Item -LiteralPath $noticePath -ErrorAction SilentlyContinue).Length -eq 0) {
        $missingEspkNotices += $noticeName
    }
}
if ($missingEspkNotices.Count) {
    throw ("Incomplete espeak-ng notices at $espkNotices - run native-deps\fetch-deps.ps1 " +
           "(missing or empty: $($missingEspkNotices -join ', ')). It provisions the " +
           'exact COPYING* set alongside the espeak build.')
}
$espkStage = Join-Path $stage 'licenses\espeak-ng'
New-Item -ItemType Directory -Force $espkStage | Out-Null
Copy-Item (Join-Path $espkNotices '*') $espkStage -Force

#     The Cargo dependency closure's own licence notices - generated fresh from the
#     Cargo.lock files this build just compiled against, not a hand-maintained prose
#     list (see generate-dependency-licenses.ps1 for why, and THIRD_PARTY_NOTICES.md's
#     "Cargo crates" section for the human-readable pointer to it). Regenerating on every
#     build, rather than provisioning once like the ORT notices, is deliberate: this
#     closure moves with ordinary `cargo update`s in a way the exact pinned ORT wheel does
#     not, and a stale copy here is exactly the kind of drift this mechanism exists to
#     catch instead of silently missing.
Write-Host '==> Generating Rust dependency licence notices'
& (Join-Path $root 'packaging\generate-dependency-licenses.ps1')
if ($LASTEXITCODE) { throw 'generate-dependency-licenses.ps1 failed' }
$depLicSrc = Join-Path $here 'dependency-licenses'
$depLicStage = Join-Path $stage 'licenses\dependencies'
New-Item -ItemType Directory -Force $depLicStage | Out-Null
Copy-Item (Join-Path $depLicSrc '*.html') $depLicStage -Force

#     The Rust standard library is statically linked into every Rust output but is NOT a
#     Cargo package, so cargo-about cannot see it. Rust ships a generated per-toolchain
#     COPYRIGHT-library.html that covers std/core/alloc/compiler-builtins and their bundled
#     source dependencies. Stage that exact file from the rustc sysroot used for this build,
#     plus rustc's release and immutable commit, rather than checking in a copy that would
#     drift whenever the `stable` toolchain moves.
$rustSysroot = (& rustc --print sysroot)
if ($LASTEXITCODE -or -not $rustSysroot) { throw 'rustc --print sysroot failed.' }
$rustSysroot = $rustSysroot.Trim()
$rustStdNotice = Join-Path $rustSysroot 'share\doc\rust\COPYRIGHT-library.html'
if (-not (Test-Path -LiteralPath $rustStdNotice -PathType Leaf)) {
    throw ("Rust standard-library notice not found at $rustStdNotice - the Rust toolchain " +
           'cannot be redistributed without its generated copyright/licence report.')
}
$rustNoticeText = [System.IO.File]::ReadAllText($rustStdNotice)
if (-not $rustNoticeText.Contains('Copyright notices for The Rust Standard Library')) {
    throw "Unexpected Rust standard-library notice content: $rustStdNotice"
}
$rustToolchain = @(& rustc --version --verbose)
if ($LASTEXITCODE -or $rustToolchain.Count -eq 0 -or
    -not (($rustToolchain -join "`n") -match '(?m)^commit-hash:\s+[0-9a-f]{40}\s*$')) {
    throw 'Could not record rustc release + immutable commit for the shipped standard library.'
}
$rustStage = Join-Path $stage 'licenses\rust'
New-Item -ItemType Directory -Force $rustStage | Out-Null
Copy-Item -LiteralPath $rustStdNotice (Join-Path $rustStage 'COPYRIGHT-library.html') -Force
[System.IO.File]::WriteAllLines(
    (Join-Path $rustStage 'TOOLCHAIN.txt'),
    [string[]]$rustToolchain,
    [System.Text.Encoding]::ASCII
)

$res = Join-Path $stage 'resources'
Copy-Item $sapiDll $res
Copy-Item $hookDll $res
Copy-Item $injectExe $res
Copy-Item (Join-Path $sapiRs 'kindle-voice-guard.ps1') $res
Copy-Item (Join-Path $sapiRs 'voice-setup.ps1') $res

# 4. Compile the installer.
#     NSIS's own licence. The installer/uninstaller stub is NSIS, compressed with LZMA
#     (installer.nsi: SetCompressor /SOLID lzma), so the shipped stub carries NSIS's
#     zlib/libpng + bzip2 + CPL-1.0 (LZMA module, with its linking exception) terms. Ship
#     NSIS's own COPYING verbatim from the installed toolchain, so it always matches the
#     NSIS version this build used (pinned in CI) rather than a checked-in copy that goes
#     stale on a version bump - same reasoning as the ORT/espeak notices.
$nsisStage = Join-Path $stage 'licenses\nsis'
New-Item -ItemType Directory -Force $nsisStage | Out-Null
Copy-Item $nsisCopying (Join-Path $nsisStage 'NSIS-COPYING.txt') -Force

Write-Host '==> makensis'
& $makensis (Join-Path $here 'installer.nsi')
if ($LASTEXITCODE) { throw 'makensis failed' }

$out = Get-ChildItem (Join-Path $here '*-setup.exe') | Sort-Object LastWriteTime -Descending | Select-Object -First 1
Write-Host "==> Installer: $($out.FullName)  ($([math]::Round($out.Length/1MB,1)) MB)"
