@echo off
setlocal EnableExtensions DisableDelayedExpansion

rem EasyDeployMesh WinPE runtime collector.
rem This script only reads system state and writes a diagnostic report.
rem It never starts registration, EIX, or an imaging operation. The injected
rem Agent may run only --version or the unauthenticated read-only --health-check;
rem exact Ghost32/64 matches may run only -ver.

set "OUTPUT_ROOT=%~1"
if not defined OUTPUT_ROOT set "OUTPUT_ROOT=%~dp0EasyDeployMesh-diagnostics"
if exist "%OUTPUT_ROOT%" set "OUTPUT_ROOT=%OUTPUT_ROOT%-%RANDOM%"
md "%OUTPUT_ROOT%" >nul 2>&1
if not exist "%OUTPUT_ROOT%\NUL" (
    echo Unable to create diagnostic directory: "%OUTPUT_ROOT%"
    exit /b 1
)

set "REPORT=%OUTPUT_ROOT%\winpe-runtime.txt"
set "AGENT_LOG=%OUTPUT_ROOT%\easydeploymesh-agent.sanitized.log"
set "DISKPART_SCRIPT=%OUTPUT_ROOT%\diskpart-readonly.txt"
set "POWERSHELL_EXE=%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe"
set "WHERE_EXE=%SystemRoot%\System32\where.exe"
set "SORT_EXE=%SystemRoot%\System32\sort.exe"

>"%REPORT%" echo EasyDeployMesh WinPE runtime diagnostics
>>"%REPORT%" echo Collected: %DATE% %TIME%
>>"%REPORT%" echo Computer: %COMPUTERNAME%
>>"%REPORT%" echo System drive: %SystemDrive%
>>"%REPORT%" echo Script location: %~dp0
>>"%REPORT%" echo Enrollment tokens are intentionally omitted and redacted.

call :Section "WinPE and process state"
ver >>"%REPORT%" 2>&1
if exist "%SystemRoot%\System32\wpeutil.exe" (
    "%SystemRoot%\System32\wpeutil.exe" UpdateBootInfo >>"%REPORT%" 2>&1
)
reg query "HKLM\System\CurrentControlSet\Control" /v PEFirmwareType >>"%REPORT%" 2>&1
if exist "%SystemRoot%\System32\tasklist.exe" (
    call :InspectAgentProcess
    tasklist /FI "IMAGENAME eq easydeploymesh-agent.exe" >>"%REPORT%" 2>&1
    tasklist /FI "IMAGENAME eq easydeploymesh-shell.exe" >>"%REPORT%" 2>&1
) else (
    >>"%REPORT%" echo EASYDEPLOYMESH_DIAG_V1^|agent.process^|unknown
    >>"%REPORT%" echo tasklist.exe is unavailable.
)
if exist "X:\EasyDeployMesh\easydeploymesh-agent.exe" (
    >>"%REPORT%" echo EASYDEPLOYMESH_DIAG_V1^|agent.binary^|present
    dir /a:-d /-c "X:\EasyDeployMesh\easydeploymesh-agent.exe" "X:\EasyDeployMesh\easydeploymesh-shell.exe" >>"%REPORT%" 2>&1
    call :ProbeAgentVersion
) else (
    >>"%REPORT%" echo EASYDEPLOYMESH_DIAG_V1^|agent.binary^|missing
    >>"%REPORT%" echo X:\EasyDeployMesh\easydeploymesh-agent.exe is missing.
)

call :Section "Bootstrap configuration (token redacted)"
call :InspectBootstraps
if exist "X:\EasyDeployMesh\easydeploymesh-agent.exe" (
    if exist "X:\EasyDeployMesh\easydeploymesh-bootstrap.json" (
        call :ProbeControlHealth
    ) else (
        >>"%REPORT%" echo EASYDEPLOYMESH_DIAG_V1^|control.health^|not_run^|reason=missing_runtime_input
    )
) else (
    >>"%REPORT%" echo EASYDEPLOYMESH_DIAG_V1^|control.health^|not_run^|reason=missing_runtime_input
)

call :Section "Agent log"
if exist "X:\EasyDeployMesh\easydeploymesh-agent.log" (
    >>"%REPORT%" echo Source: X:\EasyDeployMesh\easydeploymesh-agent.log
    findstr /V /I /C:"enrollmentToken" /C:"enrollment-token" /C:"easydeploymesh_enroll_" /C:"Authorization:" /C:"Bearer " "X:\EasyDeployMesh\easydeploymesh-agent.log" >"%AGENT_LOG%" 2>nul
    if not exist "%AGENT_LOG%" type nul >"%AGENT_LOG%"
    for %%Z in ("%AGENT_LOG%") do if %%~zZ EQU 0 (
        >>"%REPORT%" echo EASYDEPLOYMESH_DIAG_V1^|agent.log^|empty
    ) else (
        >>"%REPORT%" echo EASYDEPLOYMESH_DIAG_V1^|agent.log^|present
    )
    >>"%REPORT%" echo Sanitized copy: easydeploymesh-agent.sanitized.log
    >>"%REPORT%" echo Lines containing enrollment or authorization material were omitted.
) else (
    >>"%REPORT%" echo EASYDEPLOYMESH_DIAG_V1^|agent.log^|missing
    >>"%REPORT%" echo X:\EasyDeployMesh\easydeploymesh-agent.log is missing.
)

call :Section "Startup chain (sensitive lines omitted)"
call :AppendSanitizedFile "X:\Windows\System32\startnet.cmd"
call :AppendSanitizedFile "X:\Windows\System32\startnet.easydeploymesh-original.cmd"
call :AppendSanitizedFile "X:\Windows\System32\winpeshl.ini"
call :AppendSanitizedFile "X:\EasyDeployMesh\easydeploymesh-original-shell.cmd"

call :Section "Network interfaces and DNS"
ipconfig /all >>"%REPORT%" 2>&1
if exist "%SystemRoot%\System32\netsh.exe" (
    netsh interface ipv4 show interfaces >>"%REPORT%" 2>&1
    netsh interface ipv4 show config >>"%REPORT%" 2>&1
)

call :Section "Routes and neighbors"
route print >>"%REPORT%" 2>&1
arp -a >>"%REPORT%" 2>&1

call :Section "Physical disks and volumes (read-only queries)"
>"%DISKPART_SCRIPT%" echo list disk
>>"%DISKPART_SCRIPT%" echo list volume
diskpart /s "%DISKPART_SCRIPT%" >>"%REPORT%" 2>&1
del /q "%DISKPART_SCRIPT%" >nul 2>&1
if exist "%SystemRoot%\System32\mountvol.exe" mountvol >>"%REPORT%" 2>&1
if exist "%SystemRoot%\System32\fsutil.exe" fsutil fsinfo drives >>"%REPORT%" 2>&1
if exist "%SystemRoot%\System32\wbem\wmic.exe" (
    "%SystemRoot%\System32\wbem\wmic.exe" diskdrive get Index,Model,SerialNumber,Size,Status /format:list >>"%REPORT%" 2>&1
    "%SystemRoot%\System32\wbem\wmic.exe" logicaldisk get DeviceID,DriveType,FileSystem,FreeSpace,Size,VolumeName /format:list >>"%REPORT%" 2>&1
)

>>"%REPORT%" echo EASYDEPLOYMESH_DIAG_V1^|collector.complete^|ok

echo.
echo Diagnostics collected in:
echo   %OUTPUT_ROOT%
echo Enrollment tokens were not written to the report.
exit /b 0

:Section
>>"%REPORT%" echo.
>>"%REPORT%" echo ==== %~1 ====
exit /b 0

:InspectAgentProcess
tasklist /FI "IMAGENAME eq easydeploymesh-agent.exe" 2>nul | find /I "easydeploymesh-agent.exe" >nul 2>&1
if errorlevel 1 (
    >>"%REPORT%" echo EASYDEPLOYMESH_DIAG_V1^|agent.process^|absent
) else (
    >>"%REPORT%" echo EASYDEPLOYMESH_DIAG_V1^|agent.process^|running
)
exit /b 0

:ProbeAgentVersion
"X:\EasyDeployMesh\easydeploymesh-agent.exe" --version >>"%REPORT%" 2>&1
set "EASYDEPLOYMESH_AGENT_VERSION_EXIT=%ERRORLEVEL%"
if "%EASYDEPLOYMESH_AGENT_VERSION_EXIT%"=="0" (
    >>"%REPORT%" echo EASYDEPLOYMESH_DIAG_V1^|agent.version_probe^|ok^|exit=0
) else (
    >>"%REPORT%" echo EASYDEPLOYMESH_DIAG_V1^|agent.version_probe^|failed^|exit=%EASYDEPLOYMESH_AGENT_VERSION_EXIT%
)
set "EASYDEPLOYMESH_AGENT_VERSION_EXIT="
exit /b 0

:ProbeControlHealth
"X:\EasyDeployMesh\easydeploymesh-agent.exe" --bootstrap "X:\EasyDeployMesh\easydeploymesh-bootstrap.json" --health-check >>"%REPORT%" 2>&1
set "EASYDEPLOYMESH_CONTROL_HEALTH_EXIT=%ERRORLEVEL%"
>>"%REPORT%" echo EASYDEPLOYMESH_DIAG_V1^|control.health_probe^|exit=%EASYDEPLOYMESH_CONTROL_HEALTH_EXIT%
set "EASYDEPLOYMESH_CONTROL_HEALTH_EXIT="
exit /b 0

:InspectBootstraps
set "EASYDEPLOYMESH_BOOTSTRAP_COUNT=0"
if exist "X:\EasyDeployMesh\easydeploymesh-bootstrap.json" (
    >>"%REPORT%" echo EASYDEPLOYMESH_DIAG_V1^|bootstrap.authoritative^|present
) else (
    >>"%REPORT%" echo EASYDEPLOYMESH_DIAG_V1^|bootstrap.authoritative^|missing
)
for %%B in (
    "X:\EasyDeployMesh\easydeploymesh-bootstrap.json"
    "X:\easydeploymesh-bootstrap.json"
    "X:\Boot\easydeploymesh-bootstrap.json"
    "X:\Sources\easydeploymesh-bootstrap.json"
    "X:\Windows\System32\easydeploymesh-bootstrap.json"
) do if exist "%%~B" set /a EASYDEPLOYMESH_BOOTSTRAP_COUNT+=1 >nul
if "%EASYDEPLOYMESH_BOOTSTRAP_COUNT%"=="0" (
    >>"%REPORT%" echo EASYDEPLOYMESH_DIAG_V1^|bootstrap.discovery^|none
) else if "%EASYDEPLOYMESH_BOOTSTRAP_COUNT%"=="1" (
    >>"%REPORT%" echo EASYDEPLOYMESH_DIAG_V1^|bootstrap.discovery^|single
) else (
    >>"%REPORT%" echo EASYDEPLOYMESH_DIAG_V1^|bootstrap.discovery^|multiple
)
call :InspectBootstrap "X:\EasyDeployMesh\easydeploymesh-bootstrap.json"
call :InspectBootstrap "X:\easydeploymesh-bootstrap.json"
call :InspectBootstrap "X:\Boot\easydeploymesh-bootstrap.json"
call :InspectBootstrap "X:\Sources\easydeploymesh-bootstrap.json"
call :InspectBootstrap "X:\Windows\System32\easydeploymesh-bootstrap.json"
set "EASYDEPLOYMESH_BOOTSTRAP_COUNT="
exit /b 0

:InspectBootstrap
if not exist "%~1" exit /b 0
>>"%REPORT%" echo File: %~1
if exist "%POWERSHELL_EXE%" (
    set "EASYDEPLOYMESH_BOOTSTRAP_PATH=%~1"
    "%POWERSHELL_EXE%" -NoLogo -NoProfile -NonInteractive -Command "$ErrorActionPreference='Stop';try{$j=[IO.File]::ReadAllText($env:EASYDEPLOYMESH_BOOTSTRAP_PATH)|ConvertFrom-Json;$s=[string]$j.server;$u=$null;$safe=[Uri]::TryCreate($s,[UriKind]::Absolute,[ref]$u) -and @('http','https') -contains $u.Scheme -and $u.Host -and -not $u.UserInfo -and -not $u.Query -and -not $u.Fragment -and $s -notmatch '(?i)(easydeploymesh_enroll_|enrollmenttoken|authorization|bearer\s)';if($safe){[Console]::WriteLine('Server: '+$u.Scheme+'://'+$u.Authority+$u.AbsolutePath.TrimEnd('/'))}else{[Console]::WriteLine('Server: [invalid or unsafe URL]')};if($j.PSObject.Properties['enrollmentToken'] -and -not [string]::IsNullOrWhiteSpace([string]$j.enrollmentToken)){[Console]::WriteLine('Enrollment token: [PRESENT, REDACTED]')}else{[Console]::WriteLine('Enrollment token: [MISSING]')}}catch{[Console]::WriteLine('Server: [unreadable JSON]');[Console]::WriteLine('Enrollment token: [UNKNOWN, REDACTED]')}" >>"%REPORT%" 2>nul
    set "EASYDEPLOYMESH_BOOTSTRAP_PATH="
) else (
    call :InspectBootstrapWithoutPowerShell "%~1"
)
exit /b 0

:InspectBootstrapWithoutPowerShell
setlocal
>>"%REPORT%" echo Server: [unavailable without a safe JSON parser]
findstr /I /C:"enrollmentToken" "%~1" >nul 2>&1
if errorlevel 1 (
    >>"%REPORT%" echo Enrollment token: [MISSING]
) else (
    >>"%REPORT%" echo Enrollment token: [PRESENT, REDACTED]
)
endlocal
exit /b 0

:AppendSanitizedFile
if not exist "%~1" (
    >>"%REPORT%" echo Missing: %~1
    exit /b 0
)
>>"%REPORT%" echo -- %~1 --
findstr /V /I /C:"enrollmentToken" /C:"enrollment-token" /C:"easydeploymesh_enroll_" /C:"Authorization:" /C:"Bearer " "%~1" >>"%REPORT%" 2>nul
exit /b 0
