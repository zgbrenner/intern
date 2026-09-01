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
if (-not $Process.Start()) { throw "Failed to launch parser worker" }

# Read stderr with a Task rather than an ErrorDataReceived handler: that event
# fires on a threadpool thread with no PowerShell runspace attached, so the
# handler throws "There is no Runspace available to run scripts in this thread"
# and takes the whole process down with exit 82 the first time the worker writes
# a single diagnostic line.
$StderrTask = $Process.StandardError.ReadToEndAsync()

function Get-WorkerStderr {
    # The task only completes when the stream closes, which is process exit. While
    # the worker is alive there is nothing to report yet, and saying so beats
    # blocking a diagnostic path forever.
    if ($StderrTask.IsCompleted) { return $StderrTask.Result }
    return "<worker still running; stderr not flushed>"
}

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
                throw "Worker exited with $($Process.ExitCode): $(Get-WorkerStderr)"
            }
            if ([DateTime]::UtcNow -ge $Deadline) { throw "Timed out waiting for worker request $RequestId. stderr: $(Get-WorkerStderr)" }
        }
        $Line = $Read.Result
        if ($null -eq $Line) { throw "Worker closed stdout: $(Get-WorkerStderr)" }
        if ([string]::IsNullOrWhiteSpace($Line)) { continue }
        $Envelope = $Line | ConvertFrom-Json
        # Under Set-StrictMode, reading a missing property throws an opaque
        # error; check shape explicitly so protocol violations stay diagnosable.
        foreach ($Required in @("protocol_version", "request_id", "event")) {
            if (-not $Envelope.PSObject.Properties[$Required]) { throw "Worker emitted unsupported protocol: $Line" }
        }
        if ($Envelope.protocol_version -ne 1) { throw "Worker emitted unsupported protocol: $Line" }
        if ($Envelope.request_id -ne $RequestId) { throw "Worker interleaved an unexpected request: $Line" }
        if (-not $Envelope.event.PSObject.Properties["type"]) { throw "Worker emitted unsupported protocol: $Line" }
        if ($Envelope.event.type -in @("hello", "parsed", "error")) { return $Envelope.event }
    }
    throw "Timed out waiting for worker request $RequestId. stderr: $(Get-WorkerStderr)"
}

function Assert-ParsedFixture {
    param(
        [Parameter(Mandatory = $true)][string]$File,
        [Parameter(Mandatory = $true)][string[]]$Facts,
        [Parameter(Mandatory = $true)][string[]]$Sources,
        [double]$MinimumOcrConfidence = 0
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
    # A page read in the wrong orientation still "parses": same word count, plausible
    # shape, useless text. Only confidence separates that from a real reading, so a
    # fixture that exists to prove orientation handling asserts a floor on it.
    if ($MinimumOcrConfidence -gt 0) {
        $Confidences = @($Result.document.pages | Where-Object { $null -ne $_.ocr_confidence } | ForEach-Object ocr_confidence)
        if ($Confidences.Count -eq 0) { throw "$File reported no OCR confidence to check" }
        $Best = ($Confidences | Measure-Object -Maximum).Maximum
        if ($Best -lt $MinimumOcrConfidence) {
            throw "$File read at OCR confidence $Best, below the $MinimumOcrConfidence floor: $Text"
        }
    }
}

function Assert-RejectedFixture {
    param([Parameter(Mandatory = $true)][string]$File, [string]$Code = "PARSE_FAILED")
    $Path = Join-Path $Fixtures $File
    $Result = Invoke-WorkerCommand -RequestId ("reject-" + [IO.Path]::GetFileNameWithoutExtension($File)) -Command @{ type = "parse"; path = $Path }
    if ($Result.type -ne "error" -or $Result.code -ne $Code) {
        # ${Code}, not $Code: PowerShell reads "$Code:" as a scope-qualified
        # variable like $env:PATH, which is a parse error rather than a runtime
        # one - it kept this whole script from loading at all.
        throw "Expected $File to fail with ${Code}: $($Result | ConvertTo-Json -Compress -Depth 8)"
    }
}

try {
    $Hello = Invoke-WorkerCommand -RequestId "hello" -Command @{ type = "hello" }
    if ($Hello.type -ne "hello" -or $Hello.worker_version -ne "0.1.0-alpha.6") {
        throw "Worker hello did not report the release protocol: $($Hello | ConvertTo-Json -Compress -Depth 8)"
    }

    # Native text and AnyDoc extraction are exact, so those fixtures assert exact
    # dates and names. OCR of the clean-room bitmap font is not: measured output
    # includes "EFFECTIWE", "LEDAR" for CEDAR, and 2024 read as "24h24". Digits and
    # narrow glyphs are the least reliable, so the scanned fixtures assert the
    # multi-word alphabetic content that survives, and fixtures/README.md records
    # the fidelity this font actually achieves. Asserting prose that the generator
    # never rasterised - a comma the font has no glyph for, say - is how these
    # assertions came to be unsatisfiable in the first place.
    Assert-ParsedFixture "employment-agreement.pdf" @("Mira Vale", "February 14, 2025") @("native")
    Assert-ParsedFixture "scanned-lease.pdf" @("lease agreement", "september", "juniper loop") @("ocr")
    Assert-ParsedFixture "rotated-low-resolution-scan.png" @("delivery receipt", "june 12", "violet cartography studio") @("ocr") -MinimumOcrConfidence 60
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
    if ($Process.ExitCode -ne 0) { throw "Worker exited with $($Process.ExitCode): $(Get-WorkerStderr)" }
    Write-Host "Package-shaped worker hello, native PDF, PDFium+Tesseract OCR, DOCX/image, and invalid-fixture smoke passed."
}
finally {
    if (-not $Process.HasExited) { $Process.Kill($true) }
    $Process.Dispose()
}
