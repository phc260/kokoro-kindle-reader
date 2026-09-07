# Verify the checked-in licence texts byte-for-byte after normalizing line endings.
# Presence/non-empty checks are not enough: a truncated text, a notice copied from the wrong
# upstream revision, or a missing copyright line still produces an installer that looks
# complete. license-texts.sha256 pins the reviewed text while remaining stable across Git's
# CRLF conversion on Windows.
#
# With no arguments, also require the manifest to inventory every checked-in file under
# licenses/, plus root LICENSE, THIRD_PARTY_NOTICES.md and legal.html. -AllowAdditional is
# used against an extracted installer, whose licenses/ directory also contains provisioned
# and generated notice trees.
#
# ASCII only (PS 5.1 - see CLAUDE.md).
param([string]$Root, [switch]$AllowAdditional)
$ErrorActionPreference = 'Stop'

$here = $PSScriptRoot
$repoRoot = Split-Path $here -Parent
if (-not $Root) { $Root = $repoRoot }
$Root = [System.IO.Path]::GetFullPath($Root)
$rootPrefix = $Root.TrimEnd('\', '/') + [System.IO.Path]::DirectorySeparatorChar
$manifest = Join-Path $here 'license-texts.sha256'
$utf8NoBom = New-Object System.Text.UTF8Encoding($false, $true)
$sha = [System.Security.Cryptography.SHA256]::Create()
$entries = @{}
$errors = @()

foreach ($line in [System.IO.File]::ReadAllLines($manifest)) {
    $trimmed = $line.Trim()
    if (-not $trimmed -or $trimmed.StartsWith('#')) { continue }
    if ($trimmed -notmatch '^([0-9a-f]{64})  ([A-Za-z0-9][A-Za-z0-9._/-]*)$') {
        $errors += "Malformed checksum line: $line"
        continue
    }
    $expected = $Matches[1]
    $relative = $Matches[2]
    $segments = @($relative -split '/')
    if ([System.IO.Path]::IsPathRooted($relative) -or $segments -contains '.' -or
        $segments -contains '..' -or $segments -contains '') {
        $errors += "Unsafe checksum path: $relative"
        continue
    }
    if ($entries.ContainsKey($relative)) {
        $errors += "Duplicate checksum path: $relative"
        continue
    }
    $entries[$relative] = $expected

    $path = [System.IO.Path]::GetFullPath((Join-Path $Root ($relative -replace '/', '\')))
    if (-not $path.StartsWith($rootPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
        $errors += "Checksum path escapes the root: $relative"
        continue
    }
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        $errors += "Missing checked-in licence text: $relative"
        continue
    }
    $bytes = [System.IO.File]::ReadAllBytes($path)
    if ($bytes.Length -eq 0) {
        $errors += "Empty checked-in licence text: $relative"
        continue
    }
    if ($bytes.Length -ge 3 -and $bytes[0] -eq 0xef -and $bytes[1] -eq 0xbb -and
        $bytes[2] -eq 0xbf) {
        $errors += "UTF-8 BOM is not allowed in checked-in licence text: $relative"
        continue
    }
    try {
        $text = $utf8NoBom.GetString($bytes)
    } catch {
        $errors += "Checked-in licence text is not valid UTF-8: $relative"
        continue
    }
    $normalized = $text.Replace("`r`n", "`n").Replace("`r", "`n")
    $actual = [System.BitConverter]::ToString(
        $sha.ComputeHash($utf8NoBom.GetBytes($normalized))
    ).Replace('-', '').ToLower()
    if ($actual -ne $expected) {
        $errors += ("$relative checksum mismatch: expected $expected, got $actual - " +
                    'verify the complete replacement against its upstream revision before ' +
                    'updating packaging/license-texts.sha256.')
    }
}

if ($entries.Count -eq 0) { $errors += 'license-texts.sha256 contains no entries.' }

if (-not $AllowAdditional) {
    $actualPaths = @('LICENSE', 'THIRD_PARTY_NOTICES.md', 'legal.html')
    $licenseDir = Join-Path $Root 'licenses'
    if (Test-Path -LiteralPath $licenseDir -PathType Container) {
        $actualPaths += Get-ChildItem -LiteralPath $licenseDir -Recurse -File | ForEach-Object {
            $_.FullName.Substring($rootPrefix.Length).Replace('\', '/')
        }
    }
    foreach ($relative in @($actualPaths | Sort-Object -Unique)) {
        if (-not $entries.ContainsKey($relative)) {
            $errors += "Checked-in licence text is not pinned in license-texts.sha256: $relative"
        }
    }
    foreach ($relative in $entries.Keys) {
        if ($actualPaths -notcontains $relative) {
            $errors += "license-texts.sha256 has no matching checked-in file: $relative"
        }
    }
}

$sha.Dispose()
if ($errors.Count) {
    Write-Host ("LICENCE TEXT ERROR(S):`n  " + ($errors -join "`n  "))
    throw 'Checked-in licence-text verification FAILED (see above).'
}
Write-Host "==> OK: $($entries.Count) checked-in licence text(s) match reviewed SHA-256s."
