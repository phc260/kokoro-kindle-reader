# Generate the Cargo dependency-licence notices for every crate the installer actually
# ships, from THIS checkout's locked dependency graph - not a hand-maintained prose list.
#
# THIRD_PARTY_NOTICES.md used to assert license coverage for a handful of unusual crates
# by name. That list drifted from the truth (four crates it called "OR alternatives
# already covered by MIT/Apache-2.0" turned out to be sole-licensed ISC/Zlib/
# CDLA-Permissive-2.0), and a lockfile of 700+ transitive crates was never going to stay
# accurate by hand regardless. `cargo about` reads the SAME Cargo.lock the release build
# just used, resolves every crate's licence expression against the ACCEPTED list in
# about.toml, and renders the full text (with that crate's own copyright holder, where
# known) via packaging/about.hbs. THIRD_PARTY_NOTICES.md stays the human-readable
# overview; this is the notice of record for the Cargo closure. The Rust standard library
# comes from the compiler sysroot rather than this graph and is staged separately by
# build-installer.ps1.
#
# `--fail` is the mechanism that satisfies "detect new licence categories when
# dependencies change": a crate whose resolved expression cannot be satisfied from
# about.toml's `accepted` list makes cargo-about exit non-zero, which this script (and
# build-installer.ps1, which calls it) turns into a build failure - the same "fail loudly
# rather than ship incomplete" shape as the ONNX Runtime notices and the OCR model digests.
#
# One HTML file per SHIPPED (crate, target) pair - not one combined report - because the
# x86 artifacts (kokoro-sapi, kokoro-hook, kokoro-inject) and the x64 ones (kokoro-host,
# kokoro-panel) resolve different dependency graphs for the same lockfile family, and
# `cargo about` operates on one root crate at a time (there is no root workspace here -
# see CLAUDE.md). kokoro-ocr and kokoro-protocol are path dependencies of kokoro-host and
# are covered by ITS graph; kokoro-sapi-smoke is a dev/test tool and is not shipped, so it
# is not included here.
#
# Output is PROVISIONED, not tracked - same reasoning as native-deps/runtime/notices/
# (see fetch-deps.ps1): committing a generated report invites it to go stale the moment
# a lockfile changes without anyone re-running this script. build-installer.ps1 calls it
# on every build so the shipped notices always match what was just compiled.
param([switch]$SkipCheck, [string]$OutputDir)
$ErrorActionPreference = 'Stop'

$here = $PSScriptRoot
$root = Split-Path $here -Parent
$template = Join-Path $here 'about.hbs'
$config = Join-Path $here 'about.toml'
$outDir = if ($OutputDir) { $OutputDir } else { Join-Path $here 'dependency-licenses' }
$utf8NoBom = New-Object System.Text.UTF8Encoding($false)

# cargo-about is the licence-expression gate, but its normalized SPDX fallback text can
# contain placeholders such as "Copyright (c) <year> <owner>" even when a crate packages
# the real notice in LICENSE.md. Append those packaged files verbatim so binary
# redistribution conditions retain the actual copyright holders. The SHA-256 on every
# heading makes the appendix self-auditing and lets the installer extraction check prove
# these are harvested source files, not another hand-written summary.
function Add-PackagedLicenseAppendix([string]$manifest, [string]$triple, [string]$report) {
    $metadataText = (& cargo metadata --locked --format-version 1 --filter-platform $triple `
                     --manifest-path $manifest) | Out-String
    if ($LASTEXITCODE) { throw "cargo metadata failed for $manifest ($triple)" }
    $metadata = $metadataText | ConvertFrom-Json

    $resolved = @{}
    foreach ($node in $metadata.resolve.nodes) { $resolved[$node.id] = $true }
    $packages = @($metadata.packages | Where-Object {
        $resolved.ContainsKey($_.id)
    } | Sort-Object name, version)

    $body = New-Object System.Text.StringBuilder
    [void]$body.AppendLine('<section id="packaged-license-files" data-license-appendix="1">')
    [void]$body.AppendLine('<h2>Exact packaged licence and notice files</h2>')
    [void]$body.AppendLine('<p class="intro">Copied verbatim from the dependency packages resolved by Cargo.lock.</p>')
    $fileCount = 0
    $packageCount = 0

    foreach ($package in $packages) {
        $packageDir = Split-Path ([string]$package.manifest_path) -Parent
        $files = @()
        $files += Get-ChildItem -LiteralPath $packageDir -File | Where-Object {
            $_.Name -match '(?i)^(LICENSES?|LICENCES?|COPYINGS?|NOTICES?|COPYRIGHTS?|AUTHORS?|CONTRIBUTORS?)(?:$|[._-].*)'
        }
        foreach ($subdirName in 'licenses', 'licences', 'notices') {
            $subdir = Join-Path $packageDir $subdirName
            if (Test-Path -LiteralPath $subdir -PathType Container) {
                $files += Get-ChildItem -LiteralPath $subdir -Recurse -File
            }
        }
        $files = @($files | Sort-Object FullName -Unique)
        if ($files.Count -eq 0) { continue }

        $packageCount++
        $packageLabel = [System.Net.WebUtility]::HtmlEncode("$($package.name) $($package.version)")
        [void]$body.AppendLine('<article class="packaged-license">')
        [void]$body.AppendLine("<h3>$packageLabel</h3>")
        if ($package.authors.Count -gt 0) {
            $authors = [System.Net.WebUtility]::HtmlEncode(($package.authors -join '; '))
            [void]$body.AppendLine(('<p class="package-authors">Package authors: {0}</p>' -f $authors))
        }

        foreach ($file in $files) {
            $relative = $file.FullName.Substring($packageDir.Length).TrimStart('\', '/')
            $label = [System.Net.WebUtility]::HtmlEncode($relative)
            $hash = (Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash.ToLower()
            $text = [System.IO.File]::ReadAllText($file.FullName)
            $encoded = [System.Net.WebUtility]::HtmlEncode($text)
            [void]$body.AppendLine(('<h4 data-source-sha256="{0}">{1}</h4>' -f $hash, $label))
            [void]$body.AppendLine("<pre>$encoded</pre>")
            $fileCount++
        }
        [void]$body.AppendLine('</article>')
    }
    [void]$body.AppendLine('</section>')

    if ($packageCount -eq 0 -or $fileCount -eq 0) {
        throw "No packaged dependency licence files found for $manifest ($triple)"
    }
    $html = [System.IO.File]::ReadAllText($report)
    if (-not $html.Contains('</body>')) { throw "cargo-about report has no </body>: $report" }
    $html = $html.Replace('</body>', $body.ToString() + '</body>')
    [System.IO.File]::WriteAllText($report, $html, $utf8NoBom)

    $written = [System.IO.File]::ReadAllText($report)
    if (-not $written.Contains('data-license-appendix="1"') -or
        -not $written.Contains('data-source-sha256=')) {
        throw "Packaged dependency licence appendix was not written to $report"
    }
    return @{ Packages = $packageCount; Files = $fileCount }
}

if (-not $SkipCheck) {
    cargo about --version *> $null
    if ($LASTEXITCODE) {
        throw ('cargo-about is not installed. Run: ' +
               'cargo install cargo-about --locked --features cli')
    }
}

# (crate directory name, target triple it's actually built for - see packaging/README.md
# and CLAUDE.md's bitness invariants).
$targets = @(
    @{ Crate = 'kokoro-host';   Triple = 'x86_64-pc-windows-msvc' }
    @{ Crate = 'kokoro-panel';  Triple = 'x86_64-pc-windows-msvc' }
    @{ Crate = 'kokoro-sapi';   Triple = 'i686-pc-windows-msvc' }
    @{ Crate = 'kokoro-hook';   Triple = 'i686-pc-windows-msvc' }
    @{ Crate = 'kokoro-inject'; Triple = 'i686-pc-windows-msvc' }
)

Remove-Item -Recurse -Force $outDir -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $outDir | Out-Null

foreach ($t in $targets) {
    $manifest = Join-Path $root "$($t.Crate)\Cargo.toml"
    $out = Join-Path $outDir "$($t.Crate).html"
    Write-Host "==> cargo about generate: $($t.Crate) ($($t.Triple))"
    & cargo about generate -c $config -m $manifest --target $t.Triple --fail $template -o $out
    if ($LASTEXITCODE) {
        throw ("cargo about generate failed for $($t.Crate) ($($t.Triple)). If this is a " +
               'newly-added dependency under a licence not yet in about.toml''s `accepted` ' +
               "list, that is this script doing its job - add the licence (and, if it's " +
               'mandatory rather than an OR alternative already covered, make sure its ' +
               'notice actually ships) rather than widening `accepted` to make the error ' +
               'go away.')
    }
    $added = Add-PackagedLicenseAppendix $manifest $t.Triple $out
    Write-Host ("    appended exact packaged notices: {0} file(s) from {1} package(s)" -f `
                $added.Files, $added.Packages)
}

Write-Host "==> Dependency licence notices generated in $outDir"
Get-ChildItem $outDir -Filter '*.html' | ForEach-Object {
    Write-Host ("    {0}  ({1:N0} KB)" -f $_.Name, ($_.Length / 1KB))
}
