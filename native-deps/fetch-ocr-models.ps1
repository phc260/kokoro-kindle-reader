# Harness. The recipe is fetch-ocr-models.py, shared with Linux.
#
# Keeps the entry point and parameters callers already use:
#   .\fetch-ocr-models.ps1              # provision (idempotent)
#   .\fetch-ocr-models.ps1 -Force       # re-fetch even if already pinned
#   .\fetch-ocr-models.ps1 -VerifyOnly  # check the pins, never touch the network
param(
    [switch]$Force,
    [switch]$VerifyOnly
)
$ErrorActionPreference = 'Stop'

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
    throw ('Python 3 not found, and it is required to provision the OCR models. Install it ' +
           'from https://www.python.org/downloads/ and re-run.')
}

$argv = @((Join-Path $PSScriptRoot 'fetch-ocr-models.py'))
if ($Force) { $argv += '--force' }
if ($VerifyOnly) { $argv += '--verify-only' }

& $python @argv
if ($LASTEXITCODE -ne 0) { throw "fetch-ocr-models.py failed ($LASTEXITCODE)" }
