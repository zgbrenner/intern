<#
.SYNOPSIS
    Scores the whole gold corpus through the shipping document-understanding
    pipeline with the pinned local model, on this machine, with real inference.

.DESCRIPTION
    Downloads and verifies the exact model named by the embedded manifest,
    starts the packaged llama.cpp server text-only on a loopback port with a
    random key, runs every generated fixture through `intern-evaluate`, and
    records accuracy, review rate, latency, and peak model memory.

    Nothing leaves the machine except the pinned model download.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$RuntimeDirectory,
    [Parameter(Mandatory = $true)][string]$WorkerPath,
    [string]$EvaluatePath = (Join-Path (Resolve-Path (Join-Path $PSScriptRoot "..")).Path "target/release/intern-evaluate.exe"),
    [string]$FixtureDirectory = (Join-Path (Resolve-Path (Join-Path $PSScriptRoot "..")).Path "fixtures/generated"),
    [string]$OutputPath = (Join-Path (Resolve-Path (Join-Path $PSScriptRoot "..")).Path "docs/qa/model-evaluation.json"),
    [string]$ModelDirectory = (Join-Path $env:TEMP "intern-model-evaluation"),
    [ValidateSet("new", "legacy")][string]$Pipeline = "new",
    [int]$Threads = 0
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$Repository = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$Runtime = (Resolve-Path -LiteralPath $RuntimeDirectory).Path
$Worker = (Resolve-Path -LiteralPath $WorkerPath).Path
$Evaluate = (Resolve-Path -LiteralPath $EvaluatePath).Path
$Fixtures = (Resolve-Path -LiteralPath $FixtureDirectory).Path
$ExpectedPath = Join-Path $Repository "fixtures/expected.json"
$Manifest = Get-Content -LiteralPath (Join-Path $Repository "src-tauri/resources/model-manifest.json") -Raw | ConvertFrom-Json
$ModelSpec = $Manifest.files | Where-Object { $_.role -eq "model" } | Select-Object -First 1
if (-not $ModelSpec) { throw "The embedded manifest names no text model" }
if ($Threads -le 0) {
    $Logical = [Environment]::ProcessorCount
    $Threads = [Math]::Min(12, [Math]::Max(2, [int]($Logical / 2)))
}

function Get-PinnedModel {
    param([Parameter(Mandatory = $true)]$Spec, [Parameter(Mandatory = $true)][string]$Directory)
    New-Item -ItemType Directory -Force -Path $Directory | Out-Null
    $Destination = Join-Path $Directory ([string]$Spec.name)
    if (-not (Test-Path -LiteralPath $Destination -PathType Leaf)) {
        # A multi-gigabyte download over a long job: retry transient failures
        # with backoff instead of failing the whole run on one hiccup.
        $Attempts = 0
        while ($true) {
            $Attempts += 1
            try {
                Invoke-WebRequest -Uri ([string]$Spec.url) -OutFile $Destination -MaximumRedirection 5
                break
            } catch {
                if (Test-Path -LiteralPath $Destination) { Remove-Item -LiteralPath $Destination -Force }
                if ($Attempts -ge 4) { throw }
                Start-Sleep -Seconds (15 * $Attempts)
            }
        }
    }
    $File = Get-Item -LiteralPath $Destination
    if ($File.Length -ne [long]$Spec.size) { throw "Pinned model size mismatch: $($Spec.name)" }
    $Digest = (Get-FileHash -LiteralPath $Destination -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($Digest -ne [string]$Spec.sha256) { throw "Pinned model digest mismatch: $($Spec.name)" }
    return $Destination
}

function Get-FreePort {
    $Listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, 0)
    $Listener.Start()
    $Port = $Listener.LocalEndpoint.Port
    $Listener.Stop()
    return $Port
}

$ModelPath = Get-PinnedModel -Spec $ModelSpec -Directory $ModelDirectory
$Server = Join-Path $Runtime "llama-server.exe"
if (-not (Test-Path -LiteralPath $Server -PathType Leaf)) { throw "Staged runtime has no llama-server.exe" }

$Port = Get-FreePort
$ApiKey = -join ((1..32) | ForEach-Object { "{0:x2}" -f (Get-Random -Minimum 0 -Maximum 256) })
$Arguments = @(
    "--host", "127.0.0.1", "--port", "$Port", "--api-key", $ApiKey,
    "--model", $ModelPath, "--parallel", "1", "--ctx-size", "8192",
    "--n-gpu-layers", "0", "--threads", "$Threads", "--threads-batch", "$Threads",
    "--jinja", "--no-webui", "--no-mmproj"
)
$ServerLog = Join-Path $ModelDirectory "llama-server.log"
$Process = Start-Process -FilePath $Server -ArgumentList $Arguments -PassThru -NoNewWindow -RedirectStandardOutput $ServerLog -RedirectStandardError "$ServerLog.err"

try {
    $Deadline = (Get-Date).AddMinutes(10)
    $Ready = $false
    while ((Get-Date) -lt $Deadline) {
        if ($Process.HasExited) { throw "Local model server exited with $($Process.ExitCode)" }
        try {
            $Health = Invoke-WebRequest -Uri "http://127.0.0.1:$Port/health" -Headers @{ Authorization = "Bearer $ApiKey" } -UseBasicParsing -TimeoutSec 2
            if ($Health.StatusCode -eq 200) { $Ready = $true; break }
        } catch { Start-Sleep -Milliseconds 400 }
    }
    if (-not $Ready) { throw "Local model server did not become healthy" }

    $Monitor = Start-Job -ScriptBlock {
        param($ProcessId)
        $Peak = 0
        while ($true) {
            try { $Current = Get-Process -Id $ProcessId -ErrorAction Stop } catch { break }
            if ($Current.WorkingSet64 -gt $Peak) { $Peak = $Current.WorkingSet64 }
            Start-Sleep -Milliseconds 250
        }
        $Peak
    } -ArgumentList $Process.Id

    $Started = Get-Date
    $Raw = & $Evaluate `
        --fixtures $Fixtures `
        --expected $ExpectedPath `
        --worker $Worker `
        --endpoint "http://127.0.0.1:$Port/v1/chat/completions" `
        --api-key $ApiKey `
        --model-id ([string]$Manifest.served_model_name) `
        --pipeline $Pipeline
    if ($LASTEXITCODE -ne 0) { throw "Corpus evaluation failed" }
    $Elapsed = ((Get-Date) - $Started).TotalSeconds
} finally {
    if (-not $Process.HasExited) { Stop-Process -Id $Process.Id -Force }
}

$Peak = 0
if (Get-Variable -Name Monitor -ErrorAction SilentlyContinue) {
    $Peak = Receive-Job $Monitor -Wait -AutoRemoveJob
}

$Report = $Raw | ConvertFrom-Json
# Bind the evidence to the exact source tree and workflow run that produced it,
# so a release can never be gated on numbers from a different commit.
$Report | Add-Member -NotePropertyName "status" -NotePropertyValue "completed" -Force
$Report | Add-Member -NotePropertyName "commit" -NotePropertyValue ((git -C $Repository rev-parse HEAD).Trim()) -Force
$Report | Add-Member -NotePropertyName "release_inputs_sha256" -NotePropertyValue ((node (Join-Path $Repository "scripts/hash-release-inputs.mjs") "--root=$Repository").Trim()) -Force
$Report | Add-Member -NotePropertyName "runner" -NotePropertyValue ([ordered]@{
        os = "$([Environment]::OSVersion.VersionString)"
        logical_processors = [Environment]::ProcessorCount
        ci_run_id = $env:GITHUB_RUN_ID
        ci_run_attempt = $env:GITHUB_RUN_ATTEMPT
    }) -Force
$RawPath = Join-Path $ModelDirectory "raw-report.json"
Set-Content -LiteralPath $RawPath -Value $Raw -Encoding utf8NoBOM
$Acceptance = & node (Join-Path $Repository "scripts/validate-model-evaluation.mjs") $RawPath | ConvertFrom-Json
$Report | Add-Member -NotePropertyName "acceptance" -NotePropertyValue $Acceptance -Force
$Report | Add-Member -NotePropertyName "model_file" -NotePropertyValue ([string]$ModelSpec.name) -Force
$Report | Add-Member -NotePropertyName "model_sha256" -NotePropertyValue ([string]$ModelSpec.sha256) -Force
$Report | Add-Member -NotePropertyName "model_bytes" -NotePropertyValue ([long]$ModelSpec.size) -Force
$Report | Add-Member -NotePropertyName "threads" -NotePropertyValue $Threads -Force
$Report | Add-Member -NotePropertyName "peak_model_rss_bytes" -NotePropertyValue ([long]$Peak) -Force
$Report | Add-Member -NotePropertyName "wall_clock_seconds" -NotePropertyValue ([math]::Round($Elapsed, 1)) -Force

New-Item -ItemType Directory -Force -Path (Split-Path -Parent $OutputPath) | Out-Null
$Report | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath $OutputPath -Encoding utf8NoBOM
Write-Output "Wrote ${OutputPath}: $($Report.summary.evaluated) documents, peak model RSS $([math]::Round($Peak / 1MB)) MB, $([math]::Round($Elapsed, 1)) s"
