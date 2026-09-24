@echo off
rem Runs doctor.ps1 on a machine whose PowerShell execution policy is still the Windows
rem default (Restricted), which refuses every .ps1 - including the one that exists to be run
rem on a machine with nothing set up yet. -ExecutionPolicy Bypass applies to this one process
rem only; no policy is changed. Arguments pass through: doctor -For app
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0doctor.ps1" %*
exit /b %ERRORLEVEL%
