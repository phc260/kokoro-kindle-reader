# Kokoro Kindle Reader - toolchain doctor (Windows). ASCII ONLY: PowerShell 5.1 misreads a
# UTF-8-no-BOM em-dash, so use "-" and "..." here. That also rules out flutter's own tick and
# cross glyphs (U+2713/U+2717), so the markers are [+] [!] [x] [-] - all three columns wide,
# which is what keeps the names lined up.
#
#   .\packaging\doctor.cmd                 # everything
#   .\packaging\doctor.cmd -For app        # just build and run the host/panel
#   .\packaging\doctor.cmd -For installer  # + package it
#   .\packaging\doctor.cmd -For source     # + the source archive released with it
#   .\packaging\doctor.cmd -For extension  # the browser extension's suite
#
# Run it through doctor.cmd, not directly: Windows' default execution policy (Restricted)
# refuses every .ps1, and the shim bypasses it for that one process without changing it.
#
# WHY THIS IS POWERSHELL AND NOT PYTHON, given that twelve .ps1 harnesses were deleted just
# before it was written: a doctor has to run on a machine that has NOTHING installed. A Python
# script cannot report a missing Python - it needs one to start, so the user gets
# "python: command not found" from the shell, which is not a diagnosis. That is a real job,
# and it is the ONLY job the deleted harnesses claimed; the difference is that those wrapped
# scripts you could not usefully run before having Python anyway.
#
# WHAT THIS REPORTS, AND WHAT IT DOES NOT. Presence and versions only - is there a thing
# called X, and what does it say its version is. It deliberately does NOT check the state of
# this checkout: whether the native dependencies are provisioned and current, whether the
# pinned inventories still match, whether icons/ are real images or unresolved LFS pointers.
# Those all turn on facts the build owns (marker strings, a wheel SHA-256, component digests),
# and copying them into shell would create a second place for them to drift - so they stay
# where they are enforced, in build_installer.py's preflight. **A green report here means the
# tools are installed, not that a release build will get past that preflight.**
#
# SHAPE, after flutter doctor: one line per CATEGORY - "[status] Name - what it is for (what
# was found)" - and a category that is not ok expands underneath with the problem and the
# exact command that fixes it, rather than collecting fixes into a footer the reader has to
# match back up by hand. A category groups tools that are installed together and fail
# together (cargo+rustc, git+git-lfs) and takes the WORST status among them, so a green line
# means every tool behind it answered.

param(
    [ValidateSet('app', 'installer', 'source', 'extension', 'all')]
    [string]$For = 'all'
)

$ErrorActionPreference = 'Continue'

$script:Failures = 0
$script:Warnings = 0

# Tier implication: asking for one tier includes the cheaper ones it builds on.
$Implies = @{
    'app'       = @('app')
    'installer' = @('app', 'installer')
    'source'    = @('app', 'installer', 'source')
    'extension' = @('extension')
    'all'       = @('app', 'installer', 'source', 'extension')
}
$Wanted = $Implies[$For]

# What each tier is FOR, said in the header. A reader who typed -For source should not have
# to go looking for what 'source' covers, and the implication above is invisible otherwise -
# so each description names the cheaper tiers it drags in rather than leaving them a surprise.
$Goal = @{
    'app'       = 'build and run kokoro-host and kokoro-panel'
    'installer' = 'the app, plus packaging it with NSIS'
    'source'    = 'the app and the installer, plus the source archive released with them'
    'extension' = 'work on the browser extension, which shares nothing with the rest'
    'all'       = 'the app, the installer, the source archive and the browser extension'
}

# Colour is presentation only - every status is still readable as plain text, because this
# output gets piped, redirected and pasted into issues. Honour NO_COLOR (the de facto
# convention) and fall back to plain when there is no console to colour, e.g. a host with no
# RawUI. Write-Host does not embed escape codes when its output is redirected, so a captured
# log stays clean on its own.
$script:UseColor = $true
if ($env:NO_COLOR) { $script:UseColor = $false }
if ($env:KKR_NO_COLOR) { $script:UseColor = $false }
try { if (-not $Host.UI.RawUI) { $script:UseColor = $false } } catch { $script:UseColor = $false }

$StatusColor = @{
    'ok'   = 'Green'
    'FAIL' = 'Red'
    'warn' = 'Yellow'
    'skip' = 'DarkGray'
}
$Marker = @{ 'ok' = '[+]'; 'FAIL' = '[x]'; 'warn' = '[!]'; 'skip' = '[-]' }
$Bullet = @{ 'ok' = '+'; 'FAIL' = 'x'; 'warn' = '!'; 'skip' = '-' }

function Write-Part([string]$Text, [string]$Color, [switch]$NoNewline) {
    if ($script:UseColor -and $Color) {
        Write-Host $Text -ForegroundColor $Color -NoNewline:$NoNewline
    } else {
        Write-Host $Text -NoNewline:$NoNewline
    }
}

# One category. $Problem is optional: element 0 is the headline printed beside the bullet,
# the rest are the explanation and the fix, indented under it.
function Report([string]$Tier, [string]$Status, [string]$Name, [string]$Purpose,
                [string]$Detail, [string[]]$Problem) {
    if ($Wanted -notcontains $Tier) { return }
    Write-Part ("{0} " -f $Marker[$Status]) $StatusColor[$Status] -NoNewline
    Write-Part $Name $null -NoNewline
    if ($Purpose) { Write-Part (" - " + $Purpose) 'DarkGray' -NoNewline }
    if ($Detail) { Write-Part (" (" + $Detail + ")") 'DarkGray' -NoNewline }
    Write-Host ""
    if ($Problem -and $Problem.Count -gt 0) {
        Write-Part ("    {0} " -f $Bullet[$Status]) $StatusColor[$Status] -NoNewline
        $head = $Problem[0]
        Write-Part $head $null
        if ($Problem.Count -gt 1) {
            foreach ($line in $Problem[1..($Problem.Count - 1)]) {
                Write-Part ("      " + $line) 'DarkGray'
            }
        }
    }
    if ($Status -eq 'FAIL') { $script:Failures++ }
    if ($Status -eq 'warn') { $script:Warnings++ }
}

# Resolve by RUNNING the candidate, never by looking at the file. py/python/python3 under
# %LOCALAPPDATA%\Microsoft\WindowsApps\ are App Execution Aliases: 0-byte reparse points
# that run a real Python when one is installed and open the Store when it is not, so a size
# or existence check rejects a working install.
function Get-ToolVersion([string]$Exe, [string[]]$Argv) {
    if (-not (Get-Command $Exe -ErrorAction SilentlyContinue)) { return $null }
    try { $out = & $Exe @Argv 2>$null } catch { return $null }
    if ($LASTEXITCODE -ne 0) { return $null }
    if (-not $out) { return '' }
    return (@($out)[0]).ToString().Trim()
}

# The version banners are mostly build metadata; a category line wants the number.
function Get-ShortVersion([string]$Text) {
    if (-not $Text) { return $null }
    if ($Text -match '(\d+\.\d+(\.\d+)?)') { return $matches[1] }
    return $Text
}

Write-Host ""
Write-Part ("Doctor summary for '{0}' - {1}." -f $For, $Goal[$For]) 'Cyan'
Write-Part "Tools only, not this checkout's provisioned state - that is build_installer.py's job." 'DarkGray'
if ($For -eq 'all') {
    Write-Part "Narrow it with -For app | installer | source | extension." 'DarkGray'
}
Write-Host ""

# --- Python ---------------------------------------------------------------------------------
# ONE category, not one row per candidate: on a normal Windows install python/py/python3 are
# three names for the same interpreter (all three are App Execution Aliases here), so probing
# each in turn reported the same version three times. First match wins, in the order the docs
# use, and the detail names which one answered - that matters when it is NOT 'python', since
# every entry point is invoked as 'python <script>.py'.
$Python = $null
$PythonVersion = $null
foreach ($cand in @('python', 'py', 'python3')) {
    $v = Get-ToolVersion $cand @('--version')
    if ($v -and $v -match 'Python 3') { $Python = $cand; $PythonVersion = $v; break }
}
if ($Python) {
    Report 'app' 'ok' 'Python' 'every build and provisioning entry point' `
        ("{0}, via {1}" -f (Get-ShortVersion $PythonVersion), $Python) $null
} else {
    Report 'app' 'FAIL' 'Python' 'every build and provisioning entry point' $null @(
        'No python, py or python3 on PATH reports Python 3.',
        'Everything under packaging/ and native-deps/ is invoked as "python <script>.py";',
        'nothing in this tree resolves an interpreter for you.',
        'Install from https://www.python.org/downloads/ and tick "Add to PATH".')
}

# --- Rust -----------------------------------------------------------------------------------
$cargo = Get-ToolVersion 'cargo' @('--version')
$rustc = Get-ToolVersion 'rustc' @('--version')
if ($cargo -and $rustc) {
    Report 'app' 'ok' 'Rust' 'compiles the five shipped binaries' `
        ("cargo {0}, rustc {1}" -f (Get-ShortVersion $cargo), (Get-ShortVersion $rustc)) $null
} else {
    $missing = @()
    if (-not $cargo) { $missing += 'cargo' }
    if (-not $rustc) { $missing += 'rustc' }
    Report 'app' 'FAIL' 'Rust' 'compiles the five shipped binaries' $null @(
        ("Not found: {0}." -f ($missing -join ', ')),
        'Install Rust from https://rustup.rs - rustup brings both, and the target below.')
}

# Kindle is a 32-bit process and loads the COM shim in-process, so the SAPI DLL, the hook
# and the injector are x86. Without this target they cannot be built at all.
if (-not (Get-ToolVersion 'rustup' @('--version'))) {
    Report 'app' 'skip' 'Rust x86 target' 'the SAPI shim, hook and injector Kindle loads' $null @(
        'rustup not found, so the installed targets cannot be listed.',
        'A gap in this report only - a Rust installed another way may still have it.')
} else {
    $installed = & rustup target list --installed 2>$null
    if ($installed -contains 'i686-pc-windows-msvc') {
        Report 'app' 'ok' 'Rust x86 target' 'the SAPI shim, hook and injector Kindle loads' `
            'i686-pc-windows-msvc' $null
    } else {
        Report 'app' 'FAIL' 'Rust x86 target' 'the SAPI shim, hook and injector Kindle loads' $null @(
            'i686-pc-windows-msvc is not installed.',
            'Kindle is a 32-bit process and loads KokoroSapi.dll in-process, so those',
            'three artifacts cannot be built for any other target.',
            'Run: rustup target add i686-pc-windows-msvc')
    }
}

# --- Git, and the LFS filter the icons need --------------------------------------------------
$git = Get-ToolVersion 'git' @('--version')
$lfs = Get-ToolVersion 'git' @('lfs', 'version')
if ($git -and $lfs) {
    Report 'app' 'ok' 'Git' 'the checkout, and the LFS filter icons/ needs' `
        ("git {0}, git-lfs {1}" -f (Get-ShortVersion $git), (Get-ShortVersion $lfs)) $null
} elseif (-not $git) {
    Report 'app' 'FAIL' 'Git' 'the checkout, and the LFS filter icons/ needs' $null @(
        'git not found.',
        'Install Git for Windows from https://git-scm.com/download/win')
} else {
    Report 'app' 'FAIL' 'Git' 'the checkout, and the LFS filter icons/ needs' `
        ("git {0}" -f (Get-ShortVersion $git)) @(
        'git-lfs not found.',
        'icons/* live in LFS, so without the filter they check out as small pointer',
        'stubs and the exes and installer build with broken icons.',
        'Install Git LFS, then run: git lfs install; git lfs pull')
}

# --- Packaging --------------------------------------------------------------------------------
$nsis = Join-Path ${env:ProgramFiles(x86)} 'NSIS\makensis.exe'
if (-not (Test-Path -LiteralPath $nsis)) {
    Report 'installer' 'FAIL' 'NSIS' 'compiles the installer and uninstaller stub' $null @(
        ("makensis not found at {0}." -f $nsis),
        'build_installer.py reads makensis /VERSION and accepts 3.12 only: the staged',
        'COPYING, components.toml and the source archive all describe that one.',
        'Run: choco install nsis --version=3.12.0 -y')
} else {
    $v = (& $nsis /VERSION 2>$null | Out-String).Trim()
    if ($v -eq 'v3.12') {
        Report 'installer' 'ok' 'NSIS' 'compiles the installer and uninstaller stub' `
            'v3.12, the pinned version' $null
    } else {
        Report 'installer' 'FAIL' 'NSIS' 'compiles the installer and uninstaller stub' $null @(
            ("Found {0}; build_installer.py accepts 3.12 only." -f $v),
            'The pin is enforced, not documentary: the staged files and the source',
            'archive all name that version.',
            'Run: choco install nsis --version=3.12.0 -y --force')
    }
}

$v = Get-ToolVersion 'cargo' @('about', '--version')
if (-not $v) {
    Report 'installer' 'FAIL' 'cargo-about' 'generates the dependency notices the installer bundles' $null @(
        'cargo about not found.',
        'It is not part of cargo: cargo dispatches "cargo about" to a cargo-about',
        'binary on PATH, which you install yourself. build_installer.py calls',
        'generate_dependency_licenses.py mid-build and THROWS without it, so a missing',
        'cargo-about breaks the build itself, not just an optional step.',
        'Run: cargo install cargo-about --version 0.9.1 --locked --features cli')
} elseif ($v -notmatch '0\.9\.1') {
    Report 'installer' 'warn' 'cargo-about' 'generates the dependency notices the installer bundles' (Get-ShortVersion $v) @(
        ("{0} found; CI pins 0.9.1." -f (Get-ShortVersion $v)),
        'The rendered notices are compared by hash, so another version can produce a',
        'notice tree that differs from the one CI builds.',
        'Run: cargo install cargo-about --version 0.9.1 --locked --features cli')
} else {
    Report 'installer' 'ok' 'cargo-about' 'generates the dependency notices the installer bundles' (Get-ShortVersion $v) $null
}

$sevenZip = $null
foreach ($cand in @('7z', '7za')) {
    if (Get-Command $cand -ErrorAction SilentlyContinue) { $sevenZip = $cand; break }
}
if (-not $sevenZip) {
    foreach ($cand in @((Join-Path $env:ProgramFiles '7-Zip\7z.exe'),
                        (Join-Path ${env:ProgramFiles(x86)} '7-Zip\7z.exe'))) {
        if (Test-Path -LiteralPath $cand) { $sevenZip = $cand; break }
    }
}
if ($sevenZip) {
    Report 'installer' 'ok' '7-Zip' 'unpacks the built installer to verify its notices' `
        $sevenZip $null
} else {
    # warn, NOT FAIL: 7-Zip is not a build prerequisite. build_installer.py never calls it -
    # only verify_installer_notices.py does, after the build. Failing the run over it would
    # report "not ready" to someone who is.
    Report 'installer' 'warn' '7-Zip' 'unpacks the built installer to verify its notices' $null @(
        'Not found.',
        'Only verify_installer_notices.py uses it, and only after the build has run -',
        'a machine without 7-Zip still builds a correct installer.',
        'Install from https://www.7-zip.org/, or run: choco install 7zip -y')
}

# --- Corresponding source -----------------------------------------------------------------------
$sysroot = Get-ToolVersion 'rustc' @('--print', 'sysroot')
if (-not $sysroot) {
    Report 'source' 'skip' 'rust-src' 'the standard-library source the release ships' $null @(
        'rustc not available, so its sysroot cannot be located.')
} else {
    $lib = Join-Path $sysroot 'lib\rustlib\src\rust\library\std\Cargo.toml'
    if (Test-Path -LiteralPath $lib) {
        Report 'source' 'ok' 'rust-src' 'the standard-library source the release ships' `
            'present in the active toolchain' $null
    } else {
        Report 'source' 'FAIL' 'rust-src' 'the standard-library source the release ships' $null @(
            'The rust-src component is not installed.',
            'Every shipped binary statically links the standard library, which is not a',
            'Cargo package - so cargo-about never sees it, and build_corresponding_source.py',
            'copies this exact library/ tree into the source archive instead.',
            'Run: rustup component add rust-src')
    }
}

# --- Browser extension ---------------------------------------------------------------------------
$v = Get-ToolVersion 'bun' @('--version')
if ($v) {
    Report 'extension' 'ok' 'bun' 'builds the browser extension and runs its tests' `
        (Get-ShortVersion $v) $null
} else {
    Report 'extension' 'FAIL' 'bun' 'builds the browser extension and runs its tests' $null @(
        'bun not found.',
        'It builds kokoro-browser-extension/ and runs its 145 tests; no CI workflow',
        'runs those, so this machine is the only place they run at all.',
        'Install from https://bun.sh')
}

# NOTE: this reports TOOLS, not the state of this checkout. Whether the native dependencies
# are provisioned and current (the ORT and espeak markers), whether the runtime libraries
# and notices are there, whether the component and licence-text inventories still match, and
# whether icons/ are real images rather than unresolved LFS pointers are all things only the
# build itself checks - build_installer.py's preflight, which stops at the FIRST problem. So
# a green report here means the tools are installed, not that a release build will get past
# its preflight.

# A clean run says nothing: every line is already a green [+], so a closing "no issues found"
# only adds a line to read on the runs that need the least reading. The footer exists to say
# how much is wrong, so it appears only when something is.
Write-Host ""
$issues = $script:Failures + $script:Warnings
if ($issues -eq 0) { exit 0 }

$word = 'categories'
if ($issues -eq 1) { $word = 'category' }
$color = 'Yellow'
if ($script:Failures -gt 0) { $color = 'Red' }
Write-Part ("! Doctor found issues in {0} {1}." -f $issues, $word) $color
if ($script:Failures -gt 0) { exit 1 }
Write-Part ("  Nothing blocking - ready for '{0}'." -f $For) 'Green'
exit 0
