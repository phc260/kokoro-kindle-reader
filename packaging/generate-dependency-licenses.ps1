# Generate the Rust dependency-licence notices for every crate the installer actually
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
# overview; this is the notice of record for the Rust closure.
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
param([switch]$SkipCheck)
$ErrorActionPreference = 'Stop'

$here = $PSScriptRoot
$root = Split-Path $here -Parent
$template = Join-Path $here 'about.hbs'
$config = Join-Path $here 'about.toml'
$outDir = Join-Path $here 'dependency-licenses'

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
}

Write-Host "==> Dependency licence notices generated in $outDir"
Get-ChildItem $outDir -Filter '*.html' | ForEach-Object {
    Write-Host ("    {0}  ({1:N0} KB)" -f $_.Name, ($_.Length / 1KB))
}
