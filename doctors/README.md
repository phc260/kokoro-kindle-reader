# doctors

Developer toolchain checks. Run one before your first build: it lists every missing
development tool in one pass, with the command that installs it, so you don't have to discover
them one failed build at a time. It builds and installs nothing.

```powershell
.\doctors\doctor.cmd      # Windows, from the repo root
```

```bash
./doctors/doctor.sh       # Linux, from the repo root
```

Both are plain batch / shell, not Python and not `.ps1`. They have to report a missing Python,
and a `.cmd` runs under any PowerShell execution policy.

## Output

One line per tool, in install order:

```
[+] Rust (cargo 1.98.1)
[x] CMake - winget install Kitware.CMake
[!] 7-Zip - choco install 7zip -y (only needed to verify a built installer)
```

`[+]` found, `[x]` missing (the text after the dash is the fix), `[!]` optional. The exit code
is 1 if anything is `[x]`, otherwise 0. The markers are coloured; set `NO_COLOR` for plain
output.

## What is checked

The list lives in [`tools.conf`](tools.conf), which both scripts read: each tool's name,
version rule (`>=0.9.0`, `=3.12`, or `-` for presence only), whether it is required or
optional, and the fix to print on each platform (`-` where a platform doesn't use it). Its
order is the output order. The scripts hold only the detectors, the code that finds a tool
and reads its version, one per id: `:detect_<id>` in `doctor.cmd`, `detect_<id>` in
`doctor.sh`.

- **Changing a version, a fix or the order** needs an edit to `tools.conf` only.
- **Adding a tool** needs a row there, plus a detector in each script whose platform column
  isn't `-`.

The file's format rules are in its header. The main one is `-` rather than an empty field,
because batch's `for /f` merges empty fields.

| Tool | Windows | Linux | Why |
|---|:-:|:-:|---|
| Git + Git LFS | ✓ | ✓ | `icons/` are in LFS |
| MSVC | ✓ | | builds espeak-ng; links every Rust binary |
| C/C++ compiler + make | | ✓ | the same role as MSVC on Windows |
| CMake | ✓ | ✓ | configures the espeak-ng build (on Windows, Visual Studio's own copy counts) |
| Python ≥ 3.12 | ✓ | ✓ | every provisioning and packaging script |
| Rust (cargo) | ✓ | ✓ | every binary |
| Rust `i686-pc-windows-msvc` target | ✓ | | the x86 SAPI shim, hook and injector |
| `rust-src` component | ✓ | | the corresponding-source archive |
| NSIS 3.12 exactly | ✓ | | the installer; `build_installer.py` rejects any other version |
| cargo-about ≥ 0.9.0 | ✓ | | the dependency licence notices (CI pins 0.9.1) |
| 7-Zip | optional | | only `verify_installer_notices.py` uses it, after the build |
| bun ≥ 1.4.0 | ✓ | ✓ | the browser extension and its tests |

Linux builds only the host and the extension, with no installer and no x86 artifacts, so
it checks less.

## What is not checked

Only whether the tools are installed, not whether this checkout is ready to build. Provisioned
native deps, markers, digests and LFS pointers are checked by the build itself:
`native-deps/fetch-deps.py` and `kokoro-host`'s `build.rs`, plus `packaging/build_installer.py`'s
preflight for a release. So an all-green report does not guarantee a build will get past
provisioning.
