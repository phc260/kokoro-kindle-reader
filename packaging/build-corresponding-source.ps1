# Harness. The packager is build_corresponding_source.py.
#
# Keeps the parameters its callers already pass (installer.yml: the strict form on a tag,
# -AllowUncommitted for a manual CI run):
#   .\build-corresponding-source.ps1
#   .\build-corresponding-source.ps1 -Version 0.4.0
#   .\build-corresponding-source.ps1 -AllowUncommitted
param([string]$Version, [switch]$AllowUncommitted)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'python3.ps1')

$argv = @((Join-Path $PSScriptRoot 'build_corresponding_source.py'))
if ($Version) { $argv += @('--version', $Version) }
if ($AllowUncommitted) { $argv += '--allow-uncommitted' }

& (Get-Python3) @argv
if ($LASTEXITCODE -ne 0) { throw 'Corresponding-source build FAILED (see above).' }
