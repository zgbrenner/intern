[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$ReleaseDirectory,
    [Parameter(Mandatory = $true)][string]$RuntimeStage
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$RepositoryRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$Release = (Resolve-Path -LiteralPath $ReleaseDirectory).Path
$Runtime = (Resolve-Path -LiteralPath $RuntimeStage).Path
if (-not (Test-Path -LiteralPath (Join-Path $RepositoryRoot "Cargo.lock") -PathType Leaf)) { throw "Cargo.lock is required for the release SBOM" }
$RuntimeManifest = Get-Content -LiteralPath (Join-Path $Runtime "resources/runtime-assets.json") -Raw | ConvertFrom-Json
if ($RuntimeManifest.bundled_files.Count -eq 0) { throw "Runtime inventory is empty" }

$Work = Join-Path ([IO.Path]::GetTempPath()) ("intern-sbom-" + [guid]::NewGuid().ToString("N"))
$ToolDirectory = Join-Path $Work "tool"
New-Item -ItemType Directory -Path $ToolDirectory | Out-Null
& dotnet tool install --tool-path $ToolDirectory Microsoft.Sbom.DotNetTool --version 4.1.5
if ($LASTEXITCODE -ne 0) { throw "Failed to install pinned Microsoft SBOM Tool 4.1.5" }
$SbomTool = Join-Path $ToolDirectory "sbom-tool.exe"
if (-not (Test-Path -LiteralPath $SbomTool -PathType Leaf)) { $SbomTool = Join-Path $ToolDirectory "sbom-tool" }
if (-not (Test-Path -LiteralPath $SbomTool -PathType Leaf)) { throw "Pinned SBOM generator executable is missing" }

function Invoke-SbomGenerate {
    param([string]$Drop, [string]$Components, [string]$Name, [string]$Version, [string]$ExternalList = "")
    $Arguments = @("generate", "-b", $Drop, "-bc", $Components, "-pn", $Name, "-pv", $Version, "-ps", "Intern contributors", "-nsb", "https://github.com/$env:GITHUB_REPOSITORY/sbom", "-mi", "SPDX:2.2", "-D", "true")
    if ($ExternalList) { $Arguments += @("-er", $ExternalList) }
    & $SbomTool @Arguments
    if ($LASTEXITCODE -ne 0) { throw "Pinned SBOM generation failed for $Name" }
    $Manifest = Join-Path $Drop "_manifest/spdx_2.2/manifest.spdx.json"
    if (-not (Test-Path -LiteralPath $Manifest -PathType Leaf)) { throw "SBOM tool did not emit an SPDX 2.2 document for $Name" }
    $Validation = Join-Path $Drop "sbom-validation.json"
    & $SbomTool validate -b $Drop -o $Validation -mi "SPDX:2.2"
    if ($LASTEXITCODE -ne 0) { throw "Pinned SBOM validation failed for $Name" }
    return $Manifest
}

try {
    $ExternalDocuments = [System.Collections.Generic.List[string]]::new()
    $PackageGroups = @{}
    foreach ($Entry in $RuntimeManifest.bundled_files) {
        if (-not $Entry.packages -or $Entry.packages.Count -eq 0) { throw "Runtime file lacks package ownership: $($Entry.install_path)" }
        foreach ($Package in $Entry.packages) {
            $Key = "$($Package.name)`n$($Package.version)"
            if (-not $PackageGroups.ContainsKey($Key)) {
                $PackageGroups[$Key] = [pscustomobject]@{ Name = [string]$Package.name; Version = [string]$Package.version; Entries = [System.Collections.Generic.List[object]]::new() }
            }
            $PackageGroups[$Key].Entries.Add($Entry)
        }
    }
    $Packages = @($PackageGroups.Values | Sort-Object Name, Version)
    if (-not ($Packages.Name -contains "tesseract") -or -not ($Packages.Name -contains "leptonica")) {
        throw "Actual vcpkg Tesseract and Leptonica package ownership is missing from the runtime inventory"
    }
    if ($Packages.Name -contains "Tesseract/vcpkg") { throw "Synthetic Tesseract/vcpkg grouping is forbidden" }
    foreach ($Package in $Packages) {
        $SafeName = "$($Package.Name)-$($Package.Version)" -replace '[^A-Za-z0-9_.-]', '-'
        $Drop = Join-Path $Work "runtime-$SafeName"
        New-Item -ItemType Directory -Path $Drop | Out-Null
        foreach ($Entry in $Package.Entries) {
            $Source = Join-Path $Runtime ([string]$Entry.install_path)
            $Target = Join-Path $Drop ([string]$Entry.install_path)
            New-Item -ItemType Directory -Path (Split-Path -Parent $Target) -Force | Out-Null
            Copy-Item -LiteralPath $Source -Destination $Target
        }
        $Generated = Invoke-SbomGenerate -Drop $Drop -Components $Drop -Name $Package.Name -Version $Package.Version
        $Published = Join-Path $Release "Intern-v0.1.0-alpha.1-runtime-$SafeName.spdx.json"
        Copy-Item -LiteralPath $Generated -Destination $Published
        $External = Get-Content -LiteralPath $Published -Raw | ConvertFrom-Json
        if (-not (@($External.packages).name -contains $Package.Name)) { throw "Runtime SBOM omits package identity $($Package.Name)" }
        $ExternalDocuments.Add($Published)
    }

    $ExternalList = Join-Path $Work "external-sboms.txt"
    $ExternalDocuments | Set-Content -LiteralPath $ExternalList -Encoding utf8NoBOM
    $MainManifest = Invoke-SbomGenerate -Drop $Release -Components $RepositoryRoot -Name "Intern" -Version "0.1.0-alpha.1" -ExternalList $ExternalList
    $Main = Get-Content -LiteralPath $MainManifest -Raw | ConvertFrom-Json
    $PackageNames = @($Main.packages | ForEach-Object name)
    foreach ($RequiredPackage in @("Intern", "intern", "intern-worker")) {
        if ($RequiredPackage -notin $PackageNames) { throw "Application SBOM omits required app/npm/Cargo package: $RequiredPackage" }
    }
    if (@($Main.externalDocumentRefs).Count -ne $Packages.Count) {
        throw "Application SBOM does not reference every native runtime component SBOM"
    }
    Copy-Item -LiteralPath $MainManifest -Destination (Join-Path $Release "Intern-v0.1.0-alpha.1.spdx.json")
    Remove-Item -LiteralPath (Join-Path $Release "_manifest") -Recurse -Force
    Remove-Item -LiteralPath (Join-Path $Release "sbom-validation.json") -Force
    Write-Host "Pinned Microsoft SBOM Tool generated and validated app, npm, Cargo, and $($Packages.Count) actual runtime package documents."
}
finally {
    if (Test-Path -LiteralPath $Work) { Remove-Item -LiteralPath $Work -Recurse -Force }
}
