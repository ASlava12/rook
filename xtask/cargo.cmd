@echo off
setlocal
rem Keep artifacts on this checkout's drive and temporary projects outside Git.
set "TEMP=%~d0\RookBuildTemp"
set "TMP=%TEMP%"
for %%I in ("%~dp0..\target") do set "CARGO_TARGET_DIR=%%~fI"
if not exist "%TEMP%\" mkdir "%TEMP%"
if not exist "%TEMP%\" exit /b 1
rem A volume-root folder may grant Modify without the WRITE_OWNER needed by MIC.
rem Grant the current user FullControl here, just as in their normal Temp folder.
powershell.exe -NoProfile -NonInteractive -Command "$ErrorActionPreference='Stop'; $rookScratch=Get-Item -LiteralPath $env:TEMP; if ($rookScratch.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Build scratch must be a physical directory' }; $rookAcl=Get-Acl -LiteralPath $env:TEMP; $rookSid=[Security.Principal.WindowsIdentity]::GetCurrent().User; $rookRule=[Security.AccessControl.FileSystemAccessRule]::new($rookSid,'FullControl','ContainerInherit,ObjectInherit','None','Allow'); $rookAcl.AddAccessRule($rookRule); Set-Acl -LiteralPath $env:TEMP -AclObject $rookAcl"
if errorlevel 1 exit /b 1
pushd "%~dp0.." || exit /b 1
if "%~1"=="" goto default
cargo.exe %*
goto finished
:default
cargo.exe xtask ci
:finished
set "ROOK_CARGO_EXIT=%ERRORLEVEL%"
popd
exit /b %ROOK_CARGO_EXIT%
