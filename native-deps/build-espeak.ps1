# Build libespeak-ng as an x64 shared lib + compile espeak-ng-data, pinned to the
# EXACT phoneme behavior kokoro-js's `phonemizer` npm pkg uses, so native
# phonemization is byte-identical to the WebView2 edition. Verified 18/18 token
# parity (see the DirectML edition's tools/cmp_tok.py + phon_ref.mjs corpus).
# x64 for the WebGPU worker (the onnxruntime-webgpu runtime is x64-only); the
# phoneme pinning is EP- and arch-independent.
#
# Two things make parity exact:
#   1. Pin espeak-ng to tag 1.52.0. Master (post-1.52.0) adds a stray
#      palatalization after high front vowels that phonemizer's bundled espeak lacks;
#      1.52.0 does not.
#   2. Revert the "horse-hoarse merger" (commit 5b01dd86, phsource/ph_english_us
#      phoneme `o@`): phonemizer bundles a PRE-merger espeak, so "for/four/-ore"
#      words must emit long close-mid back rounded `o` (id 57), not the post-merger
#      open-mid back rounded vowel (id 76). We only touch the `ipa` lines - the
#      FMT/formant is irrelevant since we consume
#      espeak's IPA text, not its audio.
# The Kokoro model was trained on phonemizer's output, so matching it (pre-merger)
# is correct for THIS model even though the merger is more modern General American.
#
# espeak-ng-src is a gitignored clone next to this script; re-run after a fresh clone.
$ErrorActionPreference = 'Stop'
$src  = Join-Path $PSScriptRoot 'espeak-ng-src'          # native-deps/espeak-ng-src
if (-not (Test-Path (Join-Path $src '.git'))) {
    throw "espeak-ng source not at $src - clone it first:`n" +
          "  git clone https://github.com/espeak-ng/espeak-ng.git `"$src`""
}

# 1. Pin to the immutable commit behind 1.52.0, not only the mutable tag name.
$expectedCommit = '4870adfa25b1a32b4361592f1be8a40337c58d6c'
$currentCommit = (& git -C $src rev-parse HEAD 2>$null).Trim()
if ($currentCommit -ne $expectedCommit) {
    Write-Host "checking out espeak-ng 1.52.0 commit $expectedCommit (was: $currentCommit)"
    & git -C $src stash --quiet 2>$null
    & git -C $src checkout $expectedCommit --quiet
    if ($LASTEXITCODE -ne 0) { throw "git checkout espeak-ng commit $expectedCommit failed" }
}
$currentCommit = (& git -C $src rev-parse HEAD 2>$null).Trim()
if ($currentCommit -ne $expectedCommit) {
    throw "espeak-ng source is $currentCommit, expected immutable 1.52.0 commit $expectedCommit"
}

# 2. Revert the horse-hoarse merger for phoneme o@ ONLY (idempotent).
#    phonemizer's pre-merger espeak distinguishes O@ (horse/for/north -> open-mid vowel)
#    from o@ (hoarse/four/shore/more/-ore -> close-mid vowel). 1.52.0 merged the two;
#    restore o@ and leave O@ unchanged. Verified against the kokoro-js/phonemizer oracle.
#    Built from [char] codes so this script needs NO non-ASCII literals - PS 5.1 misreads
#    a UTF-8-without-BOM script's IPA characters, silently breaking a literal -replace.
#    NB: PowerShell variable names are case-insensitive, so the two markers must
#    differ by more than case ($merged vs $reverted, NOT $OO vs $oo).
$ph = Join-Path $src 'phsource\ph_english_us'
$merged   = [System.Char]::ConvertFromUtf32(0x0254) + [System.Char]::ConvertFromUtf32(0x02D0)
$reverted = 'o' + [System.Char]::ConvertFromUtf32(0x02D0)
$txt = [System.IO.File]::ReadAllText($ph, [System.Text.Encoding]::UTF8)
$m   = [regex]::Match($txt, 'phoneme o@.*?endphoneme', 'Singleline')
if (-not $m.Success) {
    throw "espeak-ng $expectedCommit has no phoneme o@ block at $ph"
}
if ($m.Value.Contains($merged)) {
    $block = $m.Value.Replace($merged, $reverted)
    $txt = $txt.Substring(0, $m.Index) + $block + $txt.Substring($m.Index + $m.Length)
    [System.IO.File]::WriteAllText($ph, $txt, (New-Object System.Text.UTF8Encoding($false)))
    Write-Host "reverted horse-hoarse merger in phoneme o@"
} elseif ($m.Value.Contains($reverted)) {
    Write-Host "horse-hoarse revert already applied in phoneme o@"
} else {
    throw 'phoneme o@ contains neither the expected merged nor reverted IPA sequence'
}
$verified = [regex]::Match($txt, 'phoneme o@.*?endphoneme', 'Singleline')
if (-not $verified.Success -or $verified.Value.Contains($merged) -or
    -not $verified.Value.Contains($reverted)) {
    throw 'horse-hoarse revert verification failed; refusing to build an untracked variant'
}

# Prove that this is the one documented modification, not merely a tree where that one block
# also happens to look right. Normalize newlines so Git's Windows checkout policy cannot change
# the reviewed digest.
$normalized = $txt.Replace("`r`n", "`n").Replace("`r", "`n")
$utf8NoBom = New-Object System.Text.UTF8Encoding($false)
$sha = [System.Security.Cryptography.SHA256]::Create()
$patchedHash = [System.BitConverter]::ToString(
    $sha.ComputeHash($utf8NoBom.GetBytes($normalized))
).Replace('-', '').ToLower()
$sha.Dispose()
$expectedPatchedHash = 'ffa5cbde9ec07c8c76ac9e37e505861cbf1c7582e1c72ec90e1d21f9fa0bea23'
if ($patchedHash -cne $expectedPatchedHash) {
    throw "Patched ph_english_us hash is $patchedHash, expected $expectedPatchedHash"
}
$trackedChanges = @(& git -C $src diff --name-only)
$trackedExit = $LASTEXITCODE
$stagedChanges = @(& git -C $src diff --cached --name-only)
$stagedExit = $LASTEXITCODE
$untracked = @(& git -C $src ls-files --others --directory --no-empty-directory)
$untrackedExit = $LASTEXITCODE
if ($trackedExit -ne 0 -or $stagedExit -ne 0 -or $untrackedExit -ne 0) {
    throw 'Could not inspect the espeak-ng source tree.'
}
$unexpectedUntracked = @($untracked | Where-Object {
    $top = ($_ -split '/', 2)[0]
    @('build-x64', 'build') -notcontains $top
})
if ($trackedChanges.Count -ne 1 -or $trackedChanges[0] -cne 'phsource/ph_english_us' -or
    $stagedChanges.Count -ne 0 -or $unexpectedUntracked.Count -ne 0) {
    throw ('espeak-ng has modifications beyond the documented ph_english_us revert: ' +
           "tracked=[$($trackedChanges -join ', ')], staged=[$($stagedChanges -join ', ')], " +
           "untracked=[$($unexpectedUntracked -join ', ')]")
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
