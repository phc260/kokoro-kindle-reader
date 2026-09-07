# Small offline regression fixtures; no application, Rust build, or native tools.
$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot
$utf8 = New-Object System.Text.UTF8Encoding($false)
$testDir = Join-Path ([System.IO.Path]::GetTempPath()) ('kkr-license-test-' + [guid]::NewGuid().ToString('N'))
$testDir = [System.IO.Path]::GetFullPath($testDir)
New-Item -ItemType Directory -Path $testDir | Out-Null
$report = Join-Path $testDir 'report.html'
$config = Join-Path $testDir 'about.toml'
$verifier = Join-Path $here 'verify-dependency-licenses.ps1'
$passed = 0

function Assert-Rejected([string]$name, [scriptblock]$action) {
    $rejected = $false
    try { & $action } catch { $rejected = $true }
    if (-not $rejected) { throw "Unexpected success: $name" }
    Write-Host "PASS: $name"
    $script:passed++
}
function Set-Report([string]$html) {
    [System.IO.File]::WriteAllText($report, $html, $utf8)
}
try {
    # Invented notice with non-ASCII and HTML syntax: hashes must cover decoded UTF-8.
    $notice = 'Copyright ' + [char]0x00A9 + " 2026 Fixture Authors <one & two>`nPermission notice.`n"
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try {
        $hash = [System.BitConverter]::ToString($sha.ComputeHash($utf8.GetBytes($notice))).Replace('-', '').ToLowerInvariant()
    } finally { $sha.Dispose() }
    $toml = "[fixture.clarify]`nlicense = `"MIT`"`n[[fixture.clarify.git]]`npath = `"LICENSE`"`nchecksum = `"$hash`"`n"
    [System.IO.File]::WriteAllText($config, $toml, $utf8)
    $inventory = '<span data-report-crate="fixture" data-report-version="1.0.0"></span>'
    $section = '<section class="resolved-license"><span data-license-crate="fixture" data-license-version="1.0.0"></span><pre>' +
        [System.Net.WebUtility]::HtmlEncode($notice) + '</pre></section>'
    $packaged = '<h4 data-source-sha256="' + $hash + '" data-text-sha256="' + $hash +
        '">LICENSE</h4><pre>' + [System.Net.WebUtility]::HtmlEncode($notice) + '</pre>' +
        '<span hidden data-packaged-notice-count="1"></span>'
    $valid = $inventory + $section + $packaged
    Set-Report $valid
    & $verifier -Report $report -Config $config
    Write-Host 'PASS: exact UTF-8 notice and HTML round trip'
    $passed++

    Set-Report ($inventory + $section.Replace([System.Net.WebUtility]::HtmlEncode($notice), 'Copyright &lt;year&gt; &lt;owner&gt;') + $packaged)
    Assert-Rejected 'canonical fallback after failed clarification' { & $verifier -Report $report -Config $config }
    Set-Report ($inventory + $section.Replace('Permission notice.', 'Permission changed.') + $packaged)
    Assert-Rejected 'changed notice text' { & $verifier -Report $report -Config $config }
    Set-Report ($inventory + $section.Replace('data-license-crate="fixture"', 'data-license-crate="unrelated"') + $packaged)
    Assert-Rejected 'another crate cannot supply the missing notice' { & $verifier -Report $report -Config $config }
    Set-Report ($valid + $inventory.Replace('1.0.0', '2.0.0'))
    Assert-Rejected 'one version cannot bless another version' { & $verifier -Report $report -Config $config }
    Set-Report ($inventory + $packaged)
    Assert-Rejected 'missing licence sections' { & $verifier -Report $report -Config $config }
    Set-Report ($section + $packaged)
    Assert-Rejected 'missing graph inventory' { & $verifier -Report $report -Config $config }
    Set-Report $valid
    [System.IO.File]::WriteAllText($config, $toml.Replace($hash, 'broken'), $utf8)
    Assert-Rejected 'malformed configured checksum' { & $verifier -Report $report -Config $config }

    # Load only the production header-extraction function, never the generator body.
    $tokens = $null
    $errors = $null
    $ast = [System.Management.Automation.Language.Parser]::ParseFile(
        (Join-Path $here 'generate-dependency-licenses.ps1'), [ref]$tokens, [ref]$errors)
    if ($errors.Count) { throw 'Generator does not parse.' }
    $function = $ast.Find({ param($node)
        $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
        $node.Name -eq 'Get-LeadingCopyrightNotice'
    }, $true)
    if (-not $function) { throw 'Production header extractor not found.' }
    . ([scriptblock]::Create($function.Extent.Text))
    $lineHeader = "// Copyright 2026 Fixture Authors`n// License terms and a second holder.`n"
    $blockHeader = "/*`n * Copyright 2026 Fixture Authors`n * Full permission and disclaimer.`n */"
    foreach ($header in $lineHeader, $blockHeader) {
        $actual = Get-LeadingCopyrightNotice ($header + "`nfn example() {}`n")
        if ($actual -cne $header.TrimEnd()) { throw 'Leading notice was lost or changed.' }
        $passed++
    }
    foreach ($source in 'const TEXT: &str = "Copyright 2026 Fiction";',
                         "//! Copyright in a documentation example`nfn example() {}`n",
                         "// Ordinary comment`nfn example() {}`n// Copyright later in code") {
        if (Get-LeadingCopyrightNotice $source) { throw 'Non-header text was harvested.' }
        $passed++
    }

    # Exercise the complete appendix with invented embedded terms: Rust comments
    # after attributes/docs and a native .inl file, neither seen by the broad scan.
    . (Join-Path $here 'source-notices.ps1')
    $appendFunction = $ast.Find({ param($node)
        $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
        $node.Name -eq 'Add-PackagedLicenseAppendix'
    }, $true)
    if (-not $appendFunction) { throw 'Production appendix generator not found.' }
    . ([scriptblock]::Create($appendFunction.Extent.Text))
    $utf8NoBom = $utf8
    $packageDir = Join-Path $testDir 'embedded'
    New-Item -ItemType Directory -Path (Join-Path $packageDir 'src'), (Join-Path $packageDir 'crypto') | Out-Null
    $embeddedText = "// Copyright 2026 Fixture Authors <one & two>`n// Permission to use this invented fixture.`n// No warranty."
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try {
        $embeddedHash = [System.BitConverter]::ToString($sha.ComputeHash($utf8.GetBytes($embeddedText))).Replace('-', '').ToLowerInvariant()
    } finally { $sha.Dispose() }
    $rustPrefix = "#![allow(dead_code)]`n//! Documentation before the notice.`n`n"
    $rustPath = Join-Path $packageDir 'src/lib.rs'
    [System.IO.File]::WriteAllText($rustPath, ($rustPrefix + $embeddedText + "`nfn example() {}`n").Replace("`n", "`r`n"), $utf8)
    [System.IO.File]::WriteAllText((Join-Path $packageDir 'crypto/notice.inl'), $embeddedText + "`nint example;`n", $utf8)
    # A BOM-bearing licence and an ordinary source header have no special
    # clarification requirement: the appendix's own integrity check must protect them.
    $ordinaryLicense = "Copyright 2026 Ordinary Fixture Authors`r`nInvented permission and warranty terms.`r`n"
    [System.IO.File]::WriteAllText((Join-Path $packageDir 'LICENSE'), $ordinaryLicense,
        (New-Object System.Text.UTF8Encoding($true)))
    [System.IO.File]::WriteAllText((Join-Path $packageDir 'src/ordinary.rs'),
        "// Copyright 2026 Header Fixture Authors`n// Invented notice.`nfn ordinary() {}`n", $utf8)
    $sourceConfig = Join-Path $testDir 'source-notices.json'
    $entries = @(
        @{ crate = 'embedded'; version = '1.0.0'; path = 'src/lib.rs'; first_line = 4; last_line = 6; license = 'ISC'; sha256 = $embeddedHash }
        @{ crate = 'embedded'; version = '1.0.0'; path = 'crypto/notice.inl'; first_line = 1; last_line = 3; license = 'ISC'; sha256 = $embeddedHash }
    )
    [System.IO.File]::WriteAllText($sourceConfig, (ConvertTo-Json -InputObject $entries), $utf8)
    $sourceNotices = @(Read-SourceNoticeRequirements $sourceConfig)
    $fixturePackage = @{
        id = 'embedded-id'; name = 'embedded'; version = '1.0.0'
        manifest_path = (Join-Path $packageDir 'Cargo.toml'); source = 'registry+fixture'; authors = @()
    }
    $fixtureMetadata = @{
        packages = @($fixturePackage); resolve = @{ nodes = @(@{ id = 'embedded-id' }) }
    } | ConvertTo-Json -Depth 6
    # Shadow the metadata command only inside this test script. No Cargo/build runs.
    function cargo {
        param([Parameter(ValueFromRemainingArguments = $true)][object[]]$CommandArgs)
        $global:LASTEXITCODE = 0
        $fixtureMetadata
    }
    $embeddedInventory = '<span data-report-crate="embedded" data-report-version="1.0.0"></span>'
    [System.IO.File]::WriteAllText($config, $toml, $utf8)
    Set-Report ('<body>' + $inventory + $section + $embeddedInventory + '</body>')
    $added = Add-PackagedLicenseAppendix 'fixture' 'fixture-target' $report
    if ($added.Files -ne 4) { throw 'Both ordinary and embedded notices must be appended.' }
    & $verifier -Report $report -Config $config -SourceNotices $sourceConfig
    $embeddedValid = [System.IO.File]::ReadAllText($report)
    $passed++
    Write-Host 'PASS: generator retains BOM/CRLF licence, ordinary header and embedded Rust/native notices'

    $ordinaryBlocks = [regex]::Matches($embeddedValid,
        '(?s)<h4 data-source-sha256="[0-9a-f]{64}" data-text-sha256="[0-9a-f]{64}">(?<label>.*?)</h4>\s*<pre>(?<text>.*?)</pre>')
    $licenseBlock = @($ordinaryBlocks | Where-Object { $_.Groups['label'].Value -eq 'LICENSE' })[0]
    $headerBlock = @($ordinaryBlocks | Where-Object { $_.Groups['label'].Value -like '*leading comment excerpt*' })[0]
    if (-not $licenseBlock -or -not $headerBlock) { throw 'Ordinary licence/header fixtures were not generated.' }
    Set-Report ($embeddedValid.Replace($licenseBlock.Value,
        $licenseBlock.Value.Replace($licenseBlock.Groups['text'].Value, 'NOTICE OMITTED')))
    Assert-Rejected 'ordinary packaged licence changed while all special notices survive' {
        & $verifier -Report $report -Config $config -SourceNotices $sourceConfig
    }
    Set-Report ($embeddedValid.Replace($headerBlock.Value,
        $headerBlock.Value.Replace($headerBlock.Groups['text'].Value, 'COPYRIGHT OMITTED')))
    Assert-Rejected 'ordinary source copyright changed' {
        & $verifier -Report $report -Config $config -SourceNotices $sourceConfig
    }
    Set-Report ($embeddedValid.Replace($licenseBlock.Value, ''))
    Assert-Rejected 'whole ordinary notice block removed' {
        & $verifier -Report $report -Config $config -SourceNotices $sourceConfig
    }
    Set-Report ($embeddedValid.Replace($licenseBlock.Value,
        $licenseBlock.Value.Replace('data-text-sha256=', 'data-ignored-sha256=')))
    Assert-Rejected 'ordinary notice has no checkable text hash' {
        & $verifier -Report $report -Config $config -SourceNotices $sourceConfig
    }
    Set-Report ($embeddedValid + '<span data-packaged-notice-count="4"></span>')
    Assert-Rejected 'duplicate appendix inventory' {
        & $verifier -Report $report -Config $config -SourceNotices $sourceConfig
    }
    Set-Report ($embeddedValid.Replace('data-packaged-notice-count=', 'data-ignored-notice-count='))
    Assert-Rejected 'missing appendix inventory' {
        & $verifier -Report $report -Config $config -SourceNotices $sourceConfig
    }


    Set-Report ([regex]::Replace($embeddedValid, '(?s)<section class="source-license">.*?</section>', ''))
    Assert-Rejected 'missing embedded notice despite valid package licence' {
        & $verifier -Report $report -Config $config -SourceNotices $sourceConfig
    }
    Set-Report ($embeddedValid.Replace('No warranty.', 'Truncated notice.'))
    Assert-Rejected 'changed embedded notice in extracted report' {
        & $verifier -Report $report -Config $config -SourceNotices $sourceConfig
    }
    Set-Report ($embeddedValid.Replace('data-license-crate="embedded"', 'data-license-crate="unrelated"'))
    Assert-Rejected 'embedded notice attributed to another crate' {
        & $verifier -Report $report -Config $config -SourceNotices $sourceConfig
    }
    Set-Report ($embeddedValid.Replace('1.0.0', '2.0.0'))
    Assert-Rejected 'dependency upgrade requires source-notice review' {
        & $verifier -Report $report -Config $config -SourceNotices $sourceConfig
    }
    [System.IO.File]::WriteAllText($rustPath, $rustPrefix + $embeddedText.Replace('No warranty.', 'Changed terms.'), $utf8)
    Assert-Rejected 'generator refuses source excerpt drift' {
        Add-PackagedLicenseAppendix 'fixture' 'fixture-target' $report
    }
    $fixtureMetadata = $fixtureMetadata.Replace('1.0.0', '2.0.0')
    Assert-Rejected 'generator refuses unreviewed dependency version' {
        Add-PackagedLicenseAppendix 'fixture' 'fixture-target' $report
    }
    Write-Host "==> PASS: $passed offline dependency-notice checks"
} finally {
    # Delete only this test's freshly-created, fully resolved directory under TEMP.
    $expectedParent = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath()).TrimEnd('\', '/')
    if ((Split-Path $testDir -Parent) -ne $expectedParent -or
        (Split-Path $testDir -Leaf) -notmatch '^kkr-license-test-[0-9a-f]{32}$') {
        throw "Refusing cleanup outside the test directory: $testDir"
    }
    Remove-Item -LiteralPath $testDir -Recurse -Force
}
