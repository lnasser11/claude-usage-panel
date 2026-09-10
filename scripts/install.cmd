@echo off
setlocal
cd /d "%~dp0.."
echo Building release...
cargo build --release || exit /b 1
set "DEST=%LOCALAPPDATA%\Programs\ClaudeUsagePanel"
if not exist "%DEST%" mkdir "%DEST%"
taskkill /im claude-usage-panel.exe /f >nul 2>&1
copy /y "target\release\claude-usage-panel.exe" "%DEST%\" >nul
copy /y "target\release\usage-cli.exe" "%DEST%\" >nul
echo Installed to %DEST%
start "" "%DEST%\claude-usage-panel.exe"
echo Panel started (hidden). Hover the top-center of the laptop display.
echo Settings: %APPDATA%\claude-usage-panel\settings.json
