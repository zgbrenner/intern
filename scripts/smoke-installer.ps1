[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$InstallerPath,
    [string]$InstallDirectory = (Join-Path $env:LOCALAPPDATA "Intern"),
    [string]$FixtureDirectory = (Join-Path (Resolve-Path (Join-Path $PSScriptRoot "..")).Path "fixtures/generated")
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$Installer = (Resolve-Path -LiteralPath $InstallerPath).Path
if ([System.IO.Path]::GetExtension($Installer) -ne ".exe") { throw "Installer must be an NSIS executable" }
$UserDataDirectory = Join-Path $env:LOCALAPPDATA "com.intern.app"
$Sentinel = Join-Path $UserDataDirectory "installer-smoke-user-data.txt"
New-Item -ItemType Directory -Path $UserDataDirectory -Force | Out-Null
Set-Content -LiteralPath $Sentinel -Value "must survive uninstall" -Encoding utf8NoBOM

if (Test-Path -LiteralPath $InstallDirectory) { throw "Smoke install target already exists: $InstallDirectory" }
$Install = Start-Process -FilePath $Installer -ArgumentList "/S" -Wait -PassThru
if ($Install.ExitCode -ne 0) { throw "NSIS installer exited with $($Install.ExitCode)" }

try {
    $App = Join-Path $InstallDirectory "Intern.exe"
    if (-not (Test-Path -LiteralPath $App -PathType Leaf)) { throw "Installed application is missing: $App" }

    $ManifestFiles = @(Get-ChildItem -LiteralPath $InstallDirectory -Recurse -File -Filter "runtime-assets.json")
    if ($ManifestFiles.Count -ne 1) { throw "Expected exactly one installed runtime-assets.json, got $($ManifestFiles.Count)" }
    $Manifest = Get-Content -LiteralPath $ManifestFiles[0].FullName -Raw | ConvertFrom-Json
    if ($Manifest.schema_version -ne 1 -or $Manifest.bundled_files.Count -eq 0 -or $Manifest.license_files.Count -eq 0) {
        throw "Installed runtime manifest is incomplete"
    }
    $Seen = @{}
    foreach ($Entry in @($Manifest.bundled_files) + @($Manifest.license_files)) {
        $Relative = [string]$Entry.install_path
        if ([string]::IsNullOrWhiteSpace($Relative) -or $Relative.Contains("\") -or $Relative.Contains(":") -or ($Relative.Split("/") -contains "..")) {
            throw "Unsafe installed manifest path: $Relative"
        }
        if ($Seen.ContainsKey($Relative)) { throw "Duplicate installed manifest path: $Relative" }
        $Seen[$Relative] = $true
        $Packaged = [IO.Path]::GetFullPath((Join-Path $InstallDirectory $Relative))
        $RootPrefix = [IO.Path]::GetFullPath($InstallDirectory).TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
        if (-not $Packaged.StartsWith($RootPrefix, [StringComparison]::OrdinalIgnoreCase)) { throw "Installed manifest path escapes root: $Relative" }
        if (-not (Test-Path -LiteralPath $Packaged -PathType Leaf)) { throw "Signed packaged file is missing: $Relative" }
        $File = Get-Item -LiteralPath $Packaged
        if ($File.Length -ne [long]$Entry.size) { throw "Installed size mismatch: $Relative" }
        $Digest = (Get-FileHash -LiteralPath $Packaged -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($Digest -ne [string]$Entry.sha256) { throw "Installed SHA-256 mismatch: $Relative" }
    }
    foreach ($Required in @("intern-worker.exe", "llama-server.exe", "tesseract.exe", "pdfium.dll", "tessdata/eng.traineddata", "tessdata/osd.traineddata")) {
        if (-not $Seen.ContainsKey($Required)) { throw "Installed manifest omits required runtime: $Required" }
    }
    if (-not (Get-ChildItem -LiteralPath $InstallDirectory -Recurse -File -Filter "THIRD_PARTY_NOTICES.md")) { throw "Third-party notices are missing from the installation" }
    if (-not (Get-ChildItem -LiteralPath (Join-Path $InstallDirectory "licenses") -Recurse -File -Filter "*.txt")) { throw "Complete license texts are missing from the installation" }
    if (Get-ChildItem -LiteralPath $InstallDirectory -Recurse -File -Filter "*.gguf") { throw "Model files must not be bundled in the installer" }

    & (Join-Path $PSScriptRoot "smoke-worker.ps1") -WorkerPath (Join-Path $InstallDirectory "intern-worker.exe") -RuntimeDirectory $InstallDirectory -FixtureDirectory $FixtureDirectory

    $Uninstaller = Get-ChildItem -LiteralPath $InstallDirectory -File -Filter "uninstall*.exe" | Select-Object -First 1
    if (-not $Uninstaller) { throw "NSIS uninstaller is missing" }
    $Uninstall = Start-Process -FilePath $Uninstaller.FullName -ArgumentList "/S" -Wait -PassThru
    if ($Uninstall.ExitCode -ne 0) { throw "NSIS uninstaller exited with $($Uninstall.ExitCode)" }
    Start-Sleep -Seconds 2
    if (Test-Path -LiteralPath $App) { throw "Application binary remains after uninstall" }
    if ((Test-Path -LiteralPath $InstallDirectory) -and (Get-ChildItem -LiteralPath $InstallDirectory -Recurse -Force | Select-Object -First 1)) {
        throw "Installation files remain after uninstall: $InstallDirectory"
    }
    if (-not (Test-Path -LiteralPath $Sentinel -PathType Leaf)) { throw "Uninstall removed user data" }
    Write-Host "Per-user NSIS signed runtime, worker PDF/OCR, and install/uninstall smoke passed."
}
finally {
    if (Test-Path -LiteralPath $InstallDirectory) {
        Write-Warning "Installer smoke directory remains for inspection: $InstallDirectory"
    }
}
