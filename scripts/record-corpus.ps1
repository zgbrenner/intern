<#
.SYNOPSIS
Records the gold corpus against the packaged runtime and the local model, and
writes the replay baseline CI holds every push to.

.DESCRIPTION
Runs intern-evaluate live - the staged worker, PDFium, Tesseract, and a
llama-server started here with the same flags the desktop app uses - over
fixtures/generated, and writes fixtures/corpus-recording.json (what the worker
read and what the model replied, keyed by prompt hash) and
fixtures/corpus-baseline.json (the scores that recording earns). Commit both
with the change that made the recording stale. See docs/evaluation.md.

.PARAMETER RuntimeDirectory
A package-shaped runtime: intern-worker.exe, llama-server.exe, pdfium.dll,
tesseract.exe, and tessdata\. Produce one with scripts/stage-windows-runtime.ps1
after scripts/fetch-windows-assets.ps1.

.PARAMETER ModelPath
The GGUF file named in src-tauri/resources/model-manifest.json, as the app
downloaded it.

.EXAMPLE
./scripts/record-corpus.ps1 -RuntimeDirectory C:\intern-stage -ModelPath "$env:LOCALAPPDATA\Intern\models\Qwen3.5-2B-Q4_K_M.gguf"
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$RuntimeDirectory,
    [Parameter(Mandatory = $true)][string]$ModelPath,
    [int]$Port = 8090,
    [int]$Threads = [Math]::Max(1, [Environment]::ProcessorCount - 1),
    [string]$Note = ""
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$RepositoryRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$Runtime = (Resolve-Path -LiteralPath $RuntimeDirectory).Path
$Model = (Resolve-Path -LiteralPath $ModelPath).Path
$Fixtures = Join-Path $RepositoryRoot "fixtures/generated"
if (-not (Test-Path -LiteralPath $Fixtures -PathType Container)) {
    throw "fixtures/generated is missing; run 'npm run fixtures' first"
}
foreach ($Required in @("intern-worker.exe", "llama-server.exe", "pdfium.dll", "tesseract.exe", "tessdata/eng.traineddata")) {
    if (-not (Test-Path -LiteralPath (Join-Path $Runtime $Required) -PathType Leaf)) { throw "Runtime is missing $Required" }
}

$Manifest = Get-Content -LiteralPath (Join-Path $RepositoryRoot "src-tauri/resources/model-manifest.json") -Raw | ConvertFrom-Json
$ModelHash = (Get-FileHash -LiteralPath $Model -Algorithm SHA256).Hash.ToLowerInvariant()
$Pinned = @($Manifest.files | Where-Object { $_.sha256 -eq $ModelHash })
if ($Pinned.Count -eq 0) {
    throw "The model at $Model is not the one model-manifest.json pins; a recording of another model would be scored as if it were the shipped one"
}
$RuntimeAssets = Get-Content -LiteralPath (Join-Path $RepositoryRoot "src-tauri/resources/runtime-assets.json") -Raw | ConvertFrom-Json
$LlamaVersion = @($RuntimeAssets.downloads | Where-Object { $_.id -eq "llama.cpp" } | ForEach-Object { $_.version })
$Description = "$($Pinned[0].name) (manifest sha256 $($ModelHash.Substring(0, 8))); llama.cpp $($LlamaVersion -join ',') on Windows, $Threads threads, 8192 context; packaged worker, PDFium, and Tesseract"
if ($Note) { $Description = "$Description; $Note" }

$ApiKey = [Guid]::NewGuid().ToString("N")
# The same flags src/server.rs passes: one slot, CPU only, the model's own
# chat template so the no-thinking switch reaches it, no web UI, no projector.
$ServerArguments = @(
    "--host", "127.0.0.1", "--port", "$Port", "--api-key", $ApiKey, "--model", $Model,
    "--parallel", "1", "--ctx-size", "8192", "--n-gpu-layers", "0",
    "--threads", "$Threads", "--threads-batch", "$Threads", "--jinja", "--no-webui", "--no-mmproj"
)
$Server = Start-Process -FilePath (Join-Path $Runtime "llama-server.exe") -ArgumentList $ServerArguments -PassThru -WindowStyle Hidden
try {
    $Deadline = (Get-Date).AddMinutes(3)
    do {
        Start-Sleep -Seconds 2
        try { $Health = Invoke-RestMethod -Uri "http://127.0.0.1:$Port/health" -TimeoutSec 5 } catch { $Health = $null }
    } while (($null -eq $Health -or $Health.status -ne "ok") -and (Get-Date) -lt $Deadline)
    if ($null -eq $Health -or $Health.status -ne "ok") { throw "llama-server did not become healthy on port $Port" }

    $env:INTERN_RUNTIME_DIR = $Runtime
    & cargo run --locked -p intern-engine --bin intern-evaluate -- `
        --fixtures $Fixtures --expected (Join-Path $RepositoryRoot "fixtures/expected.json") `
        --worker (Join-Path $Runtime "intern-worker.exe") `
        --endpoint "http://127.0.0.1:$Port/v1/chat/completions" --api-key $ApiKey --model-id intern-local `
        --record (Join-Path $RepositoryRoot "fixtures/corpus-recording.json") `
        --write-baseline (Join-Path $RepositoryRoot "fixtures/corpus-baseline.json") `
        --output (Join-Path $RepositoryRoot "corpus-report.json") `
        --note $Description
    if ($LASTEXITCODE -ne 0) { throw "intern-evaluate exited with $LASTEXITCODE" }
    Write-Host "Recorded fixtures/corpus-recording.json and fixtures/corpus-baseline.json; the full report is corpus-report.json"
}
finally {
    if (-not $Server.HasExited) { Stop-Process -Id $Server.Id -Force }
}
