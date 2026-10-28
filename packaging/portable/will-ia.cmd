@echo off
rem Lancador portatil do will-ia (Windows): sobe o SERVIDOR e depois abre o APP.
rem Layout: bin\will-ia-windows-x86_64.exe (servidor) e bin\will-ia-app-windows-x86_64.exe (app) ao lado.
setlocal
set ROOT=%~dp0
set ARCH=x86_64
if "%PROCESSOR_ARCHITECTURE%"=="ARM64" set ARCH=aarch64
set SERVER=%ROOT%bin\will-ia-windows-%ARCH%.exe
set APP=%ROOT%bin\will-ia-app-windows-%ARCH%.exe
if not exist "%SERVER%" (
  echo Binario do servidor nao encontrado: %SERVER%
  pause
  exit /b 1
)
if not exist "%ROOT%portable" echo will-ia portatil > "%ROOT%portable"
set WILL_IA_PORTABLE=1
rem 1) servidor (se ja ha um rodando sobre este pendrive, a segunda execucao so abre o app e sai)
start "" "%SERVER%"
rem 2) app: espera a API local (data\api-port) e conecta em "Esta maquina"
set /a i=0
:wait
if exist "%ROOT%data\api-port" goto app
if %i% geq 40 goto app
set /a i+=1
timeout /t 1 /nobreak >nul
goto wait
:app
if exist "%APP%" (
  start "" "%APP%" %*
) else (
  echo App nao encontrado: %APP%. O servidor ficou no ar; rode "%SERVER%" info para ver URL e token.
  pause
)
endlocal
