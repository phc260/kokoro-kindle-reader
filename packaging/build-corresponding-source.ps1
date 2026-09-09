# Build corresponding-source-<version>.zip for a binary release (GPLv3 section 6).
#
# The shipped binaries are conveyed under GPLv3 (kokoro-host.exe / kokoro-panel.exe link
# modified espeak-ng and Slint), so whoever conveys them must provide COMPLETE CORRESPONDING
# SOURCE. Attaching only the installer does not satisfy that, and pointing at an upstream
# espeak-ng TAG is fragile - it can be retagged or deleted through no fault of ours. This
# script assembles the source archive to attach beside each installer and link from the
# release body.
#
# "Complete corresponding source" here is: the modified GPL component (espeak-ng) carried IN
# FULL in this archive, the exact Rust standard-library source statically linked by the active
# toolchain, and the official NSIS 3.12 source archive for its CPL-covered LZMA module, plus
# GPLv3 section 6(d) clear-directions/equivalent-access pointers for the pieces whose upstream
# source is immutable and public - the Cargo crates (crates.io, which forbids republishing a
# version) and the permissively-licensed native runtime ORT/Dawn/DXC pulled in dynamically
# (their exact versions are pinned in components.toml). The archive is therefore not fully
# self-contained by design; the README below spells out where each off-archive piece is obtained
# so a section 6 recipient never depends on a mutable tag.
#
#   packaging\build-corresponding-source.ps1                 # version from installer.nsi
#   packaging\build-corresponding-source.ps1 -Version 0.4.0  # explicit
#
# Output: packaging\corresponding-source-<version>.zip
#
# ASCII only (PS 5.1 - see CLAUDE.md). Run AFTER build-installer.ps1 (needs its staged Rust
# toolchain record, plus the built modified espeak-ng tree provisioned by fetch-deps.ps1).
#
# RELEASE INTEGRITY: the archive's project source is exactly `git ls-files` at HEAD - tracked
# files only. So an untracked build input (a new manifest, a new packaging script) is silently
# dropped, and an uncommitted edit ships source that does not match the released binary. Both
# defeat GPLv3 section 6. By default this script therefore REFUSES to build unless the working
# tree is clean AND HEAD carries the v<Version> tag - which makes `git ls-files` provably the
# release content and makes any would-be untracked build input fail the clean-tree check as an
# untracked (??) path. -AllowUncommitted is the local dry-run escape hatch; it stamps the
# README as a NON-release build so such an archive can never be mistaken for the real thing.
param([string]$Version, [switch]$AllowUncommitted)
$ErrorActionPreference = 'Stop'

$here = $PSScriptRoot
$root = Split-Path $here -Parent

function Get-ProjectSourceManifestLines([string]$RepositoryRoot) {
    $trackedPaths = @(& git -C $RepositoryRoot ls-files)
    if ($LASTEXITCODE -ne 0 -or $trackedPaths.Count -eq 0) {
        throw 'git ls-files failed while checking installer source provenance.'
    }
    foreach ($relative in @($trackedPaths | Sort-Object)) {
        $path = Join-Path $RepositoryRoot $relative
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
            throw "Tracked source file is missing: $relative"
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

if (-not $Version) {
    # Derive from installer.nsi's !define VERSION, so this matches what build-installer ships.
    $nsi = Get-Content (Join-Path $here 'installer.nsi') -Raw
    if ($nsi -match '!define\s+VERSION\s+"([^"]+)"') { $Version = $Matches[1] }
    else { throw 'Could not read VERSION from installer.nsi; pass -Version explicitly.' }
}
Write-Host "==> Corresponding source for v$Version"

# Resolve the exact commit and enforce release integrity (see the header note).
Push-Location $root
$commit = (& git rev-parse HEAD 2>$null)
if ($LASTEXITCODE -or -not $commit) { Pop-Location; throw 'git rev-parse HEAD failed; not a git checkout?' }
$commit = $commit.Trim()
$porcelain = @(& git status --porcelain)
$tagsAtHead = @(& git tag --points-at HEAD)
Pop-Location
$expectedTag = "v$Version"
$isClean = ($porcelain.Count -eq 0)
$isTagged = ($tagsAtHead -contains $expectedTag)
$releaseClean = $isClean -and $isTagged
if (-not $releaseClean) {
    $reasons = @()
    if (-not $isClean) {
        $reasons += ("working tree is not clean ($($porcelain.Count) modified/untracked path(s)) - " +
                     'an untracked build input would be silently omitted from the archive')
    }
    if (-not $isTagged) {
        $reasons += ("HEAD does not carry tag $expectedTag (tags here: [$($tagsAtHead -join ', ')]) - " +
                     'the archive would not match the released binary')
    }
    $msg = "Refusing to build RELEASE corresponding source:`n  - " + ($reasons -join "`n  - ")
    if (-not $AllowUncommitted) {
        throw ($msg + "`nCheck out the release tag on a clean tree, or pass -AllowUncommitted for a " +
               'local (non-release) dry run.')
    }
    Write-Warning ($msg + "`n-AllowUncommitted set: continuing as a NON-RELEASE dry run (README will say so).")
}
Write-Host ("==> Source commit: {0}{1}" -f $commit, $(if ($releaseClean) { " (clean, tagged $expectedTag)" } else { ' (DEV/dirty)' }))

# Resolve the exact Rust toolchain whose precompiled standard library is linked into every
# Rust output. cargo-about enumerates Cargo packages only; rust-src is the corresponding source
# for std/core/alloc/compiler-builtins and the toolchain-generated COPYRIGHT-library.html is
# their exhaustive notice. installer.yml installs the rust-src component; fail loudly when a
# local release build omitted it.
$rustcInfo = @(& rustc --version --verbose)
if ($LASTEXITCODE -or $rustcInfo.Count -eq 0) { throw 'rustc --version --verbose failed.' }
$rustcInfoText = $rustcInfo -join "`n"
if ($rustcInfoText -notmatch '(?m)^release:\s+(\S+)\s*$') {
    throw 'Could not read the Rust release from rustc --version --verbose.'
}
$rustRelease = $Matches[1]
if ($rustcInfoText -notmatch '(?m)^commit-hash:\s+([0-9a-f]{40})\s*$') {
    throw 'Could not read the immutable Rust commit from rustc --version --verbose.'
}
$rustCommit = $Matches[1]
$builtRustToolchain = Join-Path $here 'staging\licenses\rust\TOOLCHAIN.txt'
if (-not (Test-Path -LiteralPath $builtRustToolchain -PathType Leaf)) {
    throw ('The installer build toolchain record is missing. Run packaging\build-installer.ps1 ' +
           'before creating its corresponding-source archive.')
}
$builtRustText = [System.IO.File]::ReadAllText($builtRustToolchain).Replace("`r`n", "`n").TrimEnd("`n")
$currentRustText = $rustcInfoText.Replace("`r`n", "`n").TrimEnd("`n")
if ($builtRustText -cne $currentRustText) {
    throw ('The active Rust toolchain differs from the one that built the installer. Restore ' +
           'the recorded toolchain before creating corresponding source.')
}
$rustSysroot = (& rustc --print sysroot)
if ($LASTEXITCODE -or -not $rustSysroot) { throw 'rustc --print sysroot failed.' }
$rustSysroot = $rustSysroot.Trim()
$rustLibrarySource = Join-Path $rustSysroot 'lib\rustlib\src\rust\library'
$rustStdNotice = Join-Path $rustSysroot 'share\doc\rust\COPYRIGHT-library.html'
if (-not (Test-Path -LiteralPath (Join-Path $rustLibrarySource 'std\Cargo.toml'))) {
    throw ("Rust standard-library source not found at $rustLibrarySource - install the exact " +
           "toolchain's rust-src component before building release corresponding source.")
}
if (-not (Test-Path -LiteralPath $rustStdNotice -PathType Leaf)) {
    throw "Rust standard-library copyright report not found at $rustStdNotice"
}
Write-Host "==> Rust standard library: $rustRelease ($rustCommit)"

$stage = Join-Path $here "corresponding-source-$Version"
$out = Join-Path $here "corresponding-source-$Version.zip"
Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
Remove-Item -Force $out -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $stage | Out-Null

# 1. The project's own tracked source, with Git-LFS assets RESOLVED. `git ls-files` is the
#    exact set under version control; copying those paths from the working tree (which a
#    CI checkout populates with real LFS content, not pointer stubs) is what guarantees the
#    archive ships the resolved bytes. This also naturally excludes target\ and the
#    gitignored native-deps provisioning - the espeak source is added deliberately below.
Write-Host '==> Copying tracked project source (LFS resolved)'
Push-Location $root
$tracked = & git ls-files
if ($LASTEXITCODE) { Pop-Location; throw 'git ls-files failed' }
Pop-Location
$installerStage = Join-Path $here 'staging'
$stageProvenance = Join-Path $installerStage 'provenance'
$projectBuildManifest = Join-Path $stageProvenance 'kkr-project-source.SHA256SUMS.txt'
if (-not (Test-Path -LiteralPath $projectBuildManifest -PathType Leaf)) {
    throw ('Installer project-source provenance is missing. Run packaging\build-installer.ps1 ' +
           'before creating corresponding source.')
}
$builtProjectText = ([System.IO.File]::ReadAllLines($projectBuildManifest) -join "`n")
$currentProjectText = ([string[]](Get-ProjectSourceManifestLines $root) -join "`n")
if ($builtProjectText -cne $currentProjectText) {
    throw ('The tracked project source changed after the installer binaries were built. ' +
           'Rebuild the installer before creating corresponding source.')
}
$outputBuildRecord = Join-Path $stageProvenance 'kkr-build-outputs.SHA256SUMS.txt'
if (-not (Test-Path -LiteralPath $outputBuildRecord -PathType Leaf)) {
    throw 'Installer output provenance is missing; rebuild the installer before creating source.'
}
$stagedOutputManifest = [string[]]@(
    foreach ($exeName in 'kokoro-host.exe', 'kokoro-panel.exe') {
        $exePath = Join-Path $installerStage $exeName
        "{0}  {1}" -f (Get-FileHash -LiteralPath $exePath -Algorithm SHA256).Hash.ToLower(), $exeName
    }
)
if (([System.IO.File]::ReadAllLines($outputBuildRecord) -join "`n") -cne
    ($stagedOutputManifest -join "`n")) {
    throw 'Staged executables differ from their build records; rebuild the installer.'
}
$srcDir = Join-Path $stage 'kokoro-kindle-reader'
foreach ($rel in $tracked) {
    if (-not $rel) { continue }
    $from = Join-Path $root $rel
    if (-not (Test-Path $from)) { continue } # a deleted-but-staged path; skip
    $to = Join-Path $srcDir $rel
    New-Item -ItemType Directory -Force (Split-Path $to -Parent) | Out-Null
    Copy-Item $from $to -Force
}
# Guard against shipping LFS pointer stubs: a resolved binary asset is not a 134-byte text
# file that starts with the LFS spec URL.
$icon = Join-Path $srcDir 'icons\icon.ico'
if (Test-Path $icon) {
    $head = [System.IO.File]::ReadAllBytes($icon)
    $isPointer = ($head.Length -lt 1024) -and
                 ([System.Text.Encoding]::ASCII.GetString($head[0..([Math]::Min(63, $head.Length - 1))]) -match 'git-lfs')
    if ($isPointer) {
        throw ('icons\icon.ico is a Git-LFS POINTER, not the resolved file. Check out with ' +
               'lfs:true (run: git lfs pull) before building the source archive.')
    }
}

# 2. The MODIFIED espeak-ng tree actually built (the post-revert source), minus its .git and
#    build output. This is the load-bearing part of section 6: the GPL component whose source
#    would otherwise depend on an upstream tag staying put. Recorded with a SHA-256 manifest.
$espkSrc = Join-Path $root 'native-deps\espeak-ng-src'
if (-not (Test-Path (Join-Path $espkSrc 'phsource\ph_english_us'))) {
    throw ("Modified espeak-ng source not found at $espkSrc - run native-deps\fetch-deps.ps1 " +
           'first (it fetches the 1.52.0 commit and applies the horse-hoarse revert).')
}
$espkProvision = Join-Path $stageProvenance 'ESPEAK-PROVISION.txt'
$espkBuildScriptHash = Get-NormalizedTextSha256 (Join-Path $root 'native-deps\build-espeak.py')
$espkProvisionExpected = ('espeak-ng=1.52.0+horse-hoarse-revert;' +
                          'base=4870adfa25b1a32b4361592f1be8a40337c58d6c' + "`n" +
                          "build-script-sha256=$espkBuildScriptHash")
$espkProvisionActual = if (Test-Path -LiteralPath $espkProvision -PathType Leaf) {
    [System.IO.File]::ReadAllText($espkProvision).Replace("`r`n", "`n").Replace("`r", "`n").TrimEnd("`n")
} else { '' }
$builtEspkManifest = Join-Path $stageProvenance 'espeak-ng-source.SHA256SUMS.txt'
if (-not (Test-Path -LiteralPath $espkProvision -PathType Leaf) -or
    $espkProvisionActual -cne $espkProvisionExpected -or
    -not (Test-Path -LiteralPath $builtEspkManifest -PathType Leaf) -or
    (Get-Item -LiteralPath $builtEspkManifest).Length -eq 0) {
    throw ('The installer has no matching espeak-ng provenance/source manifest. Run ' +
           'native-deps\fetch-deps.ps1 and rebuild the installer before creating source.')
}
Write-Host '==> Copying modified espeak-ng source (excluding .git and build output)'
$espkDest = Join-Path $stage 'espeak-ng-modified-1.52.0'
$excluded = @('.git', 'build-x64', 'build')
Get-ChildItem $espkSrc -Force | Where-Object { $excluded -notcontains $_.Name } | ForEach-Object {
    Copy-Item $_.FullName (Join-Path $espkDest $_.Name) -Recurse -Force
}
# SHA-256 manifest of the shipped espeak source, so a recipient can confirm they have the
# exact modified tree (the DLL itself is not byte-pinned - see THIRD_PARTY_NOTICES.md).
Write-Host '==> Hashing espeak-ng source tree'
$manifest = Join-Path $stage 'espeak-ng-modified-1.52.0.SHA256SUMS.txt'
Get-ChildItem $espkDest -Recurse -Force -File | Sort-Object FullName | ForEach-Object {
    $rel = $_.FullName.Substring($espkDest.Length).TrimStart('\', '/').Replace('\', '/')
    "{0}  {1}" -f (Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLower(), $rel
} | Set-Content -Encoding ascii $manifest
$builtManifestText = [System.IO.File]::ReadAllText($builtEspkManifest).Replace("`r`n", "`n")
$archiveManifestText = [System.IO.File]::ReadAllText($manifest).Replace("`r`n", "`n")
if ($archiveManifestText -cne $builtManifestText) {
    throw ('The espeak-ng source tree differs from the one staged in the installer. Restore ' +
           'that source, or re-provision and rebuild the installer before creating source.')
}

# 3. The exact Rust standard-library source linked into all five Rust outputs. rust-src is
#    target-independent, so one copy covers both the x64 and x86 precompiled standard libraries
#    from this toolchain. Include the toolchain's generated copyright report beside it: unlike
#    Cargo packages, these sources never enter cargo-about's graph.
$rustPathVersion = [regex]::Replace($rustRelease, '[^A-Za-z0-9._-]', '-')
$rustDest = Join-Path $stage "rust-standard-library-$rustPathVersion"
$rustLibraryDest = Join-Path $rustDest 'library'
Write-Host "==> Copying Rust standard-library source ($rustRelease)"
New-Item -ItemType Directory -Force $rustLibraryDest | Out-Null
Get-ChildItem -LiteralPath $rustLibrarySource -Force | ForEach-Object {
    Copy-Item $_.FullName (Join-Path $rustLibraryDest $_.Name) -Recurse -Force
}
Copy-Item -LiteralPath $rustStdNotice (Join-Path $rustDest 'COPYRIGHT-library.html') -Force
[System.IO.File]::WriteAllLines(
    (Join-Path $rustDest 'TOOLCHAIN.txt'),
    [string[]]$rustcInfo,
    [System.Text.Encoding]::ASCII
)
Write-Host '==> Hashing Rust standard-library source tree'
$rustManifest = Join-Path $stage "rust-standard-library-$rustPathVersion.SHA256SUMS.txt"
Get-ChildItem $rustDest -Recurse -File | ForEach-Object {
    $rel = $_.FullName.Substring($rustDest.Length).TrimStart('\', '/')
    "{0}  {1}" -f (Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLower(), $rel
} | Set-Content -Encoding ascii $rustManifest

# 4. NSIS's LZMA compression module is CPL-1.0 with a linking exception. The exception keeps
#    the installed application out of CPL, but the module itself remains CPL and its object-code
#    terms require us to state that source is available and explain how to obtain it. Carry the
#    exact official 3.12 source archive here instead of relying only on an upstream link that can
#    move. Hash it so a SourceForge error page or substituted download fails the release.
$nsisSourceUrl = 'https://downloads.sourceforge.net/nsis/nsis-3.12-src.tar.bz2'
$nsisSourceExpected = 'f3ed7a8e4aa2cf4e8cf47d3b563a02559e0cb4934db2662b2f9661b824e2b186'
$nsisSourceDest = Join-Path $stage 'nsis-3.12-src.tar.bz2'
Write-Host '==> Downloading NSIS 3.12 source'
$webClient = New-Object System.Net.WebClient
$webClient.Headers['User-Agent'] = 'Kokoro-Kindle-Reader-source-packager/1.0'
try {
    $webClient.DownloadFile($nsisSourceUrl, $nsisSourceDest)
} finally {
    $webClient.Dispose()
}
$nsisSourceActual = (Get-FileHash -LiteralPath $nsisSourceDest -Algorithm SHA256).Hash.ToLower()
if ($nsisSourceActual -cne $nsisSourceExpected) {
    throw ("NSIS 3.12 source SHA-256 is $nsisSourceActual, expected $nsisSourceExpected. " +
           'Refusing to publish an unverified or non-source download.')
}

# 5. The README with deterministic-build directions and the lockfile-immutability note that
#    stands in for `cargo vendor` (the crates.io versions pinned in the committed Cargo.lock
#    files ARE the corresponding source; crates.io is immutable).
$provenance = if ($releaseClean) {
    "Built from a clean checkout at tag $expectedTag, commit $commit."
} else {
    "*** NON-RELEASE DEV BUILD (built with -AllowUncommitted from an untagged or dirty tree). ***`n" +
    "*** Source commit $commit; this archive does NOT necessarily match any released binary. ***"
}
$readme = @"
Corresponding source for Kokoro Kindle Reader v$Version
=======================================================

$provenance

This archive is the complete corresponding source (GPLv3 section 6) for the GPL-covered
binaries in the matching release: kokoro-host.exe, kokoro-panel.exe, and the modified
espeak-ng.dll + espeak-ng-data/.

Contents
--------
- kokoro-kindle-reader/            The project source at tag v$Version (Git-LFS resolved),
                                   including all Cargo.lock files, native-deps/*.ps1 and
                                   packaging/*.ps1 build and provisioning scripts.
- espeak-ng-modified-1.52.0/       The exact modified espeak-ng source that was built: the
                                   1.52.0 tree with the horse-hoarse revert in
                                   phsource/ph_english_us (see build-espeak.py). Its .git
                                   and build output are excluded.
- espeak-ng-modified-1.52.0.SHA256SUMS.txt   SHA-256 of every file in that tree.
- rust-standard-library-$rustPathVersion/   The exact rust-src library/ tree for rustc
                                   $rustRelease, commit $rustCommit, plus that toolchain's
                                   generated COPYRIGHT-library.html and TOOLCHAIN.txt.
- rust-standard-library-$rustPathVersion.SHA256SUMS.txt   SHA-256 of every file in that tree.
- nsis-3.12-src.tar.bz2           The exact official NSIS 3.12 source archive, including the
                                   CPL-1.0 LZMA compression module used by the installer stub.

Cargo dependencies
------------------
The Cargo crates statically linked into the executables are the immutable crates.io versions
pinned in the committed Cargo.lock files (kokoro-host/Cargo.lock and kokoro-panel/Cargo.lock
for the GPL-covered exes). crates.io does not permit republishing a version, so those pins
ARE the corresponding source; ``cargo build`` against the included lockfiles fetches exactly
them. Slint's version is recorded in kokoro-panel/Cargo.lock (used unmodified, under its
GPL-3.0-only option). To materialize them offline: ``cargo vendor`` from each crate dir.

Rust standard library
---------------------
Every Rust output also statically links the standard library supplied by rustc $rustRelease
(commit $rustCommit). That code is outside Cargo's package graph, so it is included above in
full from this exact toolchain's rust-src component. COPYRIGHT-library.html is Rust's generated
licence and copyright inventory for that library source and its bundled dependencies.

NSIS
----
The installer/uninstaller stub is built with NSIS 3.12 (pinned in
.github/workflows/installer.yml). NSIS is not GPL and not linked into the executables; its
exact official source is the included nsis-3.12-src.tar.bz2 (SHA-256
$nsisSourceExpected). Its licence (incl. the LZMA CPL-1.0 linking exception) ships as
licenses/nsis/NSIS-COPYING.txt in the installer.

Native runtime (ONNX Runtime / Dawn / DXC)
------------------------------------------
The synth loads onnxruntime.dll (+ onnxruntime_providers_shared.dll, dxcompiler.dll,
dxil.dll) dynamically at runtime; Dawn/Tint are statically linked inside onnxruntime.dll.
These are permissively licensed (ORT: MIT; Dawn/Tint: BSD-3-Clause; DXC: NCSA plus bundled
third-party terms - see licenses/ in the installer), not GPL, and are not modified by this
project. They are identified below by IMMUTABLE commit / version IDs - not a git tag, which
can be retargeted
(this script's own header warns of exactly that) - so the exact source stays recoverable:
  - ONNX Runtime 1.27.0 = the cp312 win_amd64 wheel named in components.toml, SHA-256
    7ef99275b13e8cb9584bd0db7a6f00ebf76095601eeccf7d34749b89ee991c19; source is git
    commit 8f0278c77bf44b0cc83c098c6c722b92a36ac4b5 at
    https://github.com/microsoft/onnxruntime (the shipped DLL's build string
    1.27.20260615.2.8f0278c embeds that commit). ORT pins its OWN native deps by commit +
    archive hash in cmake/deps.txt at that commit; that file is the authoritative record of
    the exact revisions below.
  - Dawn (which contains Tint) = git commit ec7b457e5bb1fcec6f59733c4f3dd84d2f885a38 at
    https://github.com/google/dawn (archive SHA1 d4d64d1729104b61e654073566f1a376e16cad92,
    per ORT's cmake/deps.txt at the commit above).
  - DirectX Shader Compiler = the Microsoft.Direct3D.DXC prebuilt redistributable bundled in
    the ORT wheel, at https://github.com/microsoft/DirectXShaderCompiler (not built from ORT
    source). Each DLL's version resource embeds the short commit prefix; the full immutable
    commits are:
      - dxcompiler.dll v1.9.0.1     = git commit 3e6e148537683c22e3e74977d56516f16f39c7be
                                      (version resource: "1.9.0.1 (3e6e1485)")
      - dxil.dll       v1.8.2502.11 = git commit 2399215226737c64e76afc55ffd874ecd6fc459f
                                      (version resource: "1.8.2502.11 (239921522)")
The same immutable IDs are recorded in packaging/components.toml (included in the project
source above).

Rebuilding
----------
1. Install Git, CMake + MSVC, NSIS 3.12, and rustup. Install the recorded Rust toolchain:
   ``rustup toolchain install $rustRelease --component rust-src --target i686-pc-windows-msvc``.
2. Open PowerShell in the extracted kokoro-kindle-reader/ directory and run
   ``rustup override set $rustRelease``. Verify ``rustc --version --verbose`` reports
   commit $rustCommit. Then install
   ``cargo install cargo-about --version 0.9.1 --locked --features cli``.
3. This source archive has resolved Git-LFS assets but no .git directory. Initialize the
   local file index needed by the packaging scripts: ``git init`` then ``git add --all``.
   No commit, user identity, or remote is required to build the installer.
4. Run ``native-deps\fetch-deps.ps1`` and then ``packaging\build-installer.ps1``. The
   provisioner fetches the exact upstream espeak commit and reapplies the documented patch;
   it also downloads the pinned ORT wheel. OCR models download at app runtime.
   The included espeak-ng-modified-1.52.0/ is the matching modified source for inspection
   and modification, not a Git checkout to copy over native-deps/espeak-ng-src/.
See ARCHITECTURE.md and packaging/README.md in the project source for detail.

The build is not bit-for-bit reproducible (espeak-ng is built with whatever MSVC the runner
has; see THIRD_PARTY_NOTICES.md), but the source and configuration here reproduce a
functionally identical build.
"@
Set-Content -Encoding ascii (Join-Path $stage 'README.txt') $readme

# 6. Zip it.
Write-Host '==> Compressing'
Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $out -Force
Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
$z = Get-Item $out
Write-Host ("==> Corresponding source: {0}  ({1:N1} MB)" -f $z.FullName, ($z.Length / 1MB))
