# Build the NSIS installer: build the x86 Rust SAPI DLL (kokoro-sapi),
# release-build the tray host + Slint panel, stage everything the installer needs
# (both exes + native runtime DLLs + espeak data + the x86 KokoroSapi.dll + guard
# scripts), then run makensis.
#
#   packaging\build-installer.ps1            # full: build + stage + makensis
#   packaging\build-installer.ps1 -SkipBuild # reuse existing release binaries
#
# Output: packaging\kokoro-kindle-reader-<version>-setup.exe
param([switch]$SkipBuild)
$ErrorActionPreference = 'Stop'

$here = $PSScriptRoot
$root = Split-Path $here -Parent
$hostRel = Join-Path $root 'kokoro-host\target\release'
$panelRel = Join-Path $root 'kokoro-panel\target\release'
$sapiRs = Join-Path $root 'kokoro-sapi'

# 0. Fail fast on inventory drift: the in-repo assets pinned in components.toml (the five
#    Material Symbols SVGs compiled into kokoro-panel.exe) must still hash to their recorded
#    SHA-256. license-check.yml runs this on PRs, but a tag/manual build can start from a
#    commit that never went through a PR - so run it here too, before the long build, rather
#    than shipping a stale inventory. (verify-installer-notices.ps1 later proves the notice
#    tree is IN the built -setup.exe.)
Write-Host '==> Verifying non-Cargo component hashes (components.toml)'
& (Join-Path $here 'verify-component-hashes.ps1')  # throws on drift ($ErrorActionPreference=Stop)

#    Provisioned notices must be current too. Check the canonical ORT anchors before any
#    cargo build so a stale dependency cache costs seconds, not a full release build.
$ortNotices = Join-Path $root 'native-deps\runtime\notices'
$missingOrtNotices = @()
foreach ($noticeName in 'ORT-LICENSE.txt', 'ORT-ThirdPartyNotices.txt') {
    $noticePath = Join-Path $ortNotices $noticeName
    if (-not (Test-Path -LiteralPath $noticePath -PathType Leaf) -or
        (Get-Item -LiteralPath $noticePath -ErrorAction SilentlyContinue).Length -eq 0) {
        $missingOrtNotices += $noticeName
    }
}
if ($missingOrtNotices.Count) {
    throw ("ONNX Runtime notice provision is missing or stale at $ortNotices " +
           "($($missingOrtNotices -join ', ')) - run native-deps\fetch-deps.ps1. " +
           'It provisions the canonical files from the same wheel as the runtime DLLs.')
}

# 1. Build the x86 SAPI DLL (Kindle is 32-bit, loads it in-process). The Rust engine
#    is connect-only -- it forwards Speak to kokoro-host over the pipe -- so there's no
#    ONNX/espeak dep here.
Write-Host '==> cargo build --release --target i686-pc-windows-msvc (kokoro-sapi)'
Push-Location $sapiRs
cargo build --release --target i686-pc-windows-msvc
if ($LASTEXITCODE) { throw 'SAPI DLL build failed (need the i686-pc-windows-msvc target?)' }
Pop-Location
$sapiDll = Join-Path $sapiRs 'target\i686-pc-windows-msvc\release\KokoroSapi.dll'

# 1b. Build the x86 Kindle-hook DLL + injector (Kindle is 32-bit). The host's watcher spawns
#     the injector, which LoadLibrary-loads the hook into Kindle to force the Kokoro voice.
foreach ($c in 'kokoro-hook', 'kokoro-inject') {
    Write-Host "==> cargo build --release --target i686-pc-windows-msvc ($c)"
    Push-Location (Join-Path $root $c)
    cargo build --release --target i686-pc-windows-msvc
    if ($LASTEXITCODE) { throw "$c build failed (need the i686-pc-windows-msvc target?)" }
    Pop-Location
}
$hookDll = Join-Path $root 'kokoro-hook\target\i686-pc-windows-msvc\release\kokoro_hook.dll'
$injectExe = Join-Path $root 'kokoro-inject\target\i686-pc-windows-msvc\release\kokoro-inject.exe'

# 2. Release-build both Rust crates (each stages its own runtime next to the exe).
if (-not $SkipBuild) {
    Write-Host '==> cargo build --release (kokoro-host)'
    Push-Location (Join-Path $root 'kokoro-host'); cargo build --release; if ($LASTEXITCODE) { throw 'host build failed' }; Pop-Location
    Write-Host '==> cargo build --release (kokoro-panel)'
    Push-Location (Join-Path $root 'kokoro-panel'); cargo build --release; if ($LASTEXITCODE) { throw 'panel build failed' }; Pop-Location
}

# 3. Stage the bundle.
$stage = Join-Path $here 'staging'
Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $stage, (Join-Path $stage 'resources') | Out-Null

Copy-Item (Join-Path $hostRel 'kokoro-host.exe') $stage
Copy-Item (Join-Path $panelRel 'kokoro-panel.exe') $stage
foreach ($d in 'onnxruntime.dll', 'onnxruntime_providers_shared.dll', 'dxcompiler.dll', 'dxil.dll', 'espeak-ng.dll') {
    Copy-Item (Join-Path $hostRel $d) $stage
}
Copy-Item -Recurse (Join-Path $hostRel 'espeak-ng-data') $stage
Copy-Item (Join-Path $root 'icons\icon.ico') (Join-Path $stage 'icon.ico')

# 3a. The Cloud Reader OCR models are NOT bundled. Like the Kokoro voice model, they are
#     DOWNLOADED at first run - by the panel, into <app_data>/ocr/, per ocr-manifest.json and
#     SHA-256-verified (kokoro-panel::download). So there is nothing to stage here: a fresh
#     install ships no ocr\ dir, the host answers /ocr with `missing` until the download
#     lands, and the browser extension surfaces that state. This keeps ~10 MB of a
#     browser-only asset out of the installer (and out of every Kindle-only user's download).
#     fetch-ocr-models.ps1 still provisions them for DEV (`cargo run` reads native-deps\ocr).

# 3b. License texts. The bundle links espeak-ng (GPL-3.0-or-later, and MODIFIED -- see
#     native-deps\build-espeak.ps1) and Slint under its GPL-3.0-only option, so the
#     installed app as a whole is conveyed under GPLv3: the notices + the GPL text must
#     ship WITH the binaries, not just live in the repo. THIRD_PARTY_NOTICES.md links
#     LICENSE and licenses\*, so keep all three together and keep the layout.
Copy-Item (Join-Path $root 'LICENSE') $stage
Copy-Item (Join-Path $root 'THIRD_PARTY_NOTICES.md') $stage
Copy-Item -Recurse (Join-Path $root 'licenses') $stage

#     ONNX Runtime's own licence + notice set, staged from native-deps rather than kept in
#     the repo, so it stays matched to the exact wheel the shipped DLLs came out of. Four
#     of the binaries we install come from that wheel, and dxcompiler.dll's licence
#     (University of Illinois/NCSA) requires its notice accompany them.
#     The fail-fast check above already proved the canonical files exist and are non-empty.
#     Throwing beats shipping without: an installer missing licence text looks complete and
#     is not, which is the same trap as the OCR models above.
$ortStage = Join-Path $stage 'licenses\onnxruntime'
New-Item -ItemType Directory -Force $ortStage | Out-Null
# -Recurse: fetch-deps.ps1 now preserves the wheel's own directory structure under
# native-deps\runtime\notices\ (collision-proofing - see its comment), so a flat
# wildcard copy would silently drop everything inside a subdirectory.
Copy-Item (Join-Path $ortNotices '*') $ortStage -Force -Recurse

#     espeak-ng's own COPYING* set (GPLv3 + Apache + BSD2 + UCD), provisioned by
#     fetch-deps.ps1 from the exact 1.52.0 clone we build. We ship a MODIFIED espeak-ng.dll
#     + espeak-ng-data/, and COPYING.UCD in particular covers the Unicode data baked into
#     espeak-ng-data/ and is a DIFFERENT document from licenses\Unicode-3.0.txt. Same
#     fail-loud contract as the ORT notices: missing text looks complete and is not.
$espkNotices = Join-Path $root 'native-deps\espeak-ng-notices'
if (-not (Test-Path (Join-Path $espkNotices '*'))) {
    throw ("No espeak-ng notices at $espkNotices - run native-deps\fetch-deps.ps1 " +
           '(it provisions COPYING* alongside the espeak build).')
}
$espkStage = Join-Path $stage 'licenses\espeak-ng'
New-Item -ItemType Directory -Force $espkStage | Out-Null
Copy-Item (Join-Path $espkNotices '*') $espkStage -Force

#     The Cargo dependency closure's own licence notices - generated fresh from the
#     Cargo.lock files this build just compiled against, not a hand-maintained prose
#     list (see generate-dependency-licenses.ps1 for why, and THIRD_PARTY_NOTICES.md's
#     "Cargo crates" section for the human-readable pointer to it). Regenerating on every
#     build, rather than provisioning once like the ORT notices, is deliberate: this
#     closure moves with ordinary `cargo update`s in a way the ORT wheel version does
#     not, and a stale copy here is exactly the kind of drift this mechanism exists to
#     catch instead of silently missing.
Write-Host '==> Generating Rust dependency licence notices'
& (Join-Path $root 'packaging\generate-dependency-licenses.ps1')
if ($LASTEXITCODE) { throw 'generate-dependency-licenses.ps1 failed' }
$depLicSrc = Join-Path $here 'dependency-licenses'
$depLicStage = Join-Path $stage 'licenses\dependencies'
New-Item -ItemType Directory -Force $depLicStage | Out-Null
Copy-Item (Join-Path $depLicSrc '*') $depLicStage -Force

#     The Rust standard library is statically linked into every Rust output but is NOT a
#     Cargo package, so cargo-about cannot see it. Rust ships a generated per-toolchain
#     COPYRIGHT-library.html that covers std/core/alloc/compiler-builtins and their bundled
#     source dependencies. Stage that exact file from the rustc sysroot used for this build,
#     plus rustc's release and immutable commit, rather than checking in a copy that would
#     drift whenever the `stable` toolchain moves.
$rustSysroot = (& rustc --print sysroot)
if ($LASTEXITCODE -or -not $rustSysroot) { throw 'rustc --print sysroot failed.' }
$rustSysroot = $rustSysroot.Trim()
$rustStdNotice = Join-Path $rustSysroot 'share\doc\rust\COPYRIGHT-library.html'
if (-not (Test-Path -LiteralPath $rustStdNotice -PathType Leaf)) {
    throw ("Rust standard-library notice not found at $rustStdNotice - the Rust toolchain " +
           'cannot be redistributed without its generated copyright/licence report.')
}
$rustNoticeText = [System.IO.File]::ReadAllText($rustStdNotice)
if (-not $rustNoticeText.Contains('Copyright notices for The Rust Standard Library')) {
    throw "Unexpected Rust standard-library notice content: $rustStdNotice"
}
$rustToolchain = @(& rustc --version --verbose)
if ($LASTEXITCODE -or $rustToolchain.Count -eq 0 -or
    -not (($rustToolchain -join "`n") -match '(?m)^commit-hash:\s+[0-9a-f]{40}\s*$')) {
    throw 'Could not record rustc release + immutable commit for the shipped standard library.'
}
$rustStage = Join-Path $stage 'licenses\rust'
New-Item -ItemType Directory -Force $rustStage | Out-Null
Copy-Item -LiteralPath $rustStdNotice (Join-Path $rustStage 'COPYRIGHT-library.html') -Force
[System.IO.File]::WriteAllLines(
    (Join-Path $rustStage 'TOOLCHAIN.txt'),
    [string[]]$rustToolchain,
    [System.Text.Encoding]::ASCII
)

$res = Join-Path $stage 'resources'
Copy-Item $sapiDll $res
Copy-Item $hookDll $res
Copy-Item $injectExe $res
Copy-Item (Join-Path $sapiRs 'kindle-voice-guard.ps1') $res
Copy-Item (Join-Path $sapiRs 'voice-setup.ps1') $res

# 4. Compile the installer.
$makensis = 'C:\Program Files (x86)\NSIS\makensis.exe'
if (-not (Test-Path $makensis)) { throw "makensis not found at $makensis - install NSIS." }

#     NSIS's own licence. The installer/uninstaller stub is NSIS, compressed with LZMA
#     (installer.nsi: SetCompressor /SOLID lzma), so the shipped stub carries NSIS's
#     zlib/libpng + bzip2 + CPL-1.0 (LZMA module, with its linking exception) terms. Ship
#     NSIS's own COPYING verbatim from the installed toolchain, so it always matches the
#     NSIS version this build used (pinned in CI) rather than a checked-in copy that goes
#     stale on a version bump - same reasoning as the ORT/espeak notices.
$nsisCopying = Join-Path (Split-Path $makensis -Parent) 'COPYING'
if (-not (Test-Path $nsisCopying)) {
    throw ("NSIS COPYING not found at $nsisCopying - the installed NSIS is missing its " +
           'licence file; the LZMA-compressed stub must ship NSIS''s licence terms.')
}
$nsisStage = Join-Path $stage 'licenses\nsis'
New-Item -ItemType Directory -Force $nsisStage | Out-Null
Copy-Item $nsisCopying (Join-Path $nsisStage 'NSIS-COPYING.txt') -Force

Write-Host '==> makensis'
& $makensis (Join-Path $here 'installer.nsi')
if ($LASTEXITCODE) { throw 'makensis failed' }

$out = Get-ChildItem (Join-Path $here '*-setup.exe') | Sort-Object LastWriteTime -Descending | Select-Object -First 1
Write-Host "==> Installer: $($out.FullName)  ($([math]::Round($out.Length/1MB,1)) MB)"
