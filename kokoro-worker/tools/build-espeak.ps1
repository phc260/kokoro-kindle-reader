# Build libespeak-ng as an x64 shared lib + compile espeak-ng-data, pinned to the
# EXACT phoneme behavior kokoro-js's `phonemizer` npm pkg uses, so native
# phonemization is byte-identical to the WebView2 edition. Verified 18/18 token
# parity (see the DirectML edition's tools/cmp_tok.py + phon_ref.mjs corpus).
# x64 for the WebGPU worker (the onnxruntime-webgpu runtime is x64-only); the
# phoneme pinning is EP- and arch-independent.
#
# Two things make parity exact:
#   1. Pin espeak-ng to tag 1.52.0. Master (post-1.52.0) adds a stray `ʲ`
#      palatalization after high front vowels that phonemizer's bundled espeak
#      lacks; 1.52.0 does not.
#   2. Revert the "horse-hoarse merger" (commit 5b01dd86, phsource/ph_english_us
#      phoneme `o@`): phonemizer bundles a PRE-merger espeak, so "for/four/-ore"
#      words must emit `oː` (id 57), not the post-merger `ɔː` (id 76). We only
#      touch the `ipa` lines - the FMT/formant is irrelevant since we consume
#      espeak's IPA text, not its audio.
# The Kokoro model was trained on phonemizer's output, so matching it (pre-merger)
# is correct for THIS model even though `ɔː` is the more modern General American.
#
# third_party/espeak-ng-src is a gitignored clone; re-run this after a fresh clone.
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot                  # kokoro-sapi/
$src  = Join-Path $root 'third_party\espeak-ng-src'
if (-not (Test-Path (Join-Path $src '.git'))) {
    throw "espeak-ng source not at $src - clone it first:`n" +
          "  git clone https://github.com/espeak-ng/espeak-ng.git `"$src`""
}

# 1. Pin to 1.52.0.
$tag = (& git -C $src describe --tags --exact-match 2>$null)
if ($tag -ne '1.52.0') {
    Write-Host "checking out espeak-ng 1.52.0 (was: $tag)"
    & git -C $src stash --quiet 2>$null
    & git -C $src checkout 1.52.0 --quiet
    if ($LASTEXITCODE -ne 0) { throw "git checkout 1.52.0 failed" }
}

# 2. Revert the horse-hoarse merger for phoneme o@ ONLY (idempotent).
#    phonemizer's pre-merger espeak distinguishes O@ (horse/for/north -> ɔː) from
#    o@ (hoarse/four/shore/more/-ore -> oː). 1.52.0 merged o@ into ɔː; restore o@
#    to oː and leave O@ as ɔː. Verified against the kokoro-js/phonemizer oracle.
#    Built from [char] codes so this script needs NO non-ASCII literals — PS 5.1
#    misreads a UTF-8-without-BOM .ps1's ɔː/ɹ, silently breaking a literal -replace.
#    NB: PowerShell variable names are case-insensitive, so the two markers must
#    differ by more than case ($merged vs $reverted, NOT $OO vs $oo).
$ph = Join-Path $src 'phsource\ph_english_us'
$merged   = [System.Char]::ConvertFromUtf32(0x0254) + [System.Char]::ConvertFromUtf32(0x02D0)  # ɔː
$reverted = 'o' + [System.Char]::ConvertFromUtf32(0x02D0)                                        # oː
$txt = [System.IO.File]::ReadAllText($ph, [System.Text.Encoding]::UTF8)
$m   = [regex]::Match($txt, 'phoneme o@.*?endphoneme', 'Singleline')
if ($m.Success -and $m.Value.Contains($merged)) {
    $block = $m.Value.Replace($merged, $reverted)
    $txt = $txt.Substring(0, $m.Index) + $block + $txt.Substring($m.Index + $m.Length)
    [System.IO.File]::WriteAllText($ph, $txt, (New-Object System.Text.UTF8Encoding($false)))
    Write-Host "reverted horse-hoarse merger in phoneme o@"
} else {
    Write-Host "horse-hoarse revert already applied or o@ block absent (verify parity)"
}

# 3. Configure + build x64 (vcvarsall x64 -> NMake, so cl targets x64). No
#    audio/async/mbrola deps - we only call espeak_Synth for the phoneme trace.
$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$vsPath  = (& $vswhere -latest -products * -property installationPath).Trim()
$vcvars  = Join-Path $vsPath 'VC\Auxiliary\Build\vcvarsall.bat'
$build   = Join-Path $src 'build-x64'
if (Test-Path (Join-Path $build 'CMakeCache.txt')) { Remove-Item -Recurse -Force $build }

$cfg = "cmake -S `"$src`" -B `"$build`" -G `"NMake Makefiles`" " +
       "-DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=ON " +
       "-DUSE_ASYNC=OFF -DUSE_MBROLA=OFF -DUSE_LIBSONIC=OFF -DUSE_LIBPCAUDIO=OFF " +
       "-DESPEAK_BUILD_DOC=OFF"
$bld = "cmake --build `"$build`""
cmd /D /c "`"$vcvars`" x64 && $cfg && $bld"
if ($LASTEXITCODE -ne 0) { throw "espeak-ng x64 build failed ($LASTEXITCODE)" }

Write-Host "`n=== artifacts ==="
Get-ChildItem $build -Recurse -Include 'libespeak-ng.dll','espeak-ng.exe' -ErrorAction SilentlyContinue |
  Select-Object FullName, Length
$data = Join-Path $build 'espeak-ng-data'
if (Test-Path $data) { Write-Host "data dir: $data ($((Get-ChildItem $data -Recurse -File | Measure-Object).Count) files)" }
