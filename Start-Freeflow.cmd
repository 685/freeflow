@echo off
cd /d "%~dp0"
where node.exe >nul 2>&1
if errorlevel 1 (
  echo Freeflow requires Node.js 22.15 or newer. Download it from https://nodejs.org/
  pause
  exit /b 1
)
node bin\freeflow.mjs %*
if errorlevel 1 pause
