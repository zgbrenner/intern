[CmdletBinding()]
param([Parameter(Mandatory = $true)][string]$Destination)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$RepositoryRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$ManifestPath = Join-Path $RepositoryRoot "src-tauri/resources/runtime-assets.json"
$Manifest = Get-Content -LiteralPath $ManifestPath -Raw | ConvertFrom-Json
if ($Manifest.bundled_files.Count -eq 0 -or $Manifest.license_files.Count -eq 0) { throw "Runtime and license inventories must be populated before staging" }

$Stage = [IO.Path]::GetFullPath($Destination)
if (Test-Path -LiteralPath $Stage) { throw "Refusing to overwrite existing runtime stage: $Stage" }
New-Item -ItemType Directory -Path $Stage | Out-Null
$StagePrefix = $Stage.TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
$Seen = @{}

foreach ($Entry in @($Manifest.bundled_files) + @($Manifest.license_files)) {
    $Relative = [string]$Entry.install_path
    if ([string]::IsNullOrWhiteSpace($Relative) -or $Relative.Contains("\") -or $Relative.Contains(":") -or ($Relative.Split("/") -contains "..")) {
        throw "Unsafe packaged runtime path: $Relative"
    }
    if ($Seen.ContainsKey($Relative)) { throw "Duplicate packaged runtime path: $Relative" }
    $Seen[$Relative] = $true
    $Source = [IO.Path]::GetFullPath((Join-Path $RepositoryRoot ([string]$Entry.path)))
    $Target = [IO.Path]::GetFullPath((Join-Path $Stage $Relative))
    if (-not $Target.StartsWith($StagePrefix, [StringComparison]::OrdinalIgnoreCase)) { throw "Packaged path escapes stage: $Relative" }
    if (-not (Test-Path -LiteralPath $Source -PathType Leaf)) { throw "Signed staging source is missing: $($Entry.path)" }
    $ActualSize = (Get-Item -LiteralPath $Source).Length
    $ActualHash = (Get-FileHash -LiteralPath $Source -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($ActualSize -ne [long]$Entry.size -or $ActualHash -ne [string]$Entry.sha256) { throw "Signed staging source does not match manifest: $($Entry.path)" }
    New-Item -ItemType Directory -Path (Split-Path -Parent $Target) -Force | Out-Null
    Copy-Item -LiteralPath $Source -Destination $Target
}

$MetadataDirectory = Join-Path $Stage "resources"
New-Item -ItemType Directory -Path $MetadataDirectory -Force | Out-Null
Copy-Item -LiteralPath $ManifestPath -Destination (Join-Path $MetadataDirectory "runtime-assets.json")
Copy-Item -LiteralPath (Join-Path $RepositoryRoot "src-tauri/resources/THIRD_PARTY_NOTICES.md") -Destination (Join-Path $MetadataDirectory "THIRD_PARTY_NOTICES.md")
Write-Host "Staged $($Manifest.bundled_files.Count) runtime files and $($Manifest.license_files.Count) license files at $Stage"
