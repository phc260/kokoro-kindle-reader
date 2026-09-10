# Harness. The offline regression fixtures are test_dependency_licenses.py.
#
# Takes no parameters, like the script it replaced; license-check.yml runs it directly.
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'python3.ps1')

& (Get-Python3) (Join-Path $PSScriptRoot 'test_dependency_licenses.py')
if ($LASTEXITCODE -ne 0) { throw 'Offline dependency-notice checks FAILED (see above).' }
