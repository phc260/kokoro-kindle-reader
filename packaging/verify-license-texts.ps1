# Harness. The check is verify-license-texts.py.
#
# Keeps the parameters its callers already pass (build-installer.ps1 before packaging and
# against the extracted installer; license-check.yml on PRs):
#   .\verify-license-texts.ps1
#   .\verify-license-texts.ps1 -Root <extracted-tree> -AllowAdditional
param([string]$Root, [switch]$AllowAdditional)
$ErrorActionPreference = 'Stop'

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
    throw ('Python 3 not found, and it is required to verify the licence texts. Install it ' +
           'from https://www.python.org/downloads/ and re-run.')
}

$argv = @((Join-Path $PSScriptRoot 'verify-license-texts.py'))
if ($Root) { $argv += @('--root', $Root) }
if ($AllowAdditional) { $argv += '--allow-additional' }

& $python @argv
if ($LASTEXITCODE -ne 0) { throw 'Checked-in licence-text verification FAILED (see above).' }
