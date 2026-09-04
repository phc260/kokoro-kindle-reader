# Verify that the SHA-256s recorded in packaging/components.toml for the in-repo shipped
# assets still match the actual files. cargo-about covers the Cargo closure; the extraction
# test proves the notice tree ships; this closes the remaining gap components.toml claimed
# but nothing enforced: a Material Symbol SVG (compiled into kokoro-panel.exe) changing
# without its recorded digest being updated, leaving the "authoritative inventory" silently
# stale while the build stays green.
#
# Only assets that are BOTH checked into the repo AND pinned by sha256 in components.toml are
# checkable here - that is the five kokoro-panel/ui/*.svg. The ORT DLLs / OCR models are
# provisioned or downloaded (pinned by the wheel / ocr-manifest.json, not by us), and icon.ico
# is an LFS asset with no digest in the manifest.
#
#   packaging\verify-component-hashes.ps1
#
# ASCII only (PS 5.1). Throws on any mismatch or missing file.
$ErrorActionPreference = 'Stop'

$here = $PSScriptRoot
$root = Split-Path $here -Parent
$toml = Get-Content (Join-Path $here 'components.toml') -Raw

# Split into [[component]] blocks and pull name + sha256 from each. A full TOML parser is not
# available on PS 5.1; these two keys are simple quoted scalars, so a per-block regex is exact.
$blocks = $toml -split '(?m)^\[\[component\]\]'
$checked = 0
$errors = @()
$manifestSvgs = @()   # the SVG basenames components.toml claims to inventory
foreach ($b in $blocks) {
    if ($b -notmatch '(?m)^\s*name\s*=\s*"([^"]+)"') { continue }
    $name = $Matches[1]
    # Only the SVG assets map to an in-repo file with a pinned digest.
    if ($name -notmatch '^([A-Za-z0-9._-]+\.svg)\b') { continue }
    $svg = $Matches[1]
    $manifestSvgs += $svg
    if ($b -notmatch '(?m)^\s*sha256\s*=\s*"([0-9a-fA-F]{64})"') {
        $errors += "component '$name' ($svg) has no sha256 in components.toml"
        continue
    }
    $expected = $Matches[1].ToLower()
    $path = Join-Path $root (Join-Path 'kokoro-panel\ui' $svg)
    if (-not (Test-Path $path)) {
        $errors += "component '$name' references missing file kokoro-panel\ui\$svg"
        continue
    }
    $actual = (Get-FileHash $path -Algorithm SHA256).Hash.ToLower()
    if ($actual -ne $expected) {
        $errors += ("kokoro-panel\ui\$svg sha256 mismatch: components.toml has $expected, " +
                    "file is $actual - update components.toml (and re-check the notice) if the " +
                    "change is intended.")
    } else {
        $checked++
    }
}

# COMPLETENESS: a per-block hash check only sees the blocks that still exist, so deleting a
# component block (or embedding a new SVG without one) would pass silently. The authoritative
# set of SVGs that actually ship is whatever panel.slint compiles in via @image-url(...). The
# manifest's SVG set must equal it exactly - no missing block, no orphan entry.
$slint = Get-Content (Join-Path $root 'kokoro-panel\ui\panel.slint') -Raw
$referenced = [regex]::Matches($slint, '@image-url\(\s*"([^"]+\.svg)"') |
              ForEach-Object { Split-Path $_.Groups[1].Value -Leaf } | Sort-Object -Unique
$inManifest = $manifestSvgs | Sort-Object -Unique
$missingBlock = $referenced | Where-Object { $inManifest -notcontains $_ }
$orphanBlock  = $inManifest | Where-Object { $referenced -notcontains $_ }
foreach ($m in $missingBlock) {
    $errors += "panel.slint compiles in '$m' but components.toml has no component block for it"
}
foreach ($o in $orphanBlock) {
    $errors += "components.toml inventories '$o' but panel.slint no longer references it (stale block)"
}

if ($checked -eq 0 -and $errors.Count -eq 0) {
    throw 'No SVG components found in components.toml - the parser or the manifest changed shape.'
}
if ($errors.Count) {
    Write-Host ("MISMATCH/MISSING:`n  " + ($errors -join "`n  "))
    throw 'components.toml component-hash verification FAILED (see above).'
}
Write-Host ("==> OK: $checked component SHA-256(s) match, and the manifest's SVG set equals " +
            "panel.slint's ($($referenced.Count) glyphs).")
