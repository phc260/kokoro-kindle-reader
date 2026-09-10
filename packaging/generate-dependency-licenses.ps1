# Harness. The generator is generate_dependency_licenses.py.
#
# Keeps the parameters its callers already pass (build-installer.ps1 on every build, and
# license-check.yml on PRs that touch the dependency closure):
#   .\generate-dependency-licenses.ps1
#   .\generate-dependency-licenses.ps1 -SkipCheck -OutputDir <dir>
param([switch]$SkipCheck, [string]$OutputDir)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'python3.ps1')

$argv = @((Join-Path $PSScriptRoot 'generate_dependency_licenses.py'))
if ($SkipCheck) { $argv += '--skip-check' }
if ($OutputDir) { $argv += @('--output-dir', $OutputDir) }

& (Get-Python3) @argv
if ($LASTEXITCODE -ne 0) { throw 'Dependency licence generation FAILED (see above).' }
