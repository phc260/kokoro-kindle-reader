# Harness. The check is verify_component_hashes.py.
#
# Takes no parameters, like the script it replaced; license-check.yml runs it directly.
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'python3.ps1')

& (Get-Python3) (Join-Path $PSScriptRoot 'verify_component_hashes.py')
if ($LASTEXITCODE -ne 0) { throw 'components.toml component-hash verification FAILED (see above).' }
