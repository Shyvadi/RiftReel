<#
.SYNOPSIS
  Downloads the pinned LGPL ffmpeg build RiftReel records with, checks its SHA-256 and
  installs ffmpeg.exe with its license where RiftReel looks for it.

.DESCRIPTION
  RiftReel captures with ffmpeg's `ddagrab` filter (Desktop Duplication, nothing
  injected into the game) and a hardware encoder (NVENC, AMF or QSV). The ffmpeg 5.1
  "essentials" builds have no ddagrab, and GPL builds include x264, so the repository pins
  one LGPL build from https://github.com/BtbN/FFmpeg-Builds instead of using whatever
  ffmpeg is on PATH.

  The pin is a month-end "autobuild" release: BtbN keeps those for about two years, but
  daily builds for only a couple of weeks. To move to another build, change $Release,
  $Asset, $Sha256 and $SourceCommit together; the release's checksums.sha256 lists the
  hash.

  Installs (nothing of it is committed; src-tauri/binaries/ is in .gitignore):
    ffmpeg-x86_64-pc-windows-msvc.exe   Tauri's sidecar name; debug builds run it from here
    ffmpeg-license/LICENSE.txt          the license shipped in the archive
    ffmpeg-license/SOURCE.txt           where the corresponding source is
    ffmpeg.stamp                        what is installed, so reruns can skip the download

.PARAMETER Destination
  Where to install. Default: src-tauri\binaries in this repository.

.PARAMETER Force
  Download and install again even if the pinned build is already there.

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File tools\fetch-ffmpeg.ps1
#>
[CmdletBinding()]
param(
    [string]$Destination,
    [switch]$Force
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (-not $Destination) {
    # $PSScriptRoot isn't set yet in Windows PowerShell 5.1 parameter defaults.
    $Destination = Join-Path (Split-Path -Parent $MyInvocation.MyCommand.Path) '..\src-tauri\binaries'
}
# Invoke-WebRequest in Windows PowerShell 5.1 crawls while drawing its progress bar.
$ProgressPreference = 'SilentlyContinue'

# ---- The pinned build ------------------------------------------------------------------
$Release = 'autobuild-2026-09-30-13-08'
$Asset = 'ffmpeg-n8.1.3-9-g29e619e767-win64-lgpl-8.1.zip'
$Sha256 = '4a7642b2264c03e8a0ce8a3825b933ee5580656f45695a086fe7e294045ffc0a'
# FFmpeg's release/8.1 branch, 9 commits after n8.1.3.
$SourceCommit = '29e619e767'
# -----------------------------------------------------------------------------------------

$Url = "https://github.com/BtbN/FFmpeg-Builds/releases/download/$Release/$Asset"
# Tauri's sidecar naming (<name>-<target triple>.exe); bundling strips the triple.
$ExeName = 'ffmpeg-x86_64-pc-windows-msvc.exe'
$LicenseDirName = 'ffmpeg-license'
$StampName = 'ffmpeg.stamp'
# What RiftReel needs from the build.
$RequiredFilters = @('ddagrab')
$HardwareEncoders = @('h264_nvenc', 'hevc_nvenc', 'av1_nvenc', 'h264_amf', 'hevc_amf', 'h264_qsv', 'hevc_qsv')

function Get-Sha256([string]$Path) {
    (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash.ToLowerInvariant()
}

function Get-Stamp([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path)) { return $null }
    $stamp = @{}
    foreach ($line in Get-Content -LiteralPath $Path) {
        $parts = $line -split '=', 2
        if ($parts.Count -eq 2) { $stamp[$parts[0].Trim()] = $parts[1].Trim() }
    }
    return $stamp
}

New-Item -ItemType Directory -Force -Path $Destination | Out-Null
$Destination = (Resolve-Path -LiteralPath $Destination).Path
$exePath = Join-Path $Destination $ExeName
$licenseDir = Join-Path $Destination $LicenseDirName
$stampPath = Join-Path $Destination $StampName

$stamp = Get-Stamp $stampPath
if (-not $Force -and $stamp -and $stamp['archive_sha256'] -eq $Sha256 -and (Test-Path -LiteralPath $exePath) -and
    (Test-Path -LiteralPath (Join-Path $licenseDir 'LICENSE.txt')) -and (Get-Sha256 $exePath) -eq $stamp['exe_sha256']) {
    Write-Host "ffmpeg $Asset is already installed in $Destination"
    exit 0
}

$work = Join-Path ([IO.Path]::GetTempPath()) ('riftreel-ffmpeg-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $work | Out-Null
try {
    $zip = Join-Path $work $Asset
    Write-Host "Downloading $Url"
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
    Invoke-WebRequest -Uri $Url -OutFile $zip -UseBasicParsing

    $actual = Get-Sha256 $zip
    if ($actual -ne $Sha256) {
        throw "SHA-256 mismatch for ${Asset}: expected $Sha256, got $actual. Not installing it."
    }
    Write-Host "SHA-256 OK ($Sha256)"

    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $archive = [IO.Compression.ZipFile]::OpenRead($zip)
    try {
        # One top-level folder named after the build: <folder>/bin/ffmpeg.exe, <folder>/LICENSE.txt.
        $exeEntry = @($archive.Entries | Where-Object { $_.FullName -match '^[^/]+/bin/ffmpeg\.exe$' })
        $licenseEntry = @($archive.Entries | Where-Object { $_.FullName -match '^[^/]+/LICENSE(\.txt)?$' })
        if ($exeEntry.Count -ne 1) { throw "bin/ffmpeg.exe not found in $Asset" }
        if ($licenseEntry.Count -ne 1) { throw "LICENSE.txt not found in $Asset" }
        $newExe = Join-Path $work 'ffmpeg.exe'
        $newLicense = Join-Path $work 'LICENSE.txt'
        [IO.Compression.ZipFileExtensions]::ExtractToFile($exeEntry[0], $newExe, $true)
        [IO.Compression.ZipFileExtensions]::ExtractToFile($licenseEntry[0], $newLicense, $true)
    }
    finally {
        $archive.Dispose()
    }

    # Check the build before installing it.
    $version = (& $newExe -hide_banner -version | Select-Object -First 1)
    if ($LASTEXITCODE -ne 0) { throw "the extracted ffmpeg.exe does not run (exit code $LASTEXITCODE)" }
    $filters = (& $newExe -hide_banner -filters | Out-String)
    $encoders = (& $newExe -hide_banner -encoders | Out-String)
    foreach ($filter in $RequiredFilters) {
        if ($filters -notmatch "\s$filter\s") { throw "this ffmpeg build has no '$filter' filter" }
    }
    $present = @($HardwareEncoders | Where-Object { $encoders -match "\s$_\s" })
    if ($encoders -match '\slibx264\s') {
        throw 'this ffmpeg build includes libx264 (GPL); RiftReel ships an LGPL build'
    }

    New-Item -ItemType Directory -Force -Path $licenseDir | Out-Null
    Move-Item -Force -LiteralPath $newExe -Destination $exePath
    Move-Item -Force -LiteralPath $newLicense -Destination (Join-Path $licenseDir 'LICENSE.txt')
    $source = @(
        "FFmpeg for RiftReel",
        "",
        "Build:   $Asset",
        "From:    $Url",
        "SHA-256: $Sha256",
        "Version: $version",
        "",
        "This is an LGPL build of FFmpeg (see LICENSE.txt in this folder), run by RiftReel",
        "as a separate program. FFmpeg's source for this build is commit $SourceCommit of",
        "https://git.ffmpeg.org/ffmpeg.git (mirror: https://github.com/FFmpeg/FFmpeg/commit/$SourceCommit).",
        "The scripts that configured and built it, and the sources of the libraries it",
        "includes, are at https://github.com/BtbN/FFmpeg-Builds (release $Release)."
    )
    Set-Content -LiteralPath (Join-Path $licenseDir 'SOURCE.txt') -Value $source -Encoding UTF8

    $stampLines = @(
        "archive=$Asset",
        "archive_sha256=$Sha256",
        "exe_sha256=$(Get-Sha256 $exePath)",
        "version=$version"
    )
    Set-Content -LiteralPath $stampPath -Value $stampLines -Encoding UTF8

    Write-Host "Installed $version"
    Write-Host "  $exePath"
    Write-Host "  hardware encoders in this build: $($present -join ', ')"
    Write-Host "  license: $licenseDir"
}
finally {
    Remove-Item -Recurse -Force -LiteralPath $work -ErrorAction SilentlyContinue
}
