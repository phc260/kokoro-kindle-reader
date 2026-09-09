# Harness. The espeak-ng build recipe is build-espeak.py, shared with Linux.
#
# The pin, the horse-hoarse revert, the patched-file digest and the clean-tree check all live
# there now, in one copy. They were duplicated here and in a bash twin, which is a dangerous
# thing to duplicate: a phoneme difference between the platforms does not surface as an
# error, it surfaces as the voice saying something slightly different, on one OS only.
#
# Normally invoked by fetch-deps.py rather than directly.
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
    throw ('Python 3 not found, and it is required to build espeak-ng. Install it from ' +
           'https://www.python.org/downloads/ and re-run.')
}

& $python (Join-Path $PSScriptRoot 'build-espeak.py')
if ($LASTEXITCODE -ne 0) { throw "build-espeak.py failed ($LASTEXITCODE)" }
