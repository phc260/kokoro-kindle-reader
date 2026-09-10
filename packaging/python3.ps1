# Resolve a Python 3 interpreter, for the harnesses beside this file. Dot-source it:
#   . (Join-Path $PSScriptRoot 'python3.ps1')
#   $python = Get-Python3
#
# The checks under packaging/ are Python now; these .ps1 files stay only so their callers
# (build-installer.ps1, verify-installer-notices.ps1, license-check.yml) keep working
# unchanged, and so Windows keeps one place that knows how to find an interpreter.
#
# Candidates are resolved by RUNNING them, never by looking at the file. `py`, `python` and
# `python3` under %LOCALAPPDATA%\Microsoft\WindowsApps\ are App Execution Aliases: 0-byte
# reparse points that either run the installed Python or open the Store. A launcher that
# rejects them on size rejects a perfectly good install - measured on the dev machine,
# where all three are 0-byte aliases and all three report Python 3.
function Get-Python3 {
    foreach ($candidate in @('py', 'python', 'python3')) {
        if (-not (Get-Command $candidate -ErrorAction SilentlyContinue)) { continue }
        $reported = & $candidate --version 2>&1
        if ($LASTEXITCODE -eq 0 -and ($reported -join ' ') -match 'Python 3') {
            return $candidate
        }
    }
    throw ('Python 3 not found, and it is required by the packaging checks. Install it ' +
           'from https://www.python.org/downloads/ and re-run.')
}
