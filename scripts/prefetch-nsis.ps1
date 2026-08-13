# Stage the NSIS toolset the Tauri bundler needs, before the bundler asks for it.
#
# `tauri build --bundles nsis` downloads its own NSIS toolset from GitHub at
# bundle time, unauthenticated and uncached, and fails the whole build when that
# request is refused:
#
#     Info Verifying NSIS package
#      Downloading https://github.com/tauri-apps/binary-releases/releases/download/nsis-3.11/nsis-3.11.zip
#     failed to bundle project: `io: Peer disconnected`
#
# That failure arrived 0.09 seconds after the request - a connection reset, not a
# timeout - and it repeated on a re-run of the same commit, so it is not the kind
# of flake a retry fixes. It is the last unpinned network dependency in a build
# whose every other asset is pinned, hash-verified, and cached.
#
# The bundler skips its download when the toolset is already staged, so this
# script stages it. Both artifacts are pinned to the exact versions and digests
# tauri-bundler itself expects; if it ever wants different ones, the build fails
# loudly on a missing file rather than silently bundling something unexpected.
[CmdletBinding()]
param(
    [string]$CacheDirectory = "",
    # Where tauri-bundler looks: dirs::cache_dir()/tauri/NSIS, which on Windows
    # is %LOCALAPPDATA%\tauri\NSIS.
    [string]$ToolsetDirectory = (Join-Path $env:LOCALAPPDATA "tauri\NSIS")
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

# tauri-bundler pins these by SHA-1. SHA-1 is not collision resistant, so the
# size and SHA-256 below are this repository's own check on the same bytes; the
# SHA-1 is carried too so a mismatch with the bundler's expectation is visible
# here rather than at bundle time.
$Downloads = @(
    [pscustomobject]@{
        Id = "nsis-3.11.zip"
        Url = "https://github.com/tauri-apps/binary-releases/releases/download/nsis-3.11/nsis-3.11.zip"
        Size = 2361546
        Sha256 = "c7d27f780ddb6cffb4730138cd1591e841f4b7edb155856901cdf5f214394fa1"
        Sha1 = "ef7ff767e5cbd9edd22add3a32c9b8f4500bb10d"
    },
    [pscustomobject]@{
        Id = "nsis_tauri_utils.dll"
        Url = "https://github.com/tauri-apps/nsis-tauri-utils/releases/download/nsis_tauri_utils-v0.5.3/nsis_tauri_utils.dll"
        Size = 34304
        Sha256 = "5ba143b5db4a87d32d6e7802e033330aae56cbceabe0d1e3ba41948385ad4709"
        Sha1 = "75197fee3c6a814fe035788d1c34ead39349b860"
    }
)

# The files tauri-bundler tests for before deciding the toolset is usable. If
# this list stops matching what it wants, the bundler falls back to downloading,
# which is exactly the failure this script exists to remove - so the staged tree
# is asserted against this list at the end.
$RequiredFiles = @(
    "makensis.exe",
    "Bin/makensis.exe",
    "Stubs/lzma-x86-unicode",
    "Stubs/lzma_solid-x86-unicode",
    "Plugins/x86-unicode/additional/nsis_tauri_utils.dll"
)

$WorkDirectory = if ($CacheDirectory) {
    [System.IO.Path]::GetFullPath($CacheDirectory)
} else {
    Join-Path ([System.IO.Path]::GetTempPath()) ("intern-nsis-" + [guid]::NewGuid().ToString("N"))
}
$OwnsWorkDirectory = -not $CacheDirectory
New-Item -ItemType Directory -Path $WorkDirectory -Force | Out-Null

function Assert-ExactFile {
    param([string]$Path, [long]$Size, [string]$Sha256, [string]$Sha1, [string]$Label)
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { throw "$Label is missing: $Path" }
    $ActualSize = (Get-Item -LiteralPath $Path).Length
    if ($ActualSize -ne $Size) { throw "$Label size mismatch: expected $Size, got $ActualSize" }
    $ActualSha256 = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($ActualSha256 -ne $Sha256) { throw "$Label SHA-256 mismatch: expected $Sha256, got $ActualSha256" }
    $ActualSha1 = (Get-FileHash -LiteralPath $Path -Algorithm SHA1).Hash.ToLowerInvariant()
    if ($ActualSha1 -ne $Sha1) { throw "$Label SHA-1 mismatch: expected $Sha1, got $ActualSha1" }
}

function Get-PinnedDownload {
    param([pscustomobject]$Download)
    $Destination = Join-Path $WorkDirectory $Download.Id
    # A cached copy still has to satisfy the digests below, so a truncated or
    # tampered cache entry is rejected rather than trusted.
    if (Test-Path -LiteralPath $Destination -PathType Leaf) {
        try {
            Assert-ExactFile -Path $Destination -Size $Download.Size -Sha256 $Download.Sha256 -Sha1 $Download.Sha1 -Label $Download.Id
            return $Destination
        } catch {
            Write-Warning "Discarding unusable cached $($Download.Id): $($_.Exception.Message)"
            Remove-Item -LiteralPath $Destination -Force
        }
    }
    $Attempts = 0
    while (-not (Test-Path -LiteralPath $Destination -PathType Leaf)) {
        $Attempts += 1
        try {
            Invoke-WebRequest -Uri $Download.Url -OutFile $Destination -MaximumRedirection 5
        } catch {
            if (Test-Path -LiteralPath $Destination) { Remove-Item -LiteralPath $Destination -Force }
            if ($Attempts -ge 5) { throw "Failed to download $($Download.Id) after $Attempts attempts: $($_.Exception.Message)" }
            Write-Warning "Download of $($Download.Id) failed on attempt ${Attempts}: $($_.Exception.Message)"
            Start-Sleep -Seconds (10 * $Attempts)
        }
    }
    Assert-ExactFile -Path $Destination -Size $Download.Size -Sha256 $Download.Sha256 -Sha1 $Download.Sha1 -Label $Download.Id
    return $Destination
}

function Test-StagedToolset {
    param([string]$Root)
    if (-not (Test-Path -LiteralPath $Root -PathType Container)) { return $false }
    foreach ($Relative in $RequiredFiles) {
        if (-not (Test-Path -LiteralPath (Join-Path $Root $Relative) -PathType Leaf)) { return $false }
    }
    return $true
}

try {
    if (Test-StagedToolset -Root $ToolsetDirectory) {
        Write-Host "NSIS toolset already staged at $ToolsetDirectory; the bundler will not download one."
        exit 0
    }

    $Archive = Get-PinnedDownload -Download $Downloads[0]
    $Plugin = Get-PinnedDownload -Download $Downloads[1]

    # Extract beside the destination and move into place, so an interrupted run
    # cannot leave a half-populated toolset that Test-StagedToolset would later
    # accept.
    $Staging = Join-Path $WorkDirectory ("stage-" + [guid]::NewGuid().ToString("N"))
    New-Item -ItemType Directory -Path $Staging -Force | Out-Null
    Expand-Archive -LiteralPath $Archive -DestinationPath $Staging -Force
    $Extracted = Join-Path $Staging "nsis-3.11"
    if (-not (Test-Path -LiteralPath $Extracted -PathType Container)) {
        throw "nsis-3.11.zip did not contain the expected nsis-3.11 directory"
    }
    # The bundler loads its own plugin from Plugins/x86-unicode/additional, a
    # directory the stock NSIS archive does not ship.
    $PluginDirectory = Join-Path $Extracted "Plugins/x86-unicode/additional"
    New-Item -ItemType Directory -Path $PluginDirectory -Force | Out-Null
    Copy-Item -LiteralPath $Plugin -Destination (Join-Path $PluginDirectory "nsis_tauri_utils.dll") -Force

    $ToolsetParent = Split-Path -Parent $ToolsetDirectory
    if ($ToolsetParent) { New-Item -ItemType Directory -Path $ToolsetParent -Force | Out-Null }
    if (Test-Path -LiteralPath $ToolsetDirectory) { Remove-Item -LiteralPath $ToolsetDirectory -Recurse -Force }
    Move-Item -LiteralPath $Extracted -Destination $ToolsetDirectory

    foreach ($Relative in $RequiredFiles) {
        $Full = Join-Path $ToolsetDirectory $Relative
        if (-not (Test-Path -LiteralPath $Full -PathType Leaf)) { throw "Staged NSIS toolset is missing $Relative" }
    }
    Assert-ExactFile -Path (Join-Path $ToolsetDirectory "Plugins/x86-unicode/additional/nsis_tauri_utils.dll") `
        -Size $Downloads[1].Size -Sha256 $Downloads[1].Sha256 -Sha1 $Downloads[1].Sha1 -Label "staged nsis_tauri_utils.dll"

    Write-Host "Staged pinned NSIS 3.11 and nsis_tauri_utils 0.5.3 at $ToolsetDirectory."
}
finally {
    if ($OwnsWorkDirectory -and (Test-Path -LiteralPath $WorkDirectory)) {
        Remove-Item -LiteralPath $WorkDirectory -Recurse -Force -ErrorAction SilentlyContinue
    } elseif (Test-Path -LiteralPath $WorkDirectory) {
        Get-ChildItem -LiteralPath $WorkDirectory -Directory -Filter "stage-*" |
            Remove-Item -Recurse -Force -ErrorAction SilentlyContinue
    }
}
