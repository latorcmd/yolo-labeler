@echo off
setlocal
cd /d "%~dp0"
if exist "dist\yolo-labeler\yolo-labeler.exe" (
  "dist\yolo-labeler\yolo-labeler.exe" %*
  exit /b %errorlevel%
)
if exist "target\release\yolo-labeler.exe" (
  "target\release\yolo-labeler.exe" %*
  exit /b %errorlevel%
)
echo Build the Rust edition first with build-rust.ps1, or use the distribution ZIP.
pause
exit /b 1
