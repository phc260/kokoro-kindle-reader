# Harness. The provisioning recipe is fetch-deps.py, shared with Linux.
#
# It used to live here, in 346 lines of PowerShell, beside a bash twin that had to be kept
# pin-for-pin identical by hand. That invariant existed only because the recipe was
# duplicated, and its failure mode was the worst kind - a phoneme or pin difference does not
# raise an error, it makes one platform quietly build something else. One recipe, two thin
# harnesses.
#
# This keeps the entry point and the parameters callers already use:
#   .\fetch-deps.ps1            # provision (idempotent)
#   .\fetch-deps.ps1 -Force     # re-provision from scratch
param(
    [string]$OrtVersion = '1.27.0',
    [switch]$Force
)
$ErrorActionPreference = 'Stop'

# Python is a prerequisite on BOTH platforms now; it was not on Windows before, so the
# failure below says so plainly rather than letting the caller read a "term not recognized".
#
# Resolve by RUNNING each candidate, not by inspecting the file on disk. Windows App
# Execution Aliases (%LOCALAPPDATA%\Microsoft\WindowsApps\python.exe and friends) are
# 0-byte reparse points that work perfectly when Python is installed and open the Store when
# it is not - so a size or existence check rejects a good install. Measured on a machine
# where py, python and python3 are all 0-byte aliases and all three report Python 3.14.7.
# `--version` on a stub that has nothing behind it does not print "Python 3".
$python = $null
foreach ($candidate in @('py', 'python', 'python3')) {
    if (-not (Get-Command $candidate -ErrorAction SilentlyContinue)) { continue }
    $reported = & $candidate --version 2>&1
    if ($LASTEXITCODE -eq 0 -and ($reported -join ' ') -match 'Python 3') {
        $python = $candidate
        break
    }
}
if (-not $python) {
    throw ('Python 3 not found, and it is required to provision native dependencies. ' +
           'Install it from https://www.python.org/downloads/ (or `winget install Python.Python.3.12`) ' +
           'and re-run. The provisioning recipe is shared with Linux - see fetch-deps.py.')
}

$script = Join-Path $PSScriptRoot 'fetch-deps.py'
$argv = @($script, '--ort-version', $OrtVersion)
if ($Force) { $argv += '--force' }

& $python @argv
if ($LASTEXITCODE -ne 0) { throw "fetch-deps.py failed ($LASTEXITCODE)" }
