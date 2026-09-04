# Post-build check: extract the produced -setup.exe and assert the whole licence/notice
# tree is present and non-empty. GPLv3 requires the notices accompany the binaries; a
# staging bug or a dropped copy step produces an installer that LOOKS complete and is not.
# This is the same "fail loudly" contract as the OCR-model digests - the installer build is
# not "done" until this passes.
#
#   packaging\verify-installer-notices.ps1                 # newest packaging\*-setup.exe
#   packaging\verify-installer-notices.ps1 -Setup path.exe
#
# ASCII only (PS 5.1). Uses 7-Zip to unpack the NSIS installer (present on windows-latest).
param([string]$Setup)
$ErrorActionPreference = 'Stop'

$here = $PSScriptRoot

if (-not $Setup) {
    $Setup = (Get-ChildItem (Join-Path $here '*-setup.exe') |
              Sort-Object LastWriteTime -Descending | Select-Object -First 1).FullName
}
if (-not $Setup -or -not (Test-Path $Setup)) { throw 'No -setup.exe found to verify.' }
Write-Host "==> Verifying $Setup"

# Locate 7-Zip.
$sevenZip = (Get-Command '7z' -ErrorAction SilentlyContinue).Source
foreach ($cand in 'C:\Program Files\7-Zip\7z.exe', 'C:\Program Files (x86)\7-Zip\7z.exe') {
    if (-not $sevenZip -and (Test-Path $cand)) { $sevenZip = $cand }
}
if (-not $sevenZip) { throw '7-Zip (7z) not found; needed to unpack the NSIS installer.' }

$dir = Join-Path $env:TEMP ("kkr-verify-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force $dir | Out-Null
& $sevenZip x $Setup "-o$dir" -y *> $null
if ($LASTEXITCODE) { throw "7z failed to extract $Setup" }

# Find a file anywhere in the extracted tree (NSIS/7z layout puts app files under an
# internal folder), returning its FileInfo or $null. Match by relative-path suffix so
# 'licenses\espeak-ng\COPYING.UCD' is unambiguous but path-prefix-agnostic.
#
# A BARE name (no directory in the suffix, e.g. 'LICENSE') must be THE install-root file,
# beside the executables - not any nested namesake. The ORT wheel ships its own
# 'licenses\onnxruntime\...\LICENSE', and other subtrees could too (resources\, espeak-ng-data\),
# so a plain '*\LICENSE' suffix match would let any of them satisfy a check for the top-level
# LICENSE after the real one was dropped. Anchor bare names to the directory kokoro-host.exe
# lands in - the install root - and match the name exactly there.
$all = Get-ChildItem $dir -Recurse -File
$hostExe = $all | Where-Object { $_.Name -ieq 'kokoro-host.exe' } | Select-Object -First 1
if (-not $hostExe) {
    throw 'kokoro-host.exe not found in the extracted installer; cannot anchor the install root.'
}
$installRoot = $hostExe.DirectoryName
function Find-Shipped([string]$suffix) {
    $s = ($suffix -replace '/', '\')
    $bare = ($s -notmatch '\\')
    $all | Where-Object {
        if ($bare) { ($_.DirectoryName -eq $installRoot) -and ($_.Name -ieq $s) }
        else { ($_.FullName -replace '/', '\') -like "*\$s" }
    } | Select-Object -First 1
}

# Extract the exact per-component notice paths from the authoritative non-Cargo inventory.
# This is intentionally a narrow, fail-closed parser for components.toml's documented
# one-line `notice = ["path", ...]` schema. Windows PowerShell 5.1 has no TOML parser; accepting
# only this small shape avoids adding a build dependency while making format drift an error.
function Get-ComponentNoticePaths([string]$manifestPath) {
    $toml = [System.IO.File]::ReadAllText($manifestPath)
    $componentCount = [regex]::Matches($toml, '(?m)^\s*\[\[component\]\]\s*$').Count
    $declarationCount = [regex]::Matches($toml, '(?m)^\s*notice\s*=').Count
    $matches = [regex]::Matches(
        $toml,
        '(?m)^\s*notice\s*=\s*\[(?<items>[^\r\n]*)\]\s*(?:#.*)?$'
    )

    if ($componentCount -eq 0) { throw 'components.toml contains no component blocks.' }
    if ($declarationCount -ne $componentCount) {
        throw ("components.toml must have exactly one notice field per component: " +
               "$componentCount component(s), $declarationCount notice field(s).")
    }
    if ($matches.Count -ne $declarationCount) {
        throw ('Every components.toml notice field must use the one-line ' +
               '`notice = ["path", ...]` schema.')
    }

    $paths = @()
    foreach ($match in $matches) {
        $items = $match.Groups['items'].Value
        $stringPattern = '"[^"\\\r\n]+"'
        $listPattern = '^\s*' + $stringPattern + '(?:\s*,\s*' + $stringPattern + ')*\s*$'
        if ($items -notmatch $listPattern) {
            throw "Malformed components.toml notice list: [$items]"
        }
        $strings = [regex]::Matches($items, '"(?<value>[^"\\\r\n]+)"')

        foreach ($string in $strings) {
            $path = $string.Groups['value'].Value
            $segments = @($path -split '[/\\]')
            if ([System.IO.Path]::IsPathRooted($path) -or $path.EndsWith('/') -or
                $path.EndsWith('\') -or $segments -contains '..' -or
                $segments -contains '.' -or $segments -contains '' -or
                [System.Management.Automation.WildcardPattern]::ContainsWildcardCharacters($path)) {
                throw "Component notice must name an exact install-relative file: $path"
            }
            $paths += $path
        }
    }

    return @($paths | Sort-Object -Unique)
}

$componentNotices = @(Get-ComponentNoticePaths (Join-Path $here 'components.toml'))

# Project-level notices not owned by one non-Cargo component. Unicode-3.0 is the standalone
# text linked by THIRD_PARTY_NOTICES.md for the Rust unicode-ident dependency; the generated
# per-binary reports are checked separately below.
$required = @(
    'LICENSE',
    'THIRD_PARTY_NOTICES.md',
    'licenses\Unicode-3.0.txt'
) + $componentNotices
$required = @($required | Sort-Object -Unique)

$missing = @()
$empty = @()
foreach ($r in $required) {
    $f = Find-Shipped $r
    if (-not $f) { $missing += $r }
    elseif ($f.Length -eq 0) { $empty += $r }
}

$groupErrors = @()
# ORT's own LICENSE + ThirdPartyNotices now come from exact canonical paths in
# components.toml, not a directory marker: a co-location check can be satisfied by an
# unrelated namesake in a sibling subtree after ORT's real notice is dropped.
# The Rust Standard Library is outside Cargo's graph. Presence alone is not enough: prove
# the staged HTML is Rust's generated library-only report and TOOLCHAIN.txt carries the
# immutable commit identifying the source copied into the corresponding-source archive.
$rustCopyright = Find-Shipped 'licenses\rust\COPYRIGHT-library.html'
if (-not $rustCopyright -or $rustCopyright.Length -eq 0) {
    $groupErrors += 'licenses\rust\COPYRIGHT-library.html (missing or empty)'
} else {
    $rustCopyrightText = [System.IO.File]::ReadAllText($rustCopyright.FullName)
    if (-not $rustCopyrightText.Contains('Copyright notices for The Rust Standard Library')) {
        $groupErrors += 'licenses\rust\COPYRIGHT-library.html (not the generated library report)'
    }
}
$rustToolchain = Find-Shipped 'licenses\rust\TOOLCHAIN.txt'
if (-not $rustToolchain -or $rustToolchain.Length -eq 0) {
    $groupErrors += 'licenses\rust\TOOLCHAIN.txt (missing or empty)'
} else {
    $rustToolchainText = [System.IO.File]::ReadAllText($rustToolchain.FullName)
    if ($rustToolchainText -notmatch '(?m)^release:\s+\S+\s*$' -or
        $rustToolchainText -notmatch '(?m)^commit-hash:\s+[0-9a-f]{40}\s*$') {
        $groupErrors += 'licenses\rust\TOOLCHAIN.txt (missing release or immutable commit)'
    }
}
# The five per-binary generated Cargo dependency reports. Each must also carry the exact
# licence/notice files harvested from its resolved crate packages; cargo-about's normalized
# SPDX fallback can contain copyright placeholders, so the appendix is load-bearing.
foreach ($reportName in 'kokoro-host.html', 'kokoro-panel.html', 'kokoro-sapi.html',
                         'kokoro-hook.html', 'kokoro-inject.html') {
    $relative = "licenses\dependencies\$reportName"
    $report = Find-Shipped $relative
    if (-not $report -or $report.Length -eq 0) {
        $groupErrors += "$relative (missing or empty)"
        continue
    }
    $reportText = [System.IO.File]::ReadAllText($report.FullName)
    if (-not $reportText.Contains('data-license-appendix="1"') -or
        -not $reportText.Contains('data-source-sha256=')) {
        $groupErrors += "$relative (missing exact packaged licence-file appendix)"
    }
}

Remove-Item -Recurse -Force $dir -ErrorAction SilentlyContinue

if ($missing.Count -or $empty.Count -or $groupErrors.Count) {
    if ($missing.Count) { Write-Host "MISSING:`n  $($missing -join "`n  ")" }
    if ($empty.Count)   { Write-Host "EMPTY:`n  $($empty -join "`n  ")" }
    if ($groupErrors.Count) { Write-Host "GROUPS:`n  $($groupErrors -join "`n  ")" }
    throw 'Installer notice-tree verification FAILED (see above).'
}

Write-Host "==> OK: all required licence/notice files present and non-empty in the installer."
