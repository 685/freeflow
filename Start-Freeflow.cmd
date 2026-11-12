@echo off
cd /d "%~dp0"
where cargo.exe >nul 2>&1
if errorlevel 1 (
  echo Freeflow requires Rust. Install it from https://rustup.rs/
  pause
  exit /b 1
)
cargo run --quiet --locked -p freeflow-build -- %*
if errorlevel 1 pause
