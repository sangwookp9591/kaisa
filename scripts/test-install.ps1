# Offline Windows contract tests; registry and broadcast are replaced with in-memory fixtures.
param([string]$Installer = (Join-Path $PSScriptRoot 'install.ps1'))
$ErrorActionPreference = 'Stop'
. $Installer
function Assert-Kaisa($Condition, [string]$Reason) { if (-not $Condition) { throw "FAIL: $Reason" } }
$fixture = Join-Path ([IO.Path]::GetTempPath()) ([Guid]::NewGuid().ToString())
$savedEnvironment = @{}
foreach ($name in @('KAISA_VERSION', 'KAISA_INSTALL_DIR', 'KAISA_INSTALL_BASE_URL')) {
    $savedEnvironment[$name] = [Environment]::GetEnvironmentVariable($name)
}
# These seams avoid changing the real user's registry, PATH, or shell profile.
$script:userPath = 'C:\fixture\existing'
$script:broadcasts = 0
function Get-KaisaUserPath { return $script:userPath }
function Set-KaisaUserPath([string]$Value) { $script:userPath = $Value }
function Send-KaisaEnvironmentChange { $script:broadcasts++ }
try {
    $version = '0.0.0-fixture'
    $release = Join-Path $fixture "releases\download\v$version"
    $latest = Join-Path $fixture 'releases\latest\download'
    $null = New-Item -ItemType Directory -Path $release, $latest
    Set-Content -LiteralPath (Join-Path $latest 'VERSION') -Value $version -Encoding ASCII
    $asset = "kaisa-$version-x86_64-pc-windows-msvc.zip"
    $binaryText = 'fixture Windows x86_64 executable'
    Set-Content -LiteralPath (Join-Path $fixture 'kaisa.exe') -Value $binaryText -Encoding ASCII
    Compress-Archive -LiteralPath (Join-Path $fixture 'kaisa.exe') -DestinationPath (Join-Path $release $asset)
    $hash = (Get-FileHash -LiteralPath (Join-Path $release $asset) -Algorithm SHA256).Hash.ToLowerInvariant()
    Set-Content -LiteralPath (Join-Path $release 'SHA256SUMS') -Value "$hash  $asset" -Encoding ASCII
    $releaseUri = New-Object System.UriBuilder
    $releaseUri.Scheme = 'file'
    $releaseUri.Host = ''
    $releaseUri.Path = Join-Path $fixture 'releases'
    $env:KAISA_INSTALL_BASE_URL = $releaseUri.Uri.AbsoluteUri
    foreach ($requested in @("v$version", '')) {
        $env:KAISA_VERSION = $requested
        $env:KAISA_INSTALL_DIR = Join-Path $fixture ([Guid]::NewGuid().ToString() + ' bin')
        $output = @(Install-Kaisa)
        Assert-Kaisa ((Get-Content -LiteralPath (Join-Path $env:KAISA_INSTALL_DIR 'kaisa.exe')) -eq $binaryText) 'a Windows install must choose and extract the x86_64 asset'
        Assert-Kaisa ($output -contains 'Open a new terminal, then run: kaisa') 'success must explain the next step'
        Assert-Kaisa (@($output | Where-Object { $_ -like 'Adding *HKCU*' }).Count -gt 0) 'PATH mutation must be announced'
        Assert-Kaisa ($script:userPath -like "C:\fixture\existing;*$env:KAISA_INSTALL_DIR") 'PATH updates must preserve existing entries'
        $priorBroadcasts = $script:broadcasts
        Update-KaisaUserPath $env:KAISA_INSTALL_DIR
        Assert-Kaisa ($script:broadcasts -eq $priorBroadcasts) 'reinstalling must not duplicate the PATH entry'
        Write-Output "PASS: a_supported_windows_install_chooses_its_asset_and_updates_user_path ($requested)"
    }
    # Reinstall over an existing binary through the destination-filesystem replacement path.
    $null = Install-Kaisa
    Assert-Kaisa ((Get-Content -LiteralPath (Join-Path $env:KAISA_INSTALL_DIR 'kaisa.exe')) -eq $binaryText) 'atomic replacement must preserve the expected binary'
    function Move-KaisaBinary { throw 'fixture destination locked' }
    $locked = $false
    try { Install-Kaisa } catch {
        Assert-Kaisa ($_.Exception.Message -like '*close running kaisa processes*') 'locked updates must explain how to retry'
        $locked = $true
    }
    Assert-Kaisa $locked 'a failed replacement must fail installation'
    Assert-Kaisa ((Get-Content -LiteralPath (Join-Path $env:KAISA_INSTALL_DIR 'kaisa.exe')) -eq $binaryText) 'failed replacement must preserve the existing binary'
    Assert-Kaisa (@(Get-ChildItem -LiteralPath $env:KAISA_INSTALL_DIR -Filter '.kaisa-*.tmp' -Force).Count -eq 0) 'failed replacement must clean staged files'
    Write-Output 'PASS: replacement failure preserves the previous installation and cleans staging'
    # Execute the exact README bootstrap with an octet-stream fixture, without network access.
    function Invoke-WebRequest {
        param([switch]$UseBasicParsing, [Parameter(Position=0)]$Uri, $OutFile)
        Assert-Kaisa (-not [string]::IsNullOrEmpty($OutFile)) 'bootstrap must download bytes to a file'
        [IO.File]::WriteAllBytes($OutFile, [Text.Encoding]::UTF8.GetBytes("Write-Output 'bootstrap-fixture-executed'"))
    }
    $readme = Get-Content -LiteralPath (Join-Path $PSScriptRoot '../README.md') -Raw
    $bootstrap = [regex]::Match($readme, '(?m)^\$installer = .+$').Value
    Assert-Kaisa ($bootstrap.Length -gt 0) 'README bootstrap must be exercised by this test'
    $bootstrapOutput = & ([scriptblock]::Create($bootstrap))
    Assert-Kaisa ($bootstrapOutput -eq 'bootstrap-fixture-executed') 'download-and-run must execute UTF-8 octet-stream bytes'
    Write-Output 'PASS: documented download-and-run executes octet-stream content'
    $env:KAISA_INSTALL_DIR = Join-Path $fixture 'rejected'
    Add-Content -LiteralPath (Join-Path $release $asset) -Value 'tampered archive'
    $rejected = $false
    try { Install-Kaisa } catch {
        Assert-Kaisa ($_.Exception.Message -like '*checksum mismatch*') 'checksum rejection must explain the mismatch'
        $rejected = $true
    }
    Assert-Kaisa $rejected 'a checksum mismatch must fail closed'
    Assert-Kaisa (-not (Test-Path -LiteralPath $env:KAISA_INSTALL_DIR)) 'an unverified download must create no install directory'
    Assert-Kaisa ($script:broadcasts -eq $priorBroadcasts) 'an unverified download must not change the user PATH'
    Write-Output 'PASS: a_checksum_mismatch_fails_without_installing_or_updating_path'
} finally {
    Remove-Item -LiteralPath $fixture -Recurse -Force -ErrorAction SilentlyContinue
    foreach ($name in $savedEnvironment.Keys) { [Environment]::SetEnvironmentVariable($name, $savedEnvironment[$name]) }
}
