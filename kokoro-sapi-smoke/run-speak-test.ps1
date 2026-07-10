# One-command Speak-path test for the Rust SAPI DLL: build the DLL + host + harness,
# make sure kokoro-host is serving the pipe (reuse a running one, else launch a
# throwaway), run the smoke harness (COM checks + the real Speak/pipe/audio path),
# then stop only the host this script started. No Kindle, no registration, no
# elevation. ASCII-only (PowerShell 5.1).
#
#   .\kokoro-sapi-smoke\run-speak-test.ps1
#   .\kokoro-sapi-smoke\run-speak-test.ps1 -Wav out.wav   # also dump the audio
param([string]$Wav)
$ErrorActionPreference = 'Stop'
$target = 'i686-pc-windows-msvc'
$root = Split-Path $PSScriptRoot -Parent

$dll   = Join-Path $root "kokoro-sapi\target\$target\release\KokoroSapi.dll"
$smoke = Join-Path $root "kokoro-sapi-smoke\target\$target\release\smoke.exe"
$hostExe = Join-Path $root 'kokoro-host\target\debug\kokoro-host.exe'

# 1. Build the x86 DLL + harness and the x64 host.
Write-Host '==> Building DLL + harness (x86) and host (x64)'
cargo build --release --target $target --manifest-path (Join-Path $root 'kokoro-sapi\Cargo.toml')
if ($LASTEXITCODE) { throw 'DLL build failed' }
cargo build --release --target $target --manifest-path (Join-Path $root 'kokoro-sapi-smoke\Cargo.toml')
if ($LASTEXITCODE) { throw 'harness build failed' }
cargo build --manifest-path (Join-Path $root 'kokoro-host\Cargo.toml')
if ($LASTEXITCODE) { throw 'host build failed (did you run native-deps\fetch-deps.ps1?)' }

# 2. Ensure a host is serving the pipe. Reuse a running one; otherwise launch a
#    throwaway we own (and will stop). Note: real synthesis needs the model present.
function Test-Pipe { (Get-ChildItem '\\.\pipe\' -ErrorAction SilentlyContinue).Name -contains 'KokoroSapiSynth' }

$ownHost = $null
if (Get-Process kokoro-host -ErrorAction SilentlyContinue) {
    Write-Host '==> Reusing the already-running kokoro-host'
} else {
    Write-Host '==> Launching a throwaway kokoro-host'
    $ownHost = Start-Process -FilePath $hostExe -PassThru
}

# Wait up to ~15s for the pipe to appear.
$deadline = (Get-Date).AddSeconds(15)
while (-not (Test-Pipe) -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 300 }
if (-not (Test-Pipe)) { Write-Warning 'pipe never appeared; the Speak test will report SKIP' }

# 3. Run the harness (optionally dumping the audio for an A/B).
$smokeArgs = @($dll)
if ($Wav) { $smokeArgs += @('--wav', $Wav) }
try {
    Write-Host ''
    & $smoke @smokeArgs
    $code = $LASTEXITCODE
} finally {
    # 4. Stop only the host we started.
    if ($ownHost) {
        Write-Host ''
        Write-Host '==> Stopping the throwaway kokoro-host'
        Stop-Process -Id $ownHost.Id -Force -ErrorAction SilentlyContinue
    }
}
exit $code
