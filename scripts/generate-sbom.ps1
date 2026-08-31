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
    # Out-Host, not bare invocation: a native command's stdout goes to
    # PowerShell's success stream, so every line the SBOM tool printed became
    # part of this function's return value. The caller received an array whose
    # first element was the tool's leading blank line rather than the manifest
    # path, and the release died on
    #   Cannot bind argument to parameter 'LiteralPath' because it is an empty string.
    # Out-Host keeps the tool's output in the log while leaving the pipeline
    # clean, so `return $Manifest` returns only the manifest.
    & $SbomTool @Arguments | Out-Host
    if ($LASTEXITCODE -ne 0) { throw "Pinned SBOM generation failed for $Name" }
    $Manifest = Join-Path $Drop "_manifest/spdx_2.2/manifest.spdx.json"
    if (-not (Test-Path -LiteralPath $Manifest -PathType Leaf)) { throw "SBOM tool did not emit an SPDX 2.2 document for $Name" }
    $Validation = Join-Path $Drop "sbom-validation.json"
    & $SbomTool validate -b $Drop -o $Validation -mi "SPDX:2.2" | Out-Host
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
        $Published = Join-Path $Release "Intern-v0.1.0-alpha.4-runtime-$SafeName.spdx.json"
        Copy-Item -LiteralPath $Generated -Destination $Published
        $External = Get-Content -LiteralPath $Published -Raw | ConvertFrom-Json
        if (-not (@($External.packages).name -contains $Package.Name)) { throw "Runtime SBOM omits package identity $($Package.Name)" }
        $ExternalDocuments.Add($Published)
    }

    $ExternalList = Join-Path $Work "external-sboms.txt"
    $ExternalDocuments | Set-Content -LiteralPath $ExternalList -Encoding utf8NoBOM
    $MainManifest = Invoke-SbomGenerate -Drop $Release -Components $RepositoryRoot -Name "Intern" -Version "0.1.0-alpha.4" -ExternalList $ExternalList
    $Main = Get-Content -LiteralPath $MainManifest -Raw | ConvertFrom-Json
    $PackageNames = @($Main.packages | ForEach-Object name)
    # Prove each ecosystem is actually covered, using packages the SBOM tool
    # really emits.
    #
    # This used to require "intern-worker", which the tool never emits and
    # cannot: it reports dependencies, and a workspace member is a local path
    # package it treats as part of the root rather than as a component. The
    # check failed on a build whose SBOM was complete - 560 Rust components and
    # 122 npm components detected - because it was asserting the absence of a
    # design decision in someone else's tool.
    #
    # "Intern" is the root document and "intern" is the npm root; the failing
    # build proved both are emitted, because the old check tested them first and
    # got past them. Ecosystem coverage is asserted through package URLs rather
    # than any particular dependency name, so this cannot break again when a
    # single crate or npm package is added or dropped.
    foreach ($RequiredPackage in @("Intern", "intern")) {
        if ($RequiredPackage -notin $PackageNames) {
            throw ("Application SBOM omits $RequiredPackage. It lists $($PackageNames.Count) packages; " +
                "the first few are: $(($PackageNames | Select-Object -First 8) -join ', ')")
        }
    }
    $Purls = @($Main.packages | ForEach-Object { $_.externalRefs } | Where-Object { $_.referenceType -eq "purl" } | ForEach-Object { [string]$_.referenceLocator })
    foreach ($Ecosystem in @("cargo", "npm")) {
        $Matched = @($Purls | Where-Object { $_.StartsWith("pkg:$Ecosystem/") }).Count
        if ($Matched -eq 0) {
            throw ("Application SBOM contains no pkg:$Ecosystem package URLs, so it does not cover that dependency tree. " +
                "It lists $($PackageNames.Count) packages and $($Purls.Count) package URLs; a sample: $(($Purls | Select-Object -First 5) -join ', ')")
        }
        Write-Host "Application SBOM covers $Matched $Ecosystem packages."
    }
    if (@($Main.externalDocumentRefs).Count -ne $Packages.Count) {
        throw "Application SBOM does not reference every native runtime component SBOM"
    }
    Copy-Item -LiteralPath $MainManifest -Destination (Join-Path $Release "Intern-v0.1.0-alpha.4.spdx.json")
    Remove-Item -LiteralPath (Join-Path $Release "_manifest") -Recurse -Force
    Remove-Item -LiteralPath (Join-Path $Release "sbom-validation.json") -Force
    Write-Host "Pinned Microsoft SBOM Tool generated and validated app, npm, Cargo, and $($Packages.Count) actual runtime package documents."
}
finally {
    if (Test-Path -LiteralPath $Work) { Remove-Item -LiteralPath $Work -Recurse -Force }
}
