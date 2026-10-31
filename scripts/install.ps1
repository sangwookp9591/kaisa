# PowerShell 5.1+; dot-source to load functions without installing.
$ErrorActionPreference = 'Stop'
function Get-KaisaUserPath {
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $false)
    try { return [string]$key.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames) }
    finally { $key.Dispose() }
}
function Set-KaisaUserPath([string]$Value) {
    $key = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey('Environment')
    try { $key.SetValue('Path', $Value, [Microsoft.Win32.RegistryValueKind]::ExpandString) }
    finally { $key.Dispose() }
}
function Send-KaisaEnvironmentChange {
    if (-not ('Kaisa.EnvironmentNotification' -as [type])) {
        Add-Type @'
using System;
using System.Runtime.InteropServices;
namespace Kaisa {
    public static class EnvironmentNotification {
        [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        public static extern IntPtr SendMessageTimeout(IntPtr window, uint message,
            UIntPtr wParam, string lParam, uint flags, uint timeout, out UIntPtr result);
    }
}
'@
    }
    $result = [UIntPtr]::Zero
    $sent = [Kaisa.EnvironmentNotification]::SendMessageTimeout([IntPtr]0xffff, 0x1a,
        [UIntPtr]::Zero, 'Environment', 2, 5000, [ref]$result)
    if ($sent -eq [IntPtr]::Zero) { Write-Warning 'PATH saved; environment-change broadcast timed out. Sign out and back in if a new terminal cannot find kaisa.' }
}
function Update-KaisaUserPath([string]$Directory) {
    $current = Get-KaisaUserPath
    $entries = @($current -split ';' | ForEach-Object { [Environment]::ExpandEnvironmentVariables($_).TrimEnd('\') })
    if ($entries -notcontains $Directory.TrimEnd('\')) {
        Write-Output "Adding $Directory to your user PATH (HKCU\Environment); broadcasting the change."
        $updated = if ([string]::IsNullOrEmpty($current)) { $Directory } else { "$($current.TrimEnd(';'));$Directory" }
        Set-KaisaUserPath $updated
        Send-KaisaEnvironmentChange
    }
}
function Get-KaisaDownload([string]$Url, [string]$Destination) {
    $uri = [Uri]$Url
    if ($uri.IsFile) { Copy-Item -LiteralPath $uri.LocalPath -Destination $Destination }
    else { Invoke-WebRequest -UseBasicParsing -Uri $Url -OutFile $Destination }
}
function Install-Kaisa {
    if ($env:OS -ne 'Windows_NT') { throw 'kaisa install: Windows is required; use install.sh on macOS/Linux' }
    $architecture = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
    if ($architecture -ne 'AMD64') { throw 'kaisa install: only x86_64 Windows is supported' }
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
    $base = if ($env:KAISA_INSTALL_BASE_URL) { $env:KAISA_INSTALL_BASE_URL.TrimEnd('/') } else { 'https://github.com/sangwookp9591/kaisa/releases' }
    $work = Join-Path ([IO.Path]::GetTempPath()) ([Guid]::NewGuid().ToString())
    $null = New-Item -ItemType Directory -Path $work
    try {
        $version = $env:KAISA_VERSION
        if (-not $version) {
            Get-KaisaDownload "$base/latest/download/VERSION" (Join-Path $work 'VERSION')
            $version = (Get-Content -LiteralPath (Join-Path $work 'VERSION') -Raw).Trim()
        }
        $version = $version -replace '^v', ''
        if ($version -notmatch '^[0-9A-Za-z.+-]+$') { throw 'kaisa install: invalid release version' }
        $asset = "kaisa-$version-x86_64-pc-windows-msvc.zip"
        $archive = Join-Path $work $asset
        Get-KaisaDownload "$base/download/v$version/$asset" $archive
        $sums = Join-Path $work 'SHA256SUMS'
        Get-KaisaDownload "$base/download/v$version/SHA256SUMS" $sums
        $pattern = '^([0-9a-fA-F]{64})  ' + [regex]::Escape($asset) + '$'
        $entries = @(Get-Content -LiteralPath $sums | Where-Object { $_ -match $pattern })
        if ($entries.Count -ne 1) { throw 'kaisa install: checksum entry missing, invalid or duplicated' }
        $expected = [regex]::Match($entries[0], $pattern).Groups[1].Value
        if ((Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash -ne $expected) {
            throw "kaisa install: checksum mismatch for $asset; nothing installed"
        }
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        $zip = [IO.Compression.ZipFile]::OpenRead($archive)
        try {
            $binary = @($zip.Entries | Where-Object { $_.FullName -ceq 'kaisa.exe' })
            if ($binary.Count -ne 1) { throw 'kaisa install: archive must contain one kaisa.exe' }
            [IO.Compression.ZipFileExtensions]::ExtractToFile($binary[0], (Join-Path $work 'kaisa.exe'))
        } finally { $zip.Dispose() }
        $directory = if ($env:KAISA_INSTALL_DIR) { $env:KAISA_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Programs\kaisa' }
        if (-not [IO.Path]::IsPathRooted($directory) -or $directory.Contains(';')) { throw 'kaisa install: install directory must be absolute and contain no semicolon' }
        $directory = [IO.Path]::GetFullPath($directory)
        $null = New-Item -ItemType Directory -Force -Path $directory
        Copy-Item -LiteralPath (Join-Path $work 'kaisa.exe') -Destination (Join-Path $directory 'kaisa.exe') -Force
        Write-Output "Installed kaisa $version to $directory\kaisa.exe"
        Update-KaisaUserPath $directory
        Write-Output 'Open a new terminal, then run: kaisa'
    } finally { Remove-Item -LiteralPath $work -Recurse -Force }
}
if ($MyInvocation.InvocationName -ne '.') { Install-Kaisa }
