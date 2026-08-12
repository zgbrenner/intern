[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$WorkerPath,
    [Parameter(Mandatory = $true)][string]$RuntimeDirectory,
    [string]$FixtureDirectory = (Join-Path (Resolve-Path (Join-Path $PSScriptRoot "..")).Path "fixtures/generated")
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$Worker = (Resolve-Path -LiteralPath $WorkerPath).Path
$Runtime = (Resolve-Path -LiteralPath $RuntimeDirectory).Path
$Fixtures = (Resolve-Path -LiteralPath $FixtureDirectory).Path

foreach ($Required in @(
    "pdfium.dll",
    "tesseract.exe",
    "tessdata/eng.traineddata",
    "tessdata/osd.traineddata"
)) {
    $RequiredPath = Join-Path $Runtime $Required
    if (-not (Test-Path -LiteralPath $RequiredPath -PathType Leaf)) {
        throw "Package-shaped runtime is missing $Required"
    }
}

$Start = [System.Diagnostics.ProcessStartInfo]::new()
$Start.FileName = $Worker
$Start.WorkingDirectory = $Runtime
$Start.UseShellExecute = $false
$Start.CreateNoWindow = $true
$Start.RedirectStandardInput = $true
$Start.RedirectStandardOutput = $true
$Start.RedirectStandardError = $true
$Start.Environment["INTERN_RUNTIME_DIR"] = $Runtime
$Process = [System.Diagnostics.Process]::new()
$Process.StartInfo = $Start
$Stderr = [System.Text.StringBuilder]::new()
$Process.add_ErrorDataReceived({
    param($Sender, $Event)
    if ($null -ne $Event.Data) { [void]$Stderr.AppendLine($Event.Data) }
})
if (-not $Process.Start()) { throw "Failed to launch parser worker" }
$Process.BeginErrorReadLine()

function Invoke-WorkerCommand {
    param(
        [Parameter(Mandatory = $true)][hashtable]$Command,
        [Parameter(Mandatory = $true)][string]$RequestId
    )
    $Envelope = [ordered]@{
        protocol_version = 1
        request_id = $RequestId
        command = $Command
    }
    $Process.StandardInput.WriteLine(($Envelope | ConvertTo-Json -Compress -Depth 8))
    $Process.StandardInput.Flush()

    $Deadline = [DateTime]::UtcNow.AddMinutes(5)
    while ([DateTime]::UtcNow -lt $Deadline) {
        $Read = $Process.StandardOutput.ReadLineAsync()
        while (-not $Read.Wait(5000)) {
            if ($Process.HasExited) {
                throw "Worker exited with $($Process.ExitCode): $Stderr"
            }
            if ([DateTime]::UtcNow -ge $Deadline) { throw "Timed out waiting for worker request $RequestId. stderr: $Stderr" }
        }
        $Line = $Read.Result
        if ($null -eq $Line) { throw "Worker closed stdout: $Stderr" }
        if ([string]::IsNullOrWhiteSpace($Line)) { continue }
        $Envelope = $Line | ConvertFrom-Json
        if ($Envelope.protocol_version -ne 1) { throw "Worker emitted unsupported protocol: $Line" }
        if ($Envelope.request_id -ne $RequestId) { throw "Worker interleaved an unexpected request: $Line" }
        if ($Envelope.event.type -in @("hello", "parsed", "error")) { return $Envelope.event }
    }
    throw "Timed out waiting for worker request $RequestId. stderr: $Stderr"
}

function Assert-ParsedFixture {
    param(
        [Parameter(Mandatory = $true)][string]$File,
        [Parameter(Mandatory = $true)][string[]]$Facts,
        [Parameter(Mandatory = $true)][string[]]$Sources
    )
    $Path = Join-Path $Fixtures $File
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { throw "Fixture is missing: $File" }
    $Id = "parse-" + [IO.Path]::GetFileNameWithoutExtension($File)
    $Result = Invoke-WorkerCommand -RequestId $Id -Command @{ type = "parse"; path = $Path }
    if ($Result.type -ne "parsed") {
        throw "Expected $File to parse, got $($Result.type): $($Result | ConvertTo-Json -Compress -Depth 8)"
    }
    $Text = (($Result.document.pages | ForEach-Object text) -join "`n").ToLowerInvariant()
    foreach ($Fact in $Facts) {
        if (-not $Text.Contains($Fact.ToLowerInvariant())) { throw "$File is missing extracted fact '$Fact': $Text" }
    }
    $ActualSources = @($Result.document.pages | ForEach-Object source | Select-Object -Unique)
    foreach ($Source in $Sources) {
        if ($Source -notin $ActualSources) { throw "$File did not exercise $Source routing; got $($ActualSources -join ', ')" }
    }
}

function Assert-RejectedFixture {
    param([Parameter(Mandatory = $true)][string]$File, [string]$Code = "PARSE_FAILED")
    $Path = Join-Path $Fixtures $File
    $Result = Invoke-WorkerCommand -RequestId ("reject-" + [IO.Path]::GetFileNameWithoutExtension($File)) -Command @{ type = "parse"; path = $Path }
    if ($Result.type -ne "error" -or $Result.code -ne $Code) {
        throw "Expected $File to fail with ${Code}: $($Result | ConvertTo-Json -Compress -Depth 8)"
    }
}

try {
    $Hello = Invoke-WorkerCommand -RequestId "hello" -Command @{ type = "hello" }
    if ($Hello.type -ne "hello" -or $Hello.worker_version -ne "0.1.0-alpha.1") {
        throw "Worker hello did not report the release protocol: $($Hello | ConvertTo-Json -Compress -Depth 8)"
    }

    Assert-ParsedFixture "employment-agreement.pdf" @("Mira Vale", "February 14, 2025") @("native")
    Assert-ParsedFixture "scanned-lease.pdf" @("September 1, 2024", "47 Juniper Loop") @("ocr")
    Assert-ParsedFixture "rotated-low-resolution-scan.png" @("DR-771", "June 12, 2025") @("ocr")
    Assert-ParsedFixture "mixed-signature.pdf" @("Aurora Catalog Project", "January 8, 2025") @("native", "ocr")
    Assert-ParsedFixture "nda.docx" @("Project Marigold", "March 3, 2025") @("any_doc")
    Assert-ParsedFixture "document-image.jpg" @("Packing Slip", "PS-311") @("ocr")
    Assert-RejectedFixture "encrypted.pdf"
    Assert-RejectedFixture "malformed.pdf"

    $Shutdown = [ordered]@{ protocol_version = 1; request_id = "shutdown"; command = @{ type = "shutdown" } }
    $Process.StandardInput.WriteLine(($Shutdown | ConvertTo-Json -Compress -Depth 4))
    $Process.StandardInput.Flush()
    $Process.StandardInput.Close()
    if (-not $Process.WaitForExit(10000)) { throw "Worker did not exit after shutdown" }
    if ($Process.ExitCode -ne 0) { throw "Worker exited with $($Process.ExitCode): $Stderr" }
    Write-Host "Package-shaped worker hello, native PDF, PDFium+Tesseract OCR, DOCX/image, and invalid-fixture smoke passed."
}
finally {
    if (-not $Process.HasExited) { $Process.Kill($true) }
    $Process.Dispose()
}
