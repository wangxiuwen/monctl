@echo off
rem Run monctl inside the interactive console session on a remote/headless box.
rem
rem A SYSTEM/service context (winforge exec, SSH, session 0) cannot see displays
rem or hold user-session power state on Windows, so hop into the logged-on
rem user's session via a one-shot scheduled task with an interactive token.
rem
rem Usage:  windows-session-run.cmd [monctl args...]     e.g.  ... list | ... awake 8h
rem
setlocal
rem edit these three lines for your machine:
set "MONCTL_DIR=C:\WinForge\monctl"
set "RUNAS=the-console-user"

set "MONCTL=%MONCTL_DIR%\monctl.exe"
set "OUTDIR=%MONCTL_DIR%"
if "%~1"=="" (set "ARGS=help") else (set "ARGS=%*")
set "T=monctl-run-%RANDOM%%RANDOM%"
set "OUT=%OUTDIR%\out-%RANDOM%%RANDOM%.txt"
schtasks /Create /TN %T% /TR "cmd /c %MONCTL% %ARGS% > "%OUT%" 2>&1" /SC ONCE /ST 23:59 /IT /RU %RUNAS% /F >nul 2>&1
schtasks /Run /TN %T% >nul 2>&1
ping -n 6 127.0.0.1 >nul
type "%OUT%"
schtasks /Delete /TN %T% /F >nul 2>&1
del "%OUT%" >nul 2>&1
endlocal
