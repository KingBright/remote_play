param(
    [string]$FfmpegPath = $env:REMOTE_PLAY_FFMPEG_BIN,
    [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
Set-Location -LiteralPath $root

if ([string]::IsNullOrWhiteSpace($env:CARGO_TARGET_DIR)) {
    $metadata = cargo metadata --format-version 1 --no-deps | ConvertFrom-Json
    $targetDir = $metadata.target_directory
} elseif ([IO.Path]::IsPathRooted($env:CARGO_TARGET_DIR)) {
    $targetDir = [IO.Path]::GetFullPath($env:CARGO_TARGET_DIR)
} else {
    $targetDir = [IO.Path]::GetFullPath((Join-Path $root $env:CARGO_TARGET_DIR))
}
$releaseExe = Join-Path $targetDir 'release\remote_play.exe'
$packageRoot = Join-Path $targetDir 'package\windows'
$appDir = Join-Path $packageRoot 'RemotePlay'
$zipPath = Join-Path $packageRoot 'RemotePlay-Windows-x64.zip'

if (-not $SkipBuild) {
    cargo build --release -p remote_play_app --bin remote_play --locked --features native-windows-video
    if ($LASTEXITCODE -ne 0) {
        throw "RemotePlay release build failed with exit code $LASTEXITCODE"
    }
}
if (-not (Test-Path -LiteralPath $releaseExe -PathType Leaf)) {
    throw "RemotePlay release binary is missing: $releaseExe"
}

$productVersion = (Get-Content -LiteralPath (Join-Path $root 'VERSION') -Raw).Trim()
& (Join-Path $root 'scripts\verify_desktop_gui.ps1') -Binary $releaseExe -Version $productVersion

if ([string]::IsNullOrWhiteSpace($FfmpegPath)) {
    foreach ($candidate in @(
        (Join-Path $root 'vendor\ffmpeg\bin\ffmpeg.exe'),
        (Join-Path $root 'third_party\ffmpeg\bin\ffmpeg.exe')
    )) {
        if (Test-Path -LiteralPath $candidate -PathType Leaf) {
            $FfmpegPath = $candidate
            break
        }
    }
}
if ([string]::IsNullOrWhiteSpace($FfmpegPath)) {
    throw 'A private FFmpeg bundle is required. Pass -FfmpegPath or set REMOTE_PLAY_FFMPEG_BIN.'
}
if (-not [IO.Path]::IsPathRooted($FfmpegPath)) {
    throw 'FfmpegPath must be absolute; product packages never resolve FFmpeg from PATH.'
}
$ffmpeg = (Resolve-Path -LiteralPath $FfmpegPath).Path

function Invoke-FfmpegProbe([string]$Flag) {
    $text = (& $ffmpeg -hide_banner $Flag 2>&1 | Out-String)
    if ($LASTEXITCODE -ne 0) {
        throw "FFmpeg capability probe $Flag failed with exit code $LASTEXITCODE"
    }
    return $text
}

$devices = Invoke-FfmpegProbe '-devices'
if ($devices -notmatch '(?i)gdigrab') {
    throw 'FFmpeg bundle does not provide gdigrab.'
}
$encoders = Invoke-FfmpegProbe '-encoders'
if ($encoders -notmatch '(?i)libx265') {
    throw 'FFmpeg bundle does not provide libx265 HEVC encoding.'
}
$versionText = (& $ffmpeg -hide_banner -version 2>&1 | Out-String)
if ($LASTEXITCODE -ne 0) {
    throw "FFmpeg version probe failed with exit code $LASTEXITCODE"
}
$licenseText = (& $ffmpeg -hide_banner -L 2>&1 | Out-String)
if ($LASTEXITCODE -ne 0) {
    throw "FFmpeg license probe failed with exit code $LASTEXITCODE"
}

Remove-Item -LiteralPath $appDir -Recurse -Force -ErrorAction SilentlyContinue
Remove-Item -LiteralPath $zipPath -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path (Join-Path $appDir 'bin') | Out-Null
Copy-Item -LiteralPath $releaseExe -Destination (Join-Path $appDir 'remote_play.exe')
Copy-Item -LiteralPath $ffmpeg -Destination (Join-Path $appDir 'bin\ffmpeg.exe')
Set-Content -LiteralPath (Join-Path $appDir 'ffmpeg-license.txt') -Value $licenseText -Encoding UTF8

$sourceManifest = Join-Path $root 'source-manifest.json'
$sourceId = $null
if (Test-Path -LiteralPath $sourceManifest -PathType Leaf) {
    try {
        $sourceId = (Get-Content -LiteralPath $sourceManifest -Raw | ConvertFrom-Json).source_id
    } catch {
        $sourceId = $null
    }
}
$buildInfo = [ordered]@{
    product = 'RemotePlay'
    platform = 'windows-x64'
    package_version = (Get-Content -LiteralPath (Join-Path $root 'VERSION') -Raw).Trim()
    source_id = $sourceId
    remote_play_sha256 = (Get-FileHash -LiteralPath (Join-Path $appDir 'remote_play.exe') -Algorithm SHA256).Hash.ToLower()
    ffmpeg_sha256 = (Get-FileHash -LiteralPath (Join-Path $appDir 'bin\ffmpeg.exe') -Algorithm SHA256).Hash.ToLower()
    ffmpeg_version = (($versionText -split "\r?\n")[0]).Trim()
    capture_backend = 'gdigrab-physical-display-only'
    encoder = 'libx265'
    system_ffmpeg_fallback = $false
    generated_at = (Get-Date).ToUniversalTime().ToString('o')
}
$buildInfo | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path $appDir 'build-info.json') -Encoding UTF8

Compress-Archive -Path $appDir -DestinationPath $zipPath -CompressionLevel Optimal
Write-Output ('PACKAGE=' + $zipPath)
Write-Output ('PACKAGE_SHA256=' + (Get-FileHash -LiteralPath $zipPath -Algorithm SHA256).Hash.ToLower())
Write-Output ('REMOTE_PLAY_SHA256=' + $buildInfo.remote_play_sha256)
Write-Output ('FFMPEG_SHA256=' + $buildInfo.ffmpeg_sha256)
