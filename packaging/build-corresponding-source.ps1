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
# FULL in this archive, plus GPLv3 section 6(d) clear-directions/equivalent-access pointers
# for the pieces whose upstream source is immutable and public - the Rust crates (crates.io,
# which forbids republishing a version), NSIS, and the permissively-licensed native runtime
# ORT/Dawn/DXC pull in dynamically (their exact versions are pinned in components.toml). The
# archive is therefore NOT fully self-contained by design; the README below spells out where
# each off-archive piece is obtained so a section 6 recipient never depends on a mutable tag.
#
#   packaging\build-corresponding-source.ps1                 # version from installer.nsi
#   packaging\build-corresponding-source.ps1 -Version 0.4.0  # explicit
#
# Output: packaging\corresponding-source-<version>.zip
#
# ASCII only (PS 5.1 - see CLAUDE.md). Run AFTER fetch-deps.ps1 (needs the built modified
# espeak-ng tree under native-deps\espeak-ng-src).
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
           'first (it clones tag 1.52.0 and build-espeak.ps1 applies the horse-hoarse revert).')
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
Get-ChildItem $espkDest -Recurse -File | ForEach-Object {
    $rel = $_.FullName.Substring($espkDest.Length).TrimStart('\', '/')
    "{0}  {1}" -f (Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLower(), $rel
} | Set-Content -Encoding ascii $manifest

# 3. The README with deterministic-build directions and the lockfile-immutability note that
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
                                   phsource/ph_english_us (see build-espeak.ps1). Its .git
                                   and build output are excluded.
- espeak-ng-modified-1.52.0.SHA256SUMS.txt   SHA-256 of every file in that tree.

Rust dependencies
-----------------
The Rust crates statically linked into the executables are the immutable crates.io versions
pinned in the committed Cargo.lock files (kokoro-host/Cargo.lock and kokoro-panel/Cargo.lock
for the GPL-covered exes). crates.io does not permit republishing a version, so those pins
ARE the corresponding source; ``cargo build`` against the included lockfiles fetches exactly
them. Slint's version is recorded in kokoro-panel/Cargo.lock (used unmodified, under its
GPL-3.0-only option). To materialize them offline: ``cargo vendor`` from each crate dir.

NSIS
----
The installer/uninstaller stub is built with NSIS 3.11 (pinned in
.github/workflows/installer.yml). NSIS is not GPL and not linked into the executables; its
source for that version is at https://sourceforge.net/projects/nsis/files/NSIS%203/3.11/ and
its licence (incl. the LZMA CPL-1.0 linking exception) ships as licenses/nsis/NSIS-COPYING.txt
in the installer.

Native runtime (ONNX Runtime / Dawn / DXC)
------------------------------------------
The synth loads onnxruntime.dll (+ onnxruntime_providers_shared.dll, dxcompiler.dll,
dxil.dll) dynamically at runtime; Dawn/Tint are statically linked inside onnxruntime.dll.
These are permissively licensed (ORT: MIT; Dawn/Tint: BSD-3-Clause; DXC: NCSA - see
licenses/ in the installer), not GPL, and are not modified by this project. They are
identified below by IMMUTABLE commit / version IDs - not a git tag, which can be retargeted
(this script's own header warns of exactly that) - so the exact source stays recoverable:
  - ONNX Runtime 1.27.0 = git commit 8f0278c77bf44b0cc83c098c6c722b92a36ac4b5 at
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
1. Install: Rust (stable, with the i686-pc-windows-msvc target), Python 3.12, CMake + MSVC,
   NSIS 3.11, and ``cargo install cargo-about --version 0.9.1 --locked --features cli``.
2. From kokoro-kindle-reader/: run ``native-deps\fetch-deps.ps1`` and
   ``native-deps\fetch-ocr-models.ps1`` (or reuse espeak-ng-modified-1.52.0/ here for the
   GPL component), then ``packaging\build-installer.ps1``.
See ARCHITECTURE.md and packaging/README.md in the project source for detail.

The build is not bit-for-bit reproducible (espeak-ng is built with whatever MSVC the runner
has; see THIRD_PARTY_NOTICES.md), but the source and configuration here reproduce a
functionally identical build.
"@
Set-Content -Encoding ascii (Join-Path $stage 'README.txt') $readme

# 4. Zip it.
Write-Host '==> Compressing'
Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $out -Force
Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
$z = Get-Item $out
Write-Host ("==> Corresponding source: {0}  ({1:N1} MB)" -f $z.FullName, ($z.Length / 1MB))
