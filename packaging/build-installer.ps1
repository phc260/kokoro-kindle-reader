# Harness. The build is build_installer.py.
#
# Keeps the parameter its callers already pass (installer.yml on a tag or manual run, and a
# developer building locally):
#   .\build-installer.ps1            # full: build + stage + makensis
#   .\build-installer.ps1 -SkipBuild # reuse existing release binaries
param([switch]$SkipBuild)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'python3.ps1')

$argv = @((Join-Path $PSScriptRoot 'build_installer.py'))
if ($SkipBuild) { $argv += '--skip-build' }

& (Get-Python3) @argv
if ($LASTEXITCODE -ne 0) { throw 'Installer build FAILED (see above).' }
