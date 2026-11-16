@echo off
setlocal
cd /d "%~dp0"
python -c "import tkinter, PIL, cv2, numpy" >nul 2>&1
if not errorlevel 1 (
    python -m yolo_labeler %*
    if errorlevel 1 goto :failed
    exit /b 0
)
if not exist ".venv\Scripts\python.exe" (
    python -m venv .venv
    if errorlevel 1 goto :failed
)
".venv\Scripts\python.exe" -c "import tkinter, PIL, cv2, numpy" >nul 2>&1
if errorlevel 1 (
    ".venv\Scripts\python.exe" -m pip install -r requirements.txt
    if errorlevel 1 goto :failed
)
".venv\Scripts\python.exe" -m yolo_labeler %*
if errorlevel 1 goto :failed
exit /b 0
:failed
echo.
echo Startup failed. Check the error above.
pause
exit /b 1
