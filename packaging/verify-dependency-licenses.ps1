# Harness. The check is verify_dependency_licenses.py.
#
# Keeps the parameters its callers already pass (generate-dependency-licenses at the end of
# each crate, and verify-installer-notices.ps1 against the extracted installer):
#   .\verify-dependency-licenses.ps1 -Report <report.html>
#   .\verify-dependency-licenses.ps1 -Report <report.html> -Config <about.toml> -SourceNotices <json>
param(
    [Parameter(Mandatory = $true)][string]$Report,
    [string]$Config,
    [string]$SourceNotices
)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'python3.ps1')

$argv = @((Join-Path $PSScriptRoot 'verify_dependency_licenses.py'), '--report', $Report)
if ($Config) { $argv += @('--config', $Config) }
if ($SourceNotices) { $argv += @('--source-notices', $SourceNotices) }

& (Get-Python3) @argv
if ($LASTEXITCODE -ne 0) { throw 'Dependency licence verification FAILED (see above).' }
