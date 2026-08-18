#Requires -Version 5.1
#Requires -RunAsAdministrator

<#
.SYNOPSIS
Verifies the managed EasyDeployMesh WinPE package without changing boot.wim.

.DESCRIPTION
The script reads the boot image index directly from the WIM header at offset
0x78, mounts that image with DISM /ReadOnly, validates the injected EasyDeployMesh
startup chain, and always unmounts it with /Discard. Bootstrap enrollment
tokens are checked for presence but are never printed.

.PARAMETER PackageRoot
The managed pxe-boot directory, its boot directory, or boot.wim itself. When
omitted, the usual EasyDeployMesh application-data locations are checked.

.PARAMETER ExpectedAgentPath
Optional path to the currently installed easydeploymesh-agent.exe. When supplied,
its SHA-256 is also compared with the Agent embedded in boot.wim.

.EXAMPLE
.\verify-winpe-package.ps1 -PackageRoot "$env:APPDATA\com.easydeploymesh.desktop\pxe-boot"
#>

[CmdletBinding()]
param(
    [Parameter(Position = 0)]
    [string]$PackageRoot,

    [string]$ExpectedAgentPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$script:Checks = New-Object 'System.Collections.Generic.List[psobject]'
$script:Facts = New-Object 'System.Collections.Generic.List[string]'

function Add-Check {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Name,

        [Parameter(Mandatory = $true)]
        [bool]$Passed,

        [Parameter(Mandatory = $true)]
        [string]$Detail
    )

    [void]$script:Checks.Add([pscustomobject]@{
            Name   = $Name
            Passed = $Passed
            Detail = $Detail
        })
}

function Add-Fact {
    param([Parameter(Mandatory = $true)][string]$Value)
    [void]$script:Facts.Add($Value)
}

function Get-DefaultPackageRoot {
    $candidates = New-Object 'System.Collections.Generic.List[string]'
    foreach ($base in @($env:APPDATA, $env:LOCALAPPDATA)) {
        if ([string]::IsNullOrWhiteSpace($base)) {
            continue
        }
        foreach ($relative in @(
                'com.easydeploymesh.desktop\pxe-boot',
                'EasyDeployMesh\pxe-boot'
            )) {
            $candidate = Join-Path $base $relative
            if (Test-Path -LiteralPath (Join-Path $candidate 'boot\boot.wim') -PathType Leaf) {
                [void]$candidates.Add((Get-Item -LiteralPath $candidate).FullName)
            }
        }
    }

    $unique = @($candidates | Select-Object -Unique)
    if ($unique.Count -eq 1) {
        return $unique[0]
    }
    if ($unique.Count -gt 1) {
        throw 'More than one managed PXE package was found; pass -PackageRoot explicitly.'
    }
    throw 'The managed PXE package was not found; pass -PackageRoot explicitly.'
}

function Resolve-PackageLocation {
    param([Parameter(Mandatory = $true)][string]$Path)

    if (-not (Test-Path -LiteralPath $Path)) {
        throw 'The requested package path does not exist.'
    }
    $item = Get-Item -LiteralPath $Path
    if (-not $item.PSIsContainer) {
        if (-not $item.Name.Equals('boot.wim', [System.StringComparison]::OrdinalIgnoreCase)) {
            throw 'A package file argument must point to boot.wim.'
        }
        $wim = $item.FullName
    }
    else {
        $wims = @(@(
                (Join-Path $item.FullName 'boot\boot.wim'),
                (Join-Path $item.FullName 'boot.wim')
            ) | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf })
        if ($wims.Count -ne 1) {
            throw 'The package path must contain exactly one managed boot.wim.'
        }
        $wim = (Get-Item -LiteralPath $wims[0]).FullName
    }

    $bootDirectory = Split-Path -Parent $wim
    $root = if ((Split-Path -Leaf $bootDirectory).Equals(
            'boot',
            [System.StringComparison]::OrdinalIgnoreCase
        )) {
        Split-Path -Parent $bootDirectory
    }
    else {
        $bootDirectory
    }

    [pscustomobject]@{
        Root          = $root
        BootDirectory = $bootDirectory
        Wim           = $wim
    }
}

function Get-WimBootHeader {
    param([Parameter(Mandatory = $true)][string]$Path)

    $requiredLength = 0x78 + 4
    $header = New-Object byte[] $requiredLength
    $stream = [System.IO.File]::Open(
        $Path,
        [System.IO.FileMode]::Open,
        [System.IO.FileAccess]::Read,
        [System.IO.FileShare]::Read
    )
    try {
        $offset = 0
        while ($offset -lt $header.Length) {
            $read = $stream.Read($header, $offset, $header.Length - $offset)
            if ($read -eq 0) {
                throw 'boot.wim is too short to contain a complete WIM header.'
            }
            $offset += $read
        }
    }
    finally {
        $stream.Dispose()
    }

    $signature = [System.Text.Encoding]::ASCII.GetString($header, 0, 8)
    if ($signature -ne "MSWIM`0`0`0") {
        throw 'boot.wim does not have the MSWIM signature.'
    }

    $headerSize = [System.BitConverter]::ToUInt32($header, 0x08)
    $partNumber = [System.BitConverter]::ToUInt16($header, 0x28)
    $totalParts = [System.BitConverter]::ToUInt16($header, 0x2a)
    $imageCount = [System.BitConverter]::ToUInt32($header, 0x2c)
    $bootIndex = [System.BitConverter]::ToUInt32($header, 0x78)
    if ($headerSize -lt $requiredLength -or
        $partNumber -ne 1 -or
        $totalParts -ne 1 -or
        $bootIndex -eq 0 -or
        $bootIndex -gt $imageCount) {
        throw 'boot.wim has no valid, single-part boot image index.'
    }

    [pscustomobject]@{
        HeaderSize = $headerSize
        ImageCount = $imageCount
        BootIndex  = $bootIndex
    }
}

function Get-Sha256 {
    param([Parameter(Mandatory = $true)][string]$Path)
    (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Get-BytesSha256 {
    param([Parameter(Mandatory = $true)][byte[]]$Bytes)

    $algorithm = [System.Security.Cryptography.SHA256]::Create()
    try {
        $digest = $algorithm.ComputeHash($Bytes)
        ([System.BitConverter]::ToString($digest)).Replace('-', '').ToLowerInvariant()
    }
    finally {
        $algorithm.Dispose()
    }
}

function Get-SafeServerDisplay {
    param([string]$Value)

    if ([string]::IsNullOrWhiteSpace($Value)) {
        return '[missing]'
    }
    if ($Value -match '(?i)(easydeploymesh_enroll_|enrollmenttoken|authorization|bearer\s)') {
        return '[invalid or unsafe URL]'
    }

    $uri = $null
    if (-not [System.Uri]::TryCreate($Value.Trim(), [System.UriKind]::Absolute, [ref]$uri)) {
        return '[invalid or unsafe URL]'
    }
    if (($uri.Scheme -ne 'http' -and $uri.Scheme -ne 'https') -or
        [string]::IsNullOrWhiteSpace($uri.Host) -or
        -not [string]::IsNullOrEmpty($uri.UserInfo) -or
        -not [string]::IsNullOrEmpty($uri.Query) -or
        -not [string]::IsNullOrEmpty($uri.Fragment)) {
        return '[invalid or unsafe URL]'
    }

    $safe = '{0}://{1}{2}' -f $uri.Scheme, $uri.Authority, $uri.AbsolutePath
    $safe.TrimEnd('/')
}

function Read-BootstrapSummary {
    param([Parameter(Mandatory = $true)][string]$Path)

    try {
        $json = [System.IO.File]::ReadAllText($Path) | ConvertFrom-Json
    }
    catch {
        return [pscustomobject]@{
            Valid        = $false
            Server       = '[unreadable JSON]'
            TokenPresent = $false
        }
    }

    $serverProperty = $json.PSObject.Properties['server']
    $tokenProperty = $json.PSObject.Properties['enrollmentToken']
    $server = if ($null -eq $serverProperty) { '' } else { [string]$serverProperty.Value }
    $tokenPresent = $null -ne $tokenProperty -and
        -not [string]::IsNullOrWhiteSpace([string]$tokenProperty.Value)
    $safeServer = Get-SafeServerDisplay -Value $server

    [pscustomobject]@{
        Valid        = $safeServer -notin @('[missing]', '[invalid or unsafe URL]') -and $tokenPresent
        Server       = $safeServer
        TokenPresent = $tokenPresent
    }
}

function Read-TextSafely {
    param([Parameter(Mandatory = $true)][string]$Path)
    [System.IO.File]::ReadAllText($Path)
}

function Invoke-DismQuietly {
    param(
        [Parameter(Mandatory = $true)][string]$DismPath,
        [Parameter(Mandatory = $true)][string[]]$Arguments
    )

    & $DismPath @Arguments 2>$null | Out-Null
    if ($LASTEXITCODE -ne 0) {
        throw "DISM exited with code $LASTEXITCODE."
    }
}

function Assert-WindowsAdministrator {
    if ($env:OS -ne 'Windows_NT') {
        throw 'This verifier must run on Windows because it requires DISM.'
    }
    $identity = [System.Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = New-Object System.Security.Principal.WindowsPrincipal($identity)
    if (-not $principal.IsInRole([System.Security.Principal.WindowsBuiltInRole]::Administrator)) {
        throw 'Run this verifier from an elevated Administrator PowerShell session.'
    }
}

$mountDirectory = $null
$mountAttempted = $false
$discardWasSuccessful = $false
$dismPath = $null
$location = $null

try {
    Assert-WindowsAdministrator
    $dismPath = Join-Path $env:SystemRoot 'System32\dism.exe'
    if (-not (Test-Path -LiteralPath $dismPath -PathType Leaf)) {
        throw 'dism.exe was not found under the Windows system directory.'
    }

    if ([string]::IsNullOrWhiteSpace($PackageRoot)) {
        $PackageRoot = Get-DefaultPackageRoot
    }
    $location = Resolve-PackageLocation -Path $PackageRoot
    $wimHeader = Get-WimBootHeader -Path $location.Wim
    Add-Fact "Package root: $($location.Root)"
    Add-Fact "boot.wim: $($location.Wim)"
    Add-Fact "WIM images: $($wimHeader.ImageCount); header boot index (0x78): $($wimHeader.BootIndex)"
    Add-Check -Name 'WIM signature and header' -Passed $true `
        -Detail "Valid single-part MSWIM header ($($wimHeader.HeaderSize) bytes)."
    Add-Check -Name 'WIM image count' -Passed ($wimHeader.ImageCount -gt 0) `
        -Detail "Header declares $($wimHeader.ImageCount) image(s)."
    Add-Check -Name 'WIM header boot index' -Passed $true `
        -Detail "Offset 0x78 selects image $($wimHeader.BootIndex)."

    $mountDirectory = Join-Path ([System.IO.Path]::GetTempPath()) (
        'EasyDeployMesh-WinPE-Verify-{0}' -f [System.Guid]::NewGuid().ToString('N')
    )
    [void](New-Item -ItemType Directory -Path $mountDirectory)
    $mountArguments = @(
        '/English',
        '/Mount-Image',
        "/ImageFile:$($location.Wim)",
        "/Index:$($wimHeader.BootIndex)",
        "/MountDir:$mountDirectory",
        '/ReadOnly'
    )
    $mountAttempted = $true
    Invoke-DismQuietly -DismPath $dismPath -Arguments $mountArguments
    Add-Check -Name 'DISM read-only mount' -Passed $true -Detail "Mounted image index $($wimHeader.BootIndex) with /ReadOnly."

    $easyDeployMeshDirectory = Join-Path $mountDirectory 'EasyDeployMesh'
    $system32Directory = Join-Path $mountDirectory 'Windows\System32'
    $agentPath = Join-Path $easyDeployMeshDirectory 'easydeploymesh-agent.exe'
    $shellPath = Join-Path $easyDeployMeshDirectory 'easydeploymesh-shell.exe'
    $runtimeCollectorPath = Join-Path $easyDeployMeshDirectory 'collect-winpe-runtime.cmd'
    $bootstrapPath = Join-Path $easyDeployMeshDirectory 'easydeploymesh-bootstrap.json'
    $hookMarkerPath = Join-Path $easyDeployMeshDirectory 'shell-hook.enabled'
    $originalShellPath = Join-Path $easyDeployMeshDirectory 'easydeploymesh-original-shell.cmd'
    $startnetPath = Join-Path $system32Directory 'startnet.cmd'
    $originalStartnetPath = Join-Path $system32Directory 'startnet.easydeploymesh-original.cmd'
    $winpeshlPath = Join-Path $system32Directory 'winpeshl.ini'

    foreach ($required in @(
            @{ Name = 'Embedded Agent'; Path = $agentPath },
            @{ Name = 'Embedded shell launcher'; Path = $shellPath },
            @{ Name = 'Embedded runtime collector'; Path = $runtimeCollectorPath },
            @{ Name = 'Embedded bootstrap'; Path = $bootstrapPath },
            @{ Name = 'Shell-hook marker'; Path = $hookMarkerPath },
            @{ Name = 'startnet.cmd'; Path = $startnetPath },
            @{ Name = 'Original startnet'; Path = $originalStartnetPath },
            @{ Name = 'winpeshl.ini'; Path = $winpeshlPath }
        )) {
        Add-Check -Name $required.Name `
            -Passed (Test-Path -LiteralPath $required.Path -PathType Leaf) `
            -Detail $(if (Test-Path -LiteralPath $required.Path -PathType Leaf) {
                    'Present in the boot image.'
                }
                else {
                    'Missing from the boot image.'
                })
    }

    $agentExists = Test-Path -LiteralPath $agentPath -PathType Leaf
    $shellExists = Test-Path -LiteralPath $shellPath -PathType Leaf
    $outerMarkerPath = Join-Path $location.BootDirectory 'easydeploymesh-agent.sha256'
    $markerValid = $false
    $expectedDigest = $null
    if (Test-Path -LiteralPath $outerMarkerPath -PathType Leaf) {
        $markerText = [System.IO.File]::ReadAllText($outerMarkerPath).Trim()
        $markerValid = $markerText -match '\A[0-9a-fA-F]{64}\z'
        if ($markerValid) {
            $expectedDigest = $markerText.ToLowerInvariant()
        }
    }
    Add-Check -Name 'Managed Agent SHA-256 marker' -Passed $markerValid `
        -Detail $(if ($markerValid) { 'Present and syntactically valid.' } else { 'Missing or invalid.' })

    $outerRuntimeMarkerPath = Join-Path $location.BootDirectory 'easydeploymesh-runtime.sha256'
    $runtimeMarkerValid = $false
    $runtimeMarkerDigest = $null
    if (Test-Path -LiteralPath $outerRuntimeMarkerPath -PathType Leaf) {
        $runtimeMarkerText = [System.IO.File]::ReadAllText($outerRuntimeMarkerPath).Trim()
        $runtimeMarkerValid = $runtimeMarkerText -match '\A[0-9a-fA-F]{64}\z'
        if ($runtimeMarkerValid) {
            $runtimeMarkerDigest = $runtimeMarkerText.ToLowerInvariant()
        }
    }
    Add-Check -Name 'Managed runtime SHA-256 marker' -Passed $runtimeMarkerValid `
        -Detail $(if ($runtimeMarkerValid) { 'Present and syntactically valid.' } else { 'Missing or invalid; refresh the managed WinPE package.' })

    if ($agentExists -and $shellExists -and $markerValid) {
        $agentDigest = Get-Sha256 -Path $agentPath
        $shellDigest = Get-Sha256 -Path $shellPath
        Add-Check -Name 'Embedded Agent hash' -Passed ($agentDigest -eq $expectedDigest) `
            -Detail $(if ($agentDigest -eq $expectedDigest) {
                    'Matches boot\easydeploymesh-agent.sha256.'
                }
                else {
                    'Does not match boot\easydeploymesh-agent.sha256.'
                })
        Add-Check -Name 'Shell launcher hash' -Passed ($shellDigest -eq $agentDigest) `
            -Detail $(if ($shellDigest -eq $agentDigest) {
                    'easydeploymesh-shell.exe is byte-identical to easydeploymesh-agent.exe.'
                }
                else {
                    'easydeploymesh-shell.exe differs from easydeploymesh-agent.exe.'
                })
    }
    else {
        Add-Check -Name 'Embedded runtime hash comparison' -Passed $false `
            -Detail 'Agent, shell launcher, or managed SHA-256 marker is unavailable.'
    }

    $collectorExists = Test-Path -LiteralPath $runtimeCollectorPath -PathType Leaf
    $startnetExists = Test-Path -LiteralPath $startnetPath -PathType Leaf
    if ($agentExists -and $collectorExists -and $startnetExists -and $runtimeMarkerValid) {
        $runtimeRevision = 'easydeploymesh-winpe-runtime-layout-v1'
        $runtimeManifest = "revision=$runtimeRevision`n" +
            "agent=$(Get-Sha256 -Path $agentPath)`n" +
            "startnet=$(Get-Sha256 -Path $startnetPath)`n" +
            "collector=$(Get-Sha256 -Path $runtimeCollectorPath)`n"
        $expectedRuntimeDigest = Get-BytesSha256 -Bytes (
            [System.Text.Encoding]::UTF8.GetBytes($runtimeManifest)
        )
        Add-Check -Name 'Embedded runtime layout hash' `
            -Passed ($expectedRuntimeDigest -eq $runtimeMarkerDigest) `
            -Detail $(if ($expectedRuntimeDigest -eq $runtimeMarkerDigest) {
                    'Agent, startnet, and diagnostic collector match the current runtime layout marker.'
                }
                else {
                    'The embedded runtime layout differs from easydeploymesh-runtime.sha256.'
                })
    }
    else {
        Add-Check -Name 'Embedded runtime layout hash' -Passed $false `
            -Detail 'Agent, startnet, diagnostic collector, or managed runtime marker is unavailable.'
    }

    if (-not [string]::IsNullOrWhiteSpace($ExpectedAgentPath)) {
        if ((Test-Path -LiteralPath $ExpectedAgentPath -PathType Leaf) -and $agentExists) {
            $installedDigest = Get-Sha256 -Path (Get-Item -LiteralPath $ExpectedAgentPath).FullName
            $embeddedDigest = Get-Sha256 -Path $agentPath
            Add-Check -Name 'Installed versus embedded Agent hash' `
                -Passed ($installedDigest -eq $embeddedDigest) `
                -Detail $(if ($installedDigest -eq $embeddedDigest) {
                        'The supplied Agent is byte-identical to the embedded Agent.'
                    }
                    else {
                        'The supplied Agent differs from the embedded Agent.'
                    })
        }
        else {
            Add-Check -Name 'Installed versus embedded Agent hash' -Passed $false `
                -Detail 'The supplied Agent path or embedded Agent is unavailable.'
        }
    }

    if (Test-Path -LiteralPath $bootstrapPath -PathType Leaf) {
        $bootstrap = Read-BootstrapSummary -Path $bootstrapPath
        $tokenState = if ($bootstrap.TokenPresent) { '[present, redacted]' } else { '[missing]' }
        Add-Check -Name 'Embedded bootstrap configuration' -Passed $bootstrap.Valid `
            -Detail "server=$($bootstrap.Server); enrollmentToken=$tokenState"
    }

    $outerBootstrapPath = Join-Path $location.BootDirectory 'easydeploymesh-bootstrap.json'
    if (Test-Path -LiteralPath $outerBootstrapPath -PathType Leaf) {
        $outerBootstrap = Read-BootstrapSummary -Path $outerBootstrapPath
        $outerTokenState = if ($outerBootstrap.TokenPresent) { '[present, redacted]' } else { '[missing]' }
        Add-Check -Name 'PXE bootstrap configuration' -Passed $outerBootstrap.Valid `
            -Detail "server=$($outerBootstrap.Server); enrollmentToken=$outerTokenState"
        if (Test-Path -LiteralPath $bootstrapPath -PathType Leaf) {
            Add-Check -Name 'PXE versus embedded bootstrap hash' `
                -Passed ((Get-Sha256 -Path $outerBootstrapPath) -eq (Get-Sha256 -Path $bootstrapPath)) `
                -Detail $(if ((Get-Sha256 -Path $outerBootstrapPath) -eq (Get-Sha256 -Path $bootstrapPath)) {
                        'The public PXE bootstrap and embedded bootstrap are byte-identical.'
                    }
                    else {
                        'The public PXE bootstrap and embedded bootstrap differ.'
                    })
        }
    }
    else {
        Add-Check -Name 'PXE bootstrap configuration' -Passed $false `
            -Detail 'boot\easydeploymesh-bootstrap.json is missing; start the control service before testing PXE.'
    }

    $startnetText = if (Test-Path -LiteralPath $startnetPath -PathType Leaf) {
        Read-TextSafely -Path $startnetPath
    }
    else { '' }
    $startnetValid = $startnetText -match '(?im)^\s*wpeinit\s*$' -and
        $startnetText -match '(?i)X:\\EasyDeployMesh\\easydeploymesh-agent\.exe\s+--bootstrap\s+X:\\EasyDeployMesh\\easydeploymesh-bootstrap\.json' -and
        $startnetText -match '(?i)startnet\.easydeploymesh-original\.cmd'
    Add-Check -Name 'startnet startup chain' -Passed $startnetValid `
        -Detail $(if ($startnetValid) {
                'Initializes networking, references the redacted bootstrap path, and chains the original startnet.'
            }
            else {
                'The expected EasyDeployMesh startnet chain is incomplete.'
            })

    $winpeshlText = if (Test-Path -LiteralPath $winpeshlPath -PathType Leaf) {
        Read-TextSafely -Path $winpeshlPath
    }
    else { '' }
    $winpeshlValid = $winpeshlText -match '(?i)X:\\EasyDeployMesh\\easydeploymesh-shell\.exe'
    Add-Check -Name 'winpeshl shell hook' -Passed $winpeshlValid `
        -Detail $(if ($winpeshlValid) {
                'winpeshl.ini launches the EasyDeployMesh shell hook.'
            }
            else {
                'winpeshl.ini does not launch the EasyDeployMesh shell hook.'
            })

    $hookMarkerValid = $false
    if (Test-Path -LiteralPath $hookMarkerPath -PathType Leaf) {
        $hookMarkerValid = (Read-TextSafely -Path $hookMarkerPath).Trim() -eq 'enabled'
    }
    Add-Check -Name 'Shell-hook marker contents' -Passed $hookMarkerValid `
        -Detail $(if ($hookMarkerValid) { 'Marker is enabled.' } else { 'Marker is missing or invalid.' })

    try {
        $toolCandidates = @(Get-ChildItem -LiteralPath $mountDirectory -Recurse -Force `
                -ErrorAction SilentlyContinue | Where-Object {
                -not $_.PSIsContainer -and
                $_.Name -match '(?i)^(ghost(?:32|64).*|eix.*|easyimagex.*)\.exe$' -and
                $_.FullName -notmatch '(?i)(easydeploymesh_enroll_|enrollmenttoken)'
            })
        if ($toolCandidates.Count -eq 0) {
            Add-Fact 'WIM Ghost/EIX candidates: none found (informational; Agent checks are unaffected).'
        }
        else {
            foreach ($candidate in $toolCandidates) {
                $relative = $candidate.FullName.Substring($mountDirectory.Length).TrimStart([char]92)
                Add-Fact "WIM tool candidate (not executed): \$relative"
            }
        }
    }
    catch {
        Add-Fact 'WIM Ghost/EIX candidates: inventory unavailable (informational; Agent checks are unaffected).'
    }

    $vendorShellPattern = '(?i)(pecmd(?:\.exe)?|EasyU(?:\.ini)?)'
    $originalShellValid = $false
    $originalShellDetail = 'No preserved EasyU/PECMD launch command was found.'
    if (Test-Path -LiteralPath $originalShellPath -PathType Leaf) {
        $originalShellValid = (Read-TextSafely -Path $originalShellPath) -match $vendorShellPattern
        if ($originalShellValid) {
            $originalShellDetail = 'The original EasyU/PECMD shell is preserved in easydeploymesh-original-shell.cmd.'
        }
    }
    elseif ($winpeshlText) {
        $vendorLine = @($winpeshlText -split "`r?`n" | Where-Object {
                $_ -notmatch '(?i)easydeploymesh-shell\.exe' -and $_ -match $vendorShellPattern
            })
        $originalShellValid = $vendorLine.Count -gt 0
        if ($originalShellValid) {
            $originalShellDetail = 'The original EasyU/PECMD shell remains in the [LaunchApps] chain.'
        }
    }
    Add-Check -Name 'Original EasyU shell preservation' -Passed $originalShellValid `
        -Detail $originalShellDetail
}
catch {
    $safeMessage = if ($_.Exception.Message -match '^DISM exited with code [0-9]+\.$') {
        $_.Exception.Message
    }
    elseif ($_.Exception.Message -match '^(This verifier|Run this verifier|The managed|More than one|The requested|The package|A package|boot\.wim|dism\.exe)') {
        $_.Exception.Message
    }
    else {
        'Verification stopped because a package or filesystem operation failed.'
    }
    Add-Check -Name 'Verification completed' -Passed $false -Detail $safeMessage
}
finally {
    if ($mountAttempted -and $null -ne $dismPath -and $null -ne $mountDirectory) {
        & $dismPath /English /Unmount-Image "/MountDir:$mountDirectory" /Discard 2>$null | Out-Null
        $discardWasSuccessful = $LASTEXITCODE -eq 0
        Add-Check -Name 'DISM discard cleanup' -Passed $discardWasSuccessful `
            -Detail $(if ($discardWasSuccessful) {
                    'The read-only mount was unmounted with /Discard.'
                }
                else {
                    'DISM could not confirm /Discard; inspect mounted WIM state before removing the mount directory.'
                })
    }
    if ($null -ne $mountDirectory -and
        (Test-Path -LiteralPath $mountDirectory) -and
        (-not $mountAttempted -or $discardWasSuccessful)) {
        Remove-Item -LiteralPath $mountDirectory -Recurse -Force -ErrorAction SilentlyContinue
    }
}

Write-Output 'EasyDeployMesh WinPE package verification'
foreach ($fact in $script:Facts) {
    Write-Output "  $fact"
}
Write-Output ''
foreach ($check in $script:Checks) {
    $label = if ($check.Passed) { 'PASS' } else { 'FAIL' }
    Write-Output ('[{0}] {1}: {2}' -f $label, $check.Name, $check.Detail)
}

$failed = @($script:Checks | Where-Object { -not $_.Passed }).Count
$passed = $script:Checks.Count - $failed
Write-Output ''
Write-Output "Summary: $passed passed, $failed failed. Enrollment tokens were not printed."
if ($failed -gt 0) {
    exit 1
}
exit 0
