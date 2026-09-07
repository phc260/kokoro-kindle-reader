# Verify the actual rendered clarification texts, not cargo-about's exit status:
# 0.9.1 warns and falls back to canonical text if a pinned upstream fetch/hash fails.
# Also validate every ordinary appendix text and its expected block count, not only
# the special clarified/embedded notices. Used on extracted reports too; no builds/network.
param(
    [Parameter(Mandatory = $true)][string]$Report,
    [string]$Config = (Join-Path $PSScriptRoot 'about.toml'),
    [string]$SourceNotices = (Join-Path $PSScriptRoot 'source-notices.json')
)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'source-notices.ps1')
$embeddedRequirements = @(Read-SourceNoticeRequirements $SourceNotices)

# Deliberately limited to our configuration's unquoted crate keys and one-line
# checksum fields. Reject schema drift instead of silently skipping a requirement.
$toml = [System.IO.File]::ReadAllText($Config)
$declarations = [regex]::Matches($toml, '(?m)^\s*\[\[[^\r\n]*\.clarify\.(?:git|files)\]\]\s*$')
$blocks = [regex]::Matches($toml,
    '(?ms)^\[\[(?<crate>[A-Za-z0-9_-]+)\.clarify\.(?:git|files)\]\]\r?\n(?<fields>.*?)(?=^\[|\z)')
if ($declarations.Count -eq 0 -or $declarations.Count -ne $blocks.Count) {
    throw 'about.toml clarification files must use the checked one-line table schema.'
}
$required = @{}
foreach ($block in $blocks) {
    $crate = $block.Groups['crate'].Value
    $checksums = [regex]::Matches($block.Groups['fields'].Value,
        '(?m)^checksum = "(?<hash>[0-9a-f]{64})"\s*$')
    if ($checksums.Count -ne 1) { throw "Expected one SHA-256 in $crate clarification." }
    $required[$crate] = @($required[$crate]) + $checksums[0].Groups['hash'].Value
}

$html = [System.IO.File]::ReadAllText($Report)
# The old presence-only appendix check allowed a normal LICENSE/header to be
# replaced or removed while the special clarifications still passed. Keep the raw
# source hash for provenance, but check the hash of the actual decoded UTF-8 text:
# ReadAllText removes a UTF-8 BOM from a source file, so its file hash can differ.
$countMarkers = [regex]::Matches($html, 'data-packaged-notice-count="(?<count>[0-9]+)"')
$expectedCount = 0
if ($countMarkers.Count -ne 1 -or
    -not [int]::TryParse($countMarkers[0].Groups['count'].Value, [ref]$expectedCount) -or
    $expectedCount -lt 1) {
    throw "Missing or malformed packaged-notice inventory in $Report"
}
$appendixTexts = [regex]::Matches($html,
    '(?s)<h4 data-source-sha256="[0-9a-f]{64}" data-text-sha256="(?<hash>[0-9a-f]{64})">(?<label>.*?)</h4>\s*<pre>(?<text>.*?)</pre>')
$headingCount = [regex]::Matches($html, '<h4 data-source-sha256=').Count
if ($appendixTexts.Count -ne $expectedCount -or $headingCount -ne $expectedCount) {
    throw "Missing or malformed packaged notice blocks in $Report (expected $expectedCount)."
}
foreach ($notice in $appendixTexts) {
    $text = [System.Net.WebUtility]::HtmlDecode($notice.Groups['text'].Value)
    if ((Get-NoticeTextSha256 $text) -cne $notice.Groups['hash'].Value) {
        $label = [System.Net.WebUtility]::HtmlDecode($notice.Groups['label'].Value)
        throw "Packaged notice text changed in $Report ($label)."
    }
}
$inventory = [regex]::Matches($html,
    'data-report-crate="(?<crate>[A-Za-z0-9_-]+)" data-report-version="(?<version>[^"]+)"')
if ($inventory.Count -eq 0) { throw "No dependency inventory in $Report" }
$sections = [regex]::Matches($html,
    '(?s)<section class="(?:resolved-license|source-license)">(?<body>.*?)</section>')
if ($sections.Count -eq 0) { throw "No machine-checkable licence sections in $Report" }
$rendered = @{}
$sha = [System.Security.Cryptography.SHA256]::Create()
try {
    foreach ($section in $sections) {
        $body = $section.Groups['body'].Value
        $texts = [regex]::Matches($body, '(?s)<pre>(?<text>.*?)</pre>')
        $crates = [regex]::Matches($body,
            'data-license-crate="(?<crate>[A-Za-z0-9_-]+)" data-license-version="(?<version>[^"]+)"')
        if ($texts.Count -ne 1 -or $crates.Count -eq 0) {
            throw "Malformed licence section in $Report"
        }
        $text = [System.Net.WebUtility]::HtmlDecode($texts[0].Groups['text'].Value)
        $hash = [System.BitConverter]::ToString(
            $sha.ComputeHash([System.Text.Encoding]::UTF8.GetBytes($text))
        ).Replace('-', '').ToLowerInvariant()
        foreach ($crateMatch in $crates) {
            $crate = $crateMatch.Groups['crate'].Value
            $version = $crateMatch.Groups['version'].Value
            # Separate versions: one version's correct notice cannot bless another's
            # failed clarification merely because their crate names are the same.
            $key = "$crate $version"
            if (-not $rendered.ContainsKey($key)) {
                $rendered[$key] = @{ Crate = $crate; Hashes = @{} }
            }
            $rendered[$key].Hashes[$hash] = $true
        }
    }
} finally {
    $sha.Dispose()
}

$checked = 0
foreach ($entry in $inventory) {
    $crate = $entry.Groups['crate'].Value
    $version = $entry.Groups['version'].Value
    $key = "$crate $version"
    $embedded = @(Get-SourceNoticeRequirements $embeddedRequirements $crate $version)
    $hashes = @($required[$crate]) + @($embedded | ForEach-Object { $_.sha256 })
    foreach ($hash in $hashes) {
        if (-not $hash) { continue }
        if (-not $rendered.ContainsKey($key) -or -not $rendered[$key].Hashes.ContainsKey($hash)) {
            throw ("Missing exact clarified or embedded licence text for $key in $Report (SHA-256 $hash). " +
                   'Check upstream retrieval, about.toml and source-notices.json; canonical fallback is not sufficient.')
        }
        $checked++
    }
}
Write-Host "==> OK: $expectedCount packaged notice(s), $checked clarified/embedded text(s) verified in $Report"
