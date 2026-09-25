@echo off
rem Kokoro Kindle Reader - developer toolchain check (Windows). Reports which build tools are
rem missing, all in one pass. Tools only: whether native-deps\ is provisioned is checked by
rem fetch-deps.py and build_installer.py, not here.
rem
rem   packaging\doctor.cmd [-For] [app|installer|source|extension|all]
rem
rem Plain batch, so it runs with no Python installed and under any PowerShell execution policy.
setlocal EnableDelayedExpansion

set "TIER=all"
if /i "%~1"=="-For" (set "TIER=%~2") else if not "%~1"=="" set "TIER=%~1"

rem Each tier includes the ones it builds on.
if /i "%TIER%"=="app"       set "WANT_app=1"
if /i "%TIER%"=="installer" set "WANT_app=1" & set "WANT_installer=1"
if /i "%TIER%"=="source"    set "WANT_app=1" & set "WANT_installer=1" & set "WANT_source=1"
if /i "%TIER%"=="extension" set "WANT_extension=1"
if /i "%TIER%"=="all"       set "WANT_app=1" & set "WANT_installer=1" & set "WANT_source=1" & set "WANT_extension=1"
if not defined WANT_app if not defined WANT_extension (
    echo usage: doctor.cmd [-For] [app^|installer^|source^|extension^|all]
    exit /b 2
)

set FAILED=0

rem Colour the markers with ANSI escapes (cmd understands them on Windows 10+). NO_COLOR
rem turns them off, e.g. for output pasted into an issue.
set "G=" & set "R=" & set "Y=" & set "N="
if not defined NO_COLOR for /f %%e in ('echo prompt $E^| cmd') do (
    set "G=%%e[32m" & set "R=%%e[31m" & set "Y=%%e[33m" & set "N=%%e[0m"
)
set "PF86=%ProgramFiles(x86)%"
echo.

rem --- app ---
rem Runs each candidate rather than looking for the file: python/py/python3 are often 0-byte
rem App Execution Aliases that work fine.
set "PY="
for %%p in (python py python3) do if not defined PY (
    for /f "tokens=1,2" %%a in ('%%p --version 2^>nul') do if "%%a"=="Python" (
        set "V=%%b"
        if "!V:~0,2!"=="3." set "PY=%%b, via %%p"
    )
)
if defined PY (call :ok app "Python" "!PY!") else call :fail app "Python 3" "install from https://www.python.org/downloads/ (tick 'Add to PATH')"

set "CARGO="
for /f "tokens=2" %%v in ('cargo --version 2^>nul') do set "CARGO=%%v"
if defined CARGO (call :ok app "Rust" "cargo !CARGO!") else call :fail app "Rust" "install from https://rustup.rs"

rustup target list --installed 2>nul | findstr /b /c:"i686-pc-windows-msvc" >nul
if not errorlevel 1 (call :ok app "Rust x86 target" "i686-pc-windows-msvc") else call :fail app "Rust x86 target" "rustup target add i686-pc-windows-msvc"

set "GIT=" & set "LFS="
for /f "tokens=3" %%v in ('git --version 2^>nul') do set "GIT=%%v"
for /f "tokens=1" %%v in ('git lfs version 2^>nul') do set "LFS=%%v"
if not defined GIT (
    call :fail app "Git" "install from https://git-scm.com/download/win"
) else if not defined LFS (
    call :fail app "Git LFS" "install Git LFS, then: git lfs install, git lfs pull"
) else call :ok app "Git" "git !GIT!, !LFS!"

rem MSVC: found the same way build-espeak.py finds it (vswhere, then vcvarsall.bat).
rem 'call' keeps for /f from stripping the quotes around a path with (x86) in it.
set "VS="
set "VSWHERE=%PF86%\Microsoft Visual Studio\Installer\vswhere.exe"
if exist "%VSWHERE%" for /f "usebackq delims=" %%i in (`call "%VSWHERE%" -latest -products * -property installationPath`) do set "VS=%%i"
set "MSVC_OK="
if defined VS if exist "!VS!\VC\Auxiliary\Build\vcvarsall.bat" set "MSVC_OK=1"
if defined MSVC_OK (call :ok app "MSVC" "!VS!") else call :fail app "MSVC" "install VS Build Tools with 'Desktop development with C++'"

rem CMake: on PATH, or Visual Studio's own copy (build-espeak.py runs cmake after vcvarsall).
set "CMAKE="
for /f "tokens=3" %%v in ('cmake --version 2^>nul') do if not defined CMAKE set "CMAKE=%%v"
if not defined CMAKE if defined VS (
    set "VSCMAKE=!VS!\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin\cmake.exe"
    if exist "!VSCMAKE!" for /f "tokens=3" %%v in ('call "!VSCMAKE!" --version 2^>nul') do if not defined CMAKE set "CMAKE=%%v, Visual Studio's"
)
if defined CMAKE (call :ok app "CMake" "!CMAKE!") else call :fail app "CMake" "winget install Kitware.CMake"

rem --- installer ---
set "NSIS="
set "MAKENSIS=%PF86%\NSIS\makensis.exe"
if exist "%MAKENSIS%" for /f %%v in ('call "%MAKENSIS%" /VERSION 2^>nul') do set "NSIS=%%v"
if "!NSIS!"=="v3.12" (call :ok installer "NSIS" "v3.12") else call :fail installer "NSIS 3.12" "choco install nsis --version=3.12.0 -y --force"

set "ABOUT="
for /f "tokens=2" %%v in ('cargo about --version 2^>nul') do set "ABOUT=%%v"
set "ABOUT_FIX=cargo install cargo-about --locked --features cli"
if not defined ABOUT (
    call :fail installer "cargo-about" "!ABOUT_FIX!"
) else (
    for /f "tokens=1,2 delims=." %%a in ("!ABOUT!") do set /a "MAJ=%%a, MIN=%%b"
    set "ABOUT_OK="
    if !MAJ! gtr 0 set "ABOUT_OK=1"
    if !MAJ! equ 0 if !MIN! geq 9 set "ABOUT_OK=1"
    if defined ABOUT_OK (call :ok installer "cargo-about" "!ABOUT!") else call :fail installer "cargo-about !ABOUT!, need 0.9.0 or later" "!ABOUT_FIX!"
)

rem Only verify_installer_notices.py uses 7-Zip, after the build - so a warning, not a failure.
set "SEVENZIP="
for %%e in (7z.exe 7za.exe) do if not defined SEVENZIP if not "%%~$PATH:e"=="" set "SEVENZIP=%%~$PATH:e"
if not defined SEVENZIP if exist "%ProgramFiles%\7-Zip\7z.exe" set "SEVENZIP=%ProgramFiles%\7-Zip\7z.exe"
if not defined SEVENZIP if exist "%PF86%\7-Zip\7z.exe" set "SEVENZIP=%PF86%\7-Zip\7z.exe"
if defined SEVENZIP (call :ok installer "7-Zip" "!SEVENZIP!") else call :warn installer "7-Zip" "choco install 7zip -y (only needed to verify a built installer)"

rem --- source ---
set "SYSROOT="
for /f "delims=" %%s in ('rustc --print sysroot 2^>nul') do set "SYSROOT=%%s"
set "RUSTSRC="
if defined SYSROOT if exist "!SYSROOT!\lib\rustlib\src\rust\library\std\Cargo.toml" set "RUSTSRC=1"
if defined RUSTSRC (call :ok source "rust-src" "installed") else call :fail source "rust-src" "rustup component add rust-src"

rem --- extension ---
set "BUN="
for /f %%v in ('bun --version 2^>nul') do set "BUN=%%v"
if defined BUN (call :ok extension "bun" "!BUN!") else call :fail extension "bun" "install from https://bun.sh"

if %FAILED% gtr 0 (
    echo.
    echo !R!%FAILED% missing.!N!
    exit /b 1
)
exit /b 0

rem :ok/:fail/:warn TIER NAME TEXT - print one line if TIER was asked for. The text goes through
rem delayed expansion so parentheses and paths in it are printed, not parsed.
:ok
if not defined WANT_%~1 exit /b 0
set "L=!G![+]!N! %~2 (%~3)"
echo(!L!
exit /b 0

:fail
if not defined WANT_%~1 exit /b 0
set "L=!R![x]!N! %~2 - %~3"
echo(!L!
set /a FAILED+=1
exit /b 0

:warn
if not defined WANT_%~1 exit /b 0
set "L=!Y![^!]!N! %~2 - %~3"
echo(!L!
exit /b 0
