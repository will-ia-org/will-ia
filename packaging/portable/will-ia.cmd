@echo off
rem Lançador portátil do will-ia (Windows). Layout: bin\will-ia-windows-x86_64.exe ao lado.
setlocal
set ROOT=%~dp0
set BIN=%ROOT%bin\will-ia-windows-x86_64.exe
if "%PROCESSOR_ARCHITECTURE%"=="ARM64" set BIN=%ROOT%bin\will-ia-windows-aarch64.exe
if not exist "%BIN%" (
  echo Binario nao encontrado: %BIN%
  pause
  exit /b 1
)
if not exist "%ROOT%portable" echo will-ia portatil > "%ROOT%portable"
set WILL_IA_PORTABLE=1
start "" "%BIN%" %*
endlocal
