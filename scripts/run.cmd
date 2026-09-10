@echo off
setlocal
cd /d "%~dp0.."
taskkill /im claude-usage-panel.exe /f >nul 2>&1
start "" "target\release\claude-usage-panel.exe"
