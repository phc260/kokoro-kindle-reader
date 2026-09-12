# Harness. The check is verify_installer_notices.py.
#
# Keeps the parameter its callers already pass (installer.yml after the build; also run by
# hand against any built installer):
#   .\verify-installer-notices.ps1
#   .\verify-installer-notices.ps1 -Setup path.exe
param([string]$Setup)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'python3.ps1')

$argv = @((Join-Path $PSScriptRoot 'verify_installer_notices.py'))
if ($Setup) { $argv += @('--setup', $Setup) }

& (Get-Python3) @argv
if ($LASTEXITCODE -ne 0) { throw 'Installer notice-tree verification FAILED (see above).' }
