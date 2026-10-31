<#
.SYNOPSIS
Install the prebuilt repogi executable on Windows (x64).

.DESCRIPTION
Downloads the release archive from the official GitHub Release, verifies it
against the release's SHA256SUMS, and installs only repogi.exe into a per-user
directory (default: %LOCALAPPDATA%\Programs\Repogi\bin). Needs no administrator
rights and never changes the execution policy. PATH is changed only with
-AddToPath, and then only for the current user. An existing installation is
replaced only after the new executable verified and ran successfully.

.EXAMPLE
irm https://github.com/thinkgrid-labs/repogi/releases/latest/download/install.ps1 | iex

.EXAMPLE
& ([scriptblock]::Create((irm https://github.com/thinkgrid-labs/repogi/releases/download/v0.1.0/install.ps1))) -Version 0.1.0

.EXAMPLE
powershell -NoProfile -ExecutionPolicy Bypass -File .\install.ps1 -Version 0.1.0 -AddToPath
(Bypass applies to this one process only.)
#>
[CmdletBinding()]
param(
    # Release to install, e.g. 0.1.0 or 0.1.1-preview.1. Default: latest stable release.
    [string]$Version = $env:REPOGI_VERSION,
    # Directory for repogi.exe. Default: %LOCALAPPDATA%\Programs\Repogi\bin.
    [string]$InstallDir = $env:REPOGI_INSTALL_DIR,
    # Add the install directory to the current user's PATH.
    [switch]$AddToPath,
    # Also verify GitHub build provenance with the gh CLI.
    [switch]$VerifyAttestation
)

Set-StrictMode -Version 3.0
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$Repository = 'thinkgrid-labs/repogi'
$OfficialReleases = "https://github.com/$Repository/releases"
$Target = 'x86_64-pc-windows-msvc'

function Say([string]$Message) { [Console]::Error.WriteLine("repogi-install: $Message") }

function Get-FinalUri($Response) {
    # Windows PowerShell 5.1 exposes HttpWebResponse; PowerShell 7 exposes HttpResponseMessage.
    $base = $Response.BaseResponse
    if ($base.PSObject.Properties['ResponseUri']) { return [string]$base.ResponseUri }
    return [string]$base.RequestMessage.RequestUri
}

function Invoke-Download([string]$Uri, [string]$OutFile) {
    Invoke-WebRequest -UseBasicParsing -Uri $Uri -OutFile $OutFile
}

function Install-Repogi {
    if ($env:OS -ne 'Windows_NT') {
        throw "install.ps1 is for Windows. On macOS or Linux use install.sh: https://github.com/$Repository#install"
    }
    $arch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
    if ($arch -ne 'AMD64') {
        throw "unsupported Windows architecture: $arch. Prebuilt binaries exist for x64 (AMD64) only; build from source instead: https://github.com/$Repository#build-from-source"
    }

    $releases = if ($env:REPOGI_RELEASES_URL) { $env:REPOGI_RELEASES_URL.TrimEnd('/') } else { $OfficialReleases }
    if ($releases -notmatch '^https://' -and $releases -notmatch '^http://(127\.0\.0\.1|localhost)(:\d+)?/') {
        throw "REPOGI_RELEASES_URL must be an https URL: $releases"
    }
    if ($releases -ne $OfficialReleases) { Say "warning: downloading from $releases instead of the official GitHub Release" }
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

    $ver = $Version
    if (-not $ver) {
        $final = ''
        try { $final = Get-FinalUri (Invoke-WebRequest -UseBasicParsing -Method Head -Uri "$releases/latest") } catch { $final = '' }
        if ($final -notmatch '/releases/tag/v([^/?#]+)$') {
            throw "no stable Repogi release is published yet (or GitHub could not be reached). Install a specific release with -Version VERSION; see $OfficialReleases"
        }
        $ver = $Matches[1]
        Say "latest stable release: $ver"
    }
    $ver = $ver -replace '^v', ''
    if ($ver -notmatch '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z.-]+)?$') {
        throw "invalid version: $ver (expected MAJOR.MINOR.PATCH, optionally -PRERELEASE)"
    }

    if (-not $InstallDir) {
        if (-not $env:LOCALAPPDATA) { throw 'LOCALAPPDATA is not set; pass -InstallDir' }
        $InstallDir = Join-Path $env:LOCALAPPDATA 'Programs\Repogi\bin'
    }
    $installPath = [IO.Path]::GetFullPath($InstallDir)

    $stem = "repogi-$ver-$Target"
    $archive = "$stem.zip"
    $baseUrl = "$releases/download/v$ver"
    $tmp = Join-Path ([IO.Path]::GetTempPath()) ('repogi-install-' + [guid]::NewGuid().ToString('N'))
    $staged = $null
    $createdDirs = @()
    $installed = $false
    New-Item -ItemType Directory -Path $tmp | Out-Null
    try {
        Say "downloading $archive"
        try { Invoke-Download "$baseUrl/SHA256SUMS" (Join-Path $tmp 'SHA256SUMS') }
        catch { throw "could not download SHA256SUMS for release $ver from $baseUrl (does the release exist?): $($_.Exception.Message)" }
        $zip = Join-Path $tmp $archive
        try { Invoke-Download "$baseUrl/$archive" $zip }
        catch { throw "could not download $archive from $baseUrl (is Windows x64 published for $ver?): $($_.Exception.Message)" }

        $entries = @(Get-Content -LiteralPath (Join-Path $tmp 'SHA256SUMS') | ForEach-Object {
            if ($_ -match '^([0-9a-f]{64}) [ *](.+)$' -and $Matches[2] -ceq $archive) { $Matches[1] }
        })
        if ($entries.Count -ne 1) { throw "SHA256SUMS has no single valid entry for $archive" }
        $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $zip).Hash.ToLowerInvariant()
        if ($actual -ne $entries[0]) {
            throw "checksum mismatch for $archive (expected $($entries[0]), got $actual); nothing was installed"
        }
        Say "verified SHA-256 $actual"

        if ($VerifyAttestation) {
            if (-not (Get-Command gh -ErrorAction SilentlyContinue)) { throw '-VerifyAttestation needs the GitHub CLI (gh)' }
            & gh attestation verify $zip --repo $Repository
            if ($LASTEXITCODE -ne 0) { throw "build provenance verification failed for $archive" }
        }

        Add-Type -AssemblyName System.IO.Compression.FileSystem
        $exe = Join-Path $tmp 'repogi.exe'
        $reader = [IO.Compression.ZipFile]::OpenRead($zip)
        try {
            $wanted = $null
            foreach ($entry in $reader.Entries) {
                $name = $entry.FullName
                $segments = $name.TrimEnd('/').Split('/')
                if ($name.StartsWith('/') -or $name.Contains('\') -or $name.Contains(':') -or $segments -contains '..' -or
                    $segments[0] -cne $stem) {
                    throw "unsafe or unexpected path in archive: $name"
                }
                if ((($entry.ExternalAttributes -shr 16) -band 0xF000) -eq 0xA000) { throw "archive contains a link: $name" }
                if ($name -ceq "$stem/repogi.exe") { $wanted = $entry }
            }
            if (-not $wanted) { throw "$archive does not contain $stem/repogi.exe" }
            [IO.Compression.ZipFileExtensions]::ExtractToFile($wanted, $exe)
        } finally {
            $reader.Dispose()
        }

        $probe = $installPath
        while (-not (Test-Path -LiteralPath $probe)) {
            $createdDirs = @($probe) + $createdDirs
            $probe = Split-Path -Parent $probe
        }
        New-Item -ItemType Directory -Force -Path $installPath | Out-Null
        $destination = Join-Path $installPath 'repogi.exe'
        if (Test-Path -LiteralPath $destination -PathType Container) { throw "$destination is a directory; refusing to replace it" }

        $staged = Join-Path $installPath ('.repogi.install.' + [guid]::NewGuid().ToString('N') + '.exe')
        Copy-Item -LiteralPath $exe -Destination $staged
        $reported = (& $staged --version 2>$null | Out-String).Trim()
        if ($LASTEXITCODE -ne 0) { throw 'the downloaded repogi.exe does not run on this system; nothing was installed' }
        if ($reported -cne "repogi $ver") {
            throw "the downloaded executable reports '$reported', expected 'repogi $ver'; nothing was installed"
        }
        try { Move-Item -LiteralPath $staged -Destination $destination -Force }
        catch { throw "could not replace $destination (is repogi running?): $($_.Exception.Message)" }
        $staged = $null
        $installed = $true
        Say "installed $reported to $destination"
    } finally {
        if ($staged -and (Test-Path -LiteralPath $staged)) { Remove-Item -LiteralPath $staged -Force }
        Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
        if (-not $installed) {
            # Remove only directories this run created, deepest first, and only while empty.
            for ($i = $createdDirs.Count - 1; $i -ge 0; $i--) {
                $dir = $createdDirs[$i]
                if ((Test-Path -LiteralPath $dir) -and -not (Get-ChildItem -LiteralPath $dir -Force)) {
                    Remove-Item -LiteralPath $dir -Force
                }
            }
        }
    }

    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $onUserPath = @($userPath -split ';' | Where-Object { $_ -and $_.TrimEnd('\') -ieq $installPath.TrimEnd('\') }).Count -gt 0
    if ($onUserPath) {
        Say "$installPath is already on your user PATH. Open a new terminal and run: repogi --version"
    } elseif ($AddToPath) {
        $newPath = if ($userPath) { "$($userPath.TrimEnd(';'));$installPath" } else { $installPath }
        [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
        Say "added $installPath to your user PATH (no system settings were changed). Open a new terminal and run: repogi --version"
    } else {
        Say "$installPath is not on your PATH. Either rerun with -AddToPath, or add it for your user yourself:"
        Say "  [Environment]::SetEnvironmentVariable('Path', [Environment]::GetEnvironmentVariable('Path','User') + ';$installPath', 'User')"
        Say "then open a new terminal and run: repogi --version   (or run `"$installPath\repogi.exe`" directly)"
    }
}

Install-Repogi
