[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$InstallerPath,
    [string]$InstallDirectory = (Join-Path $env:LOCALAPPDATA "Intern"),
    [string]$FixtureDirectory = (Join-Path (Resolve-Path (Join-Path $PSScriptRoot "..")).Path "fixtures/generated"),
    [string]$EvidencePath,
    [string]$Commit = $env:GITHUB_SHA,
    [string]$Workflow = $env:GITHUB_WORKFLOW,
    [string]$RunId = $env:GITHUB_RUN_ID,
    [string]$RunAttempt = $env:GITHUB_RUN_ATTEMPT
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$Installer = (Resolve-Path -LiteralPath $InstallerPath).Path
if ([System.IO.Path]::GetExtension($Installer) -ne ".exe") { throw "Installer must be an NSIS executable" }
$InstallerSha256 = (Get-FileHash -LiteralPath $Installer -Algorithm SHA256).Hash.ToLowerInvariant()
if ($EvidencePath -and (@($Commit, $Workflow, $RunId, $RunAttempt) | Where-Object { [string]::IsNullOrWhiteSpace($_) })) {
    throw "Evidence output requires commit, workflow, run id, and run attempt"
}
$UserDataDirectory = Join-Path $env:LOCALAPPDATA "com.intern.app"
$Sentinel = Join-Path $UserDataDirectory "installer-smoke-user-data.txt"
New-Item -ItemType Directory -Path $UserDataDirectory -Force | Out-Null
Set-Content -LiteralPath $Sentinel -Value "must survive uninstall" -Encoding utf8NoBOM

if (Test-Path -LiteralPath $InstallDirectory) { throw "Smoke install target already exists: $InstallDirectory" }
$AppProcess = $null
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

    $AppProcess = Start-Process -FilePath $App -PassThru
    $WindowReady = $false
    for ($Attempt = 0; $Attempt -lt 60; $Attempt += 1) {
        Start-Sleep -Milliseconds 500
        $AppProcess.Refresh()
        if ($AppProcess.HasExited) { throw "Installed Intern.exe exited before its window became ready" }
        if ($AppProcess.MainWindowHandle -ne 0) {
            $WindowReady = $true
            break
        }
    }
    if (-not $WindowReady) { throw "Installed Intern.exe did not create a main window" }
    if (-not $AppProcess.CloseMainWindow()) { throw "Installed Intern.exe rejected a normal window close request" }
    # A WebView2 app on a shared CI runner can take well over fifteen seconds to
    # tear its browser process down, and a timeout here reads as "the app hangs on
    # close" when the truth is "the runner was busy". Sixty seconds still fails a
    # genuine hang, and a slow-but-clean shutdown costs only the time it needs.
    if (-not $AppProcess.WaitForExit(60000)) {
        # Say what is still alive. Without this the failure names no process and
        # gives no way to tell a hung app from a hung child.
        $Surviving = @(Get-Process -Name "intern", "intern-worker", "llama-server", "msedgewebview2" -ErrorAction SilentlyContinue |
            ForEach-Object { "$($_.Name)#$($_.Id)" })
        $Children = @(Get-CimInstance Win32_Process -Filter "ParentProcessId = $($AppProcess.Id)" -ErrorAction SilentlyContinue |
            ForEach-Object { "$($_.Name)#$($_.ProcessId)" })
        throw ("Installed Intern.exe did not shut down cleanly within 60s. " +
            "Main process HasExited=$($AppProcess.HasExited). " +
            "Children: $($Children -join ', '). Live Intern processes: $($Surviving -join ', ')")
    }
    if ($AppProcess.ExitCode -ne 0) { throw "Installed Intern.exe exited with $($AppProcess.ExitCode)" }

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
    if ($EvidencePath) {
        $EvidenceDirectory = Split-Path -Parent $EvidencePath
        if ($EvidenceDirectory) { New-Item -ItemType Directory -Path $EvidenceDirectory -Force | Out-Null }
        [ordered]@{
            schema_version = 1
            status = "accepted"
            commit = $Commit
            workflow = $Workflow
            run_id = $RunId
            run_attempt = $RunAttempt
            installer_sha256 = $InstallerSha256
            checks = [ordered]@{
                app_launched = $true
                clean_shutdown = $true
                runtime_inventory_verified = $true
                installed_worker_core_path = $true
                uninstall_succeeded = $true
                user_data_retained = $true
            }
        } | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $EvidencePath -Encoding utf8NoBOM
    }
    Write-Host "Per-user NSIS app launch, clean shutdown, signed runtime, worker PDF/OCR, and install/uninstall smoke passed."
}
finally {
    if ($AppProcess -and -not $AppProcess.HasExited) {
        Stop-Process -Id $AppProcess.Id -Force -ErrorAction SilentlyContinue
    }
    if (Test-Path -LiteralPath $InstallDirectory) {
        Write-Warning "Installer smoke directory remains for inspection: $InstallDirectory"
    }
}
