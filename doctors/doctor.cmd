@echo off
rem Kokoro Kindle Reader - developer toolchain check (Windows). Reports which build tools are
rem missing, all in one pass. Tools only: whether native-deps\ is provisioned is checked by
rem fetch-deps.py and build_installer.py, not here.
rem
rem   doctors\doctor.cmd
rem
rem WHAT is checked (names, version rules, fixes, order) is in tools.conf, shared with
rem doctor.sh; this file only knows HOW to find each tool - one :detect_<id> per row.
rem Plain batch, so it runs with no Python installed and under any PowerShell execution policy.
setlocal EnableDelayedExpansion

set FAILED=0

rem Colour the markers with ANSI escapes (cmd understands them on Windows 10+). NO_COLOR
rem turns them off, e.g. for output pasted into an issue.
set "G=" & set "R=" & set "Y=" & set "N="
if not defined NO_COLOR for /f %%e in ('echo prompt $E^| cmd') do (
    set "G=%%e[32m" & set "R=%%e[31m" & set "Y=%%e[33m" & set "N=%%e[0m"
)
set "PF86=%ProgramFiles(x86)%"
echo.

for /f "usebackq eol=# tokens=1-5 delims=|" %%a in ("%~dp0tools.conf") do (
    if not "%%e"=="-" (
        set "ID=%%a" & set "NAME=%%b" & set "RULE=%%c" & set "LEVEL=%%d" & set "FIX=%%e"
        call :check
    )
)

if %FAILED% gtr 0 (
    echo.
    echo !R!%FAILED% missing.!N!
    exit /b 1
)
exit /b 0

rem --- reporting -------------------------------------------------------------------------------
rem :check runs :detect_<ID>, which sets FOUND, and optionally VER and DETAIL, then prints one
rem line. Text is echoed through delayed expansion so parentheses and paths are printed, not
rem parsed.
:check
set "FOUND=" & set "VER=" & set "DETAIL="
call :detect_%ID%
if not defined FOUND goto :missing
if "!RULE!"=="-" goto :found
call :vercheck
if not defined VOK goto :tooold
:found
set "INFO=!VER!"
if defined DETAIL if defined VER (set "INFO=!VER!, !DETAIL!") else set "INFO=!DETAIL!"
set "L=!G![+]!N! !NAME!"
if defined INFO set "L=!L! (!INFO!)"
echo(!L!
exit /b 0

:tooold
set "L=!R![x]!N! !NAME! !VER!, need !RULE! - !FIX!"
echo(!L!
set /a FAILED+=1
exit /b 0

:missing
if "!LEVEL!"=="optional" goto :optional
set "L=!R![x]!N! !NAME! - !FIX!"
echo(!L!
set /a FAILED+=1
exit /b 0

:optional
set "L=!Y![^!]!N! !NAME! - !FIX!"
echo(!L!
exit /b 0

rem :vercheck - is VER within RULE (>=X or =X)? Sets VOK.
:vercheck
set "VOK="
if "!RULE:~0,2!"==">=" (set "OP=ge" & set "WANT=!RULE:~2!") else (set "OP=eq" & set "WANT=!RULE:~1!")
call :vnum "!VER!" HAVE
call :vnum "!WANT!" NEED
if "!OP!"=="ge" (if !HAVE! geq !NEED! set "VOK=1") else if !HAVE! equ !NEED! set "VOK=1"
exit /b 0

rem :vnum STRING VAR - the first three dotted parts as one comparable number. Missing or
rem non-numeric parts count as 0 (set /a reads an unset name as 0).
:vnum
set "P1=" & set "P2=" & set "P3="
for /f "tokens=1-3 delims=.-+ " %%x in ("%~1") do (set "P1=%%x" & set "P2=%%y" & set "P3=%%z")
set /a "%~2=P1*1000000+P2*1000+P3"
exit /b 0

rem --- detectors: one per tools.conf id that Windows uses --------------------------------------
:detect_git
for /f "tokens=3" %%v in ('git --version 2^>nul') do (set "FOUND=1" & set "VER=%%v")
exit /b 0

:detect_git_lfs
for /f "tokens=1" %%v in ('git lfs version 2^>nul') do (set "FOUND=1" & set "VER=%%v")
if defined VER set "VER=!VER:git-lfs/=!"
exit /b 0

rem MSVC is found the same way build-espeak.py finds it: vswhere, then vcvarsall.bat.
:detect_msvc
call :find_vs
if defined VS if exist "!VS!\VC\Auxiliary\Build\vcvarsall.bat" (set "FOUND=1" & set "DETAIL=!VS!")
exit /b 0

rem CMake on PATH, or Visual Studio's own copy (build-espeak.py runs cmake after vcvarsall).
:detect_cmake
for /f "tokens=3" %%v in ('cmake --version 2^>nul') do if not defined VER set "VER=%%v"
if defined VER (set "FOUND=1" & exit /b 0)
call :find_vs
if not defined VS exit /b 0
set "VSCMAKE=!VS!\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin\cmake.exe"
if exist "!VSCMAKE!" for /f "tokens=3" %%v in ('call "!VSCMAKE!" --version 2^>nul') do if not defined VER set "VER=%%v"
if defined VER (set "FOUND=1" & set "DETAIL=Visual Studio's")
exit /b 0

rem Runs each candidate rather than looking for the file: python/py/python3 are often 0-byte
rem App Execution Aliases that work fine.
:detect_python
for %%p in (python py python3) do if not defined FOUND (
    for /f "tokens=1,2" %%a in ('%%p --version 2^>nul') do if "%%a"=="Python" (
        set "FOUND=1" & set "VER=%%b" & set "DETAIL=via %%p"
    )
)
exit /b 0

:detect_rust
for /f "tokens=2" %%v in ('cargo --version 2^>nul') do (set "FOUND=1" & set "VER=%%v")
exit /b 0

rem rustup prints LF-only lines, which findstr /x never matches; /b is enough here.
:detect_rust_x86
rustup target list --installed 2>nul | findstr /b /c:"i686-pc-windows-msvc" >nul
if not errorlevel 1 set "FOUND=1"
exit /b 0

:detect_rust_src
set "SYSROOT="
for /f "delims=" %%s in ('rustc --print sysroot 2^>nul') do set "SYSROOT=%%s"
if defined SYSROOT if exist "!SYSROOT!\lib\rustlib\src\rust\library\std\Cargo.toml" set "FOUND=1"
exit /b 0

:detect_nsis
set "MAKENSIS=%PF86%\NSIS\makensis.exe"
if exist "%MAKENSIS%" for /f %%v in ('call "%MAKENSIS%" /VERSION 2^>nul') do (set "FOUND=1" & set "VER=%%v")
if defined VER if "!VER:~0,1!"=="v" set "VER=!VER:~1!"
exit /b 0

:detect_cargo_about
for /f "tokens=2" %%v in ('cargo about --version 2^>nul') do (set "FOUND=1" & set "VER=%%v")
exit /b 0

:detect_seven_zip
for %%e in (7z.exe 7za.exe) do if not defined DETAIL if not "%%~$PATH:e"=="" set "DETAIL=%%~$PATH:e"
if not defined DETAIL if exist "%ProgramFiles%\7-Zip\7z.exe" set "DETAIL=%ProgramFiles%\7-Zip\7z.exe"
if not defined DETAIL if exist "%PF86%\7-Zip\7z.exe" set "DETAIL=%PF86%\7-Zip\7z.exe"
if defined DETAIL set "FOUND=1"
exit /b 0

:detect_bun
for /f %%v in ('bun --version 2^>nul') do (set "FOUND=1" & set "VER=%%v")
exit /b 0

rem Sets VS to the latest Visual Studio install, once. 'call' keeps for /f from stripping the
rem quotes around a path with (x86) in it.
:find_vs
if defined VS_SEARCHED exit /b 0
set "VS_SEARCHED=1" & set "VS="
set "VSWHERE=%PF86%\Microsoft Visual Studio\Installer\vswhere.exe"
if exist "%VSWHERE%" for /f "usebackq delims=" %%i in (`call "%VSWHERE%" -latest -products * -property installationPath`) do set "VS=%%i"
exit /b 0
