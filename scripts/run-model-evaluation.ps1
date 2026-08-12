[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$RuntimeDirectory,
    [Parameter(Mandatory = $true)][string]$WorkerPath,
    [string]$EvaluatorPath = (Join-Path (Resolve-Path (Join-Path $PSScriptRoot "..")).Path "target/release/intern-model-evaluator.exe"),
    [string]$FixtureDirectory = (Join-Path (Resolve-Path (Join-Path $PSScriptRoot "..")).Path "fixtures/generated"),
    [string]$OutputPath = (Join-Path (Resolve-Path (Join-Path $PSScriptRoot "..")).Path "docs/qa/model-evaluation.json")
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$Repository = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$Runtime = (Resolve-Path -LiteralPath $RuntimeDirectory).Path
$Worker = (Resolve-Path -LiteralPath $WorkerPath).Path
$Evaluator = (Resolve-Path -LiteralPath $EvaluatorPath).Path
$Fixtures = (Resolve-Path -LiteralPath $FixtureDirectory).Path
$ExpectedPath = Join-Path $Repository "fixtures/expected.json"
$ManifestPath = Join-Path $Repository "fixtures/manifest.json"
$PromptPath = Join-Path $Repository "src-tauri/src/model/prompt.rs"
$Projector = [ordered]@{
    name = "mmproj-Qwen2.5-VL-3B-Instruct-f16.gguf"
    url = "https://huggingface.co/ggml-org/Qwen2.5-VL-3B-Instruct-GGUF/resolve/main/mmproj-Qwen2.5-VL-3B-Instruct-f16.gguf"
    size = 1338428128L
    sha256 = "b9160fe9d814d1fadf68395677468534778b39ac33c2e7561b7b218626e60d5e"
}
$Models = [ordered]@{
    q4 = [ordered]@{
        model_id = "qwen2.5-vl-3b-instruct-q4-k-m"
        filename = "Qwen2.5-VL-3B-Instruct-Q4_K_M.gguf"
        url = "https://huggingface.co/ggml-org/Qwen2.5-VL-3B-Instruct-GGUF/resolve/main/Qwen2.5-VL-3B-Instruct-Q4_K_M.gguf"
        size = 1929901056L
        model_sha256 = "d02fe9b69ad8cadbbd228e387667af66612c44bed29ffc8eb1e7caf9ac486c12"
        projector_sha256 = $Projector.sha256
    }
    q8 = [ordered]@{
        model_id = "qwen2.5-vl-3b-instruct-q8-0"
        filename = "Qwen2.5-VL-3B-Instruct-Q8_0.gguf"
        url = "https://huggingface.co/ggml-org/Qwen2.5-VL-3B-Instruct-GGUF/resolve/main/Qwen2.5-VL-3B-Instruct-Q8_0.gguf"
        size = 3285474304L
        model_sha256 = "fa8aeb3b6bf6152774e87d13e09892aa065f4e0c4abe90806cd8ab18ff72d9fe"
        projector_sha256 = $Projector.sha256
    }
}

function Get-PinnedFile {
    param(
        [Parameter(Mandatory = $true)][System.Collections.IDictionary]$Spec,
        [Parameter(Mandatory = $true)][string]$Directory
    )
    $Destination = Join-Path $Directory ([string]$Spec.name)
    if (-not (Test-Path -LiteralPath $Destination -PathType Leaf)) {
        Invoke-WebRequest -Uri ([string]$Spec.url) -OutFile $Destination -MaximumRedirection 5
    }
    $File = Get-Item -LiteralPath $Destination
    if ($File.Length -ne [long]$Spec.size) { throw "Model evidence file size mismatch: $($Spec.name)" }
    $Digest = (Get-FileHash -LiteralPath $Destination -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($Digest -ne [string]$Spec.sha256) { throw "Model evidence file SHA-256 mismatch: $($Spec.name)" }
    return $Destination
}

function New-PendingResult {
    return [ordered]@{
        status = "pending"
        model_invoked = $null
        response_valid = $null
        parser_error = $null
        model_error = $null
        readiness = $null
        input_packet_sha256 = $null
        proposal_sha256 = $null
        validation_sha256 = $null
        proposal = $null
        validated_proposal = $null
        field_results = $null
        unsupported_facts = @()
        timings_ms = [ordered]@{ extraction = $null; inference = $null; total = $null }
        peak_rss_bytes = $null
    }
}

function Wait-LlamaServer {
    param([System.Diagnostics.Process]$Process, [int]$Port, [string]$ApiKey, [string]$Log, [string]$ErrorLog)
    $Headers = @{ Authorization = "Bearer $ApiKey" }
    for ($Attempt = 0; $Attempt -lt 300; $Attempt += 1) {
        $Process.Refresh()
        if ($Process.HasExited) {
            throw "llama-server exited during model-evaluation startup: $(Get-Content -LiteralPath $Log -Raw) $(Get-Content -LiteralPath $ErrorLog -Raw)"
        }
        try {
            Invoke-RestMethod -Uri "http://127.0.0.1:$Port/health" -Headers $Headers -TimeoutSec 5 | Out-Null
            return
        }
        catch {
            Start-Sleep -Seconds 2
        }
    }
    throw "Timed out starting llama-server for model evaluation"
}

function Invoke-Variant {
    param(
        [Parameter(Mandatory = $true)][string]$Variant,
        [Parameter(Mandatory = $true)][System.Collections.IDictionary]$Spec,
        [Parameter(Mandatory = $true)][string]$ProjectorPath,
        [Parameter(Mandatory = $true)][System.Collections.IDictionary]$Records,
        [Parameter(Mandatory = $true)][object[]]$Gold,
        [Parameter(Mandatory = $true)][string]$Work
    )
    $ModelSpec = [ordered]@{ name = $Spec.filename; url = $Spec.url; size = $Spec.size; sha256 = $Spec.model_sha256 }
    $ModelPath = Get-PinnedFile -Spec $ModelSpec -Directory $Work
    $Listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
    $Listener.Start()
    $Port = ([Net.IPEndPoint]$Listener.LocalEndpoint).Port
    $Listener.Stop()
    $ApiKey = [guid]::NewGuid().ToString("N")
    $Log = Join-Path $Work "$Variant-llama-server.log"
    $ErrorLog = Join-Path $Work "$Variant-llama-server-error.log"
    $Arguments = @(
        "--host", "127.0.0.1",
        "--port", $Port,
        "--api-key", $ApiKey,
        "--model", $ModelPath,
        "--mmproj", $ProjectorPath,
        "--parallel", "1",
        "--ctx-size", "8192",
        "--n-gpu-layers", "0"
    )
    $Server = Start-Process -FilePath (Join-Path $Runtime "llama-server.exe") -ArgumentList $Arguments -RedirectStandardOutput $Log -RedirectStandardError $ErrorLog -WindowStyle Hidden -PassThru
    try {
        Wait-LlamaServer -Process $Server -Port $Port -ApiKey $ApiKey -Log $Log -ErrorLog $ErrorLog
        foreach ($Fixture in $Gold) {
            $FixturePath = Join-Path $Fixtures ([string]$Fixture.file)
            if (-not (Test-Path -LiteralPath $FixturePath -PathType Leaf)) { throw "Generated gold fixture is missing: $($Fixture.file)" }
            $EvaluatorArguments = @(
                "--endpoint", "http://127.0.0.1:$Port/v1/chat/completions",
                "--api-key", $ApiKey,
                "--worker", $Worker,
                "--fixture", $FixturePath,
                "--fixture-name", ([string]$Fixture.file),
                "--expected", $ExpectedPath
            )
            $Json = & $Evaluator @EvaluatorArguments
            if ($LASTEXITCODE -ne 0) { throw "Production evaluator failed for $Variant/$($Fixture.file)" }
            $Result = $Json | ConvertFrom-Json
            $Server.Refresh()
            $Result.peak_rss_bytes = [long]$Server.PeakWorkingSet64
            $Records[[string]$Fixture.file][$Variant] = $Result
        }
    }
    finally {
        if (-not $Server.HasExited) { Stop-Process -Id $Server.Id -Force }
        Remove-Item -LiteralPath $ModelPath -Force -ErrorAction SilentlyContinue
    }
}

function Measure-Summary {
    param([Parameter(Mandatory = $true)][System.Collections.IDictionary]$Records, [Parameter(Mandatory = $true)][object[]]$Gold)
    $Eligible = @($Gold | Where-Object { -not $_.PSObject.Properties["expected_error"] }).Count
    $Metrics = [ordered]@{
        q4 = [ordered]@{ correct = 0; total = 0; valid = 0; readiness = 0; dates = 0; parties = 0 }
        q8 = [ordered]@{ correct = 0; total = 0; valid = 0; readiness = 0; dates = 0; parties = 0 }
    }
    $AmbiguousInReview = 0
    $AmbiguousTotal = 0
    foreach ($Fixture in $Gold) {
        if ($Fixture.PSObject.Properties["expected_error"]) { continue }
        foreach ($Variant in @("q4", "q8")) {
            $Result = $Records[[string]$Fixture.file][$Variant]
            if ($Result.response_valid -eq $true) { $Metrics[$Variant].valid += 1 }
            if ($Result.readiness -eq $Fixture.expected_readiness) { $Metrics[$Variant].readiness += 1 }
            foreach ($Field in @($Result.field_results.PSObject.Properties)) {
                $Metrics[$Variant].total += 1
                if ($Field.Value -eq $true) { $Metrics[$Variant].correct += 1 }
            }
            foreach ($Fact in @($Result.unsupported_facts)) {
                if ($Result.readiness -ne "ready") { continue }
                if ($Fact.field -eq "document_date") { $Metrics[$Variant].dates += 1 }
                if ($Fact.field -eq "parties") { $Metrics[$Variant].parties += 1 }
            }
            if (@($Fixture.ambiguity).Count -gt 0) {
                $AmbiguousTotal += 1
                if ($Result.readiness -eq "needs_review") { $AmbiguousInReview += 1 }
            }
        }
    }
    return [ordered]@{
        eligible_fixtures = $Eligible
        q4_response_validity = $Metrics.q4.valid / $Eligible
        q8_response_validity = $Metrics.q8.valid / $Eligible
        q4_field_accuracy = $Metrics.q4.correct / $Metrics.q4.total
        q8_field_accuracy = $Metrics.q8.correct / $Metrics.q8.total
        q4_unsupported_ready_dates = $Metrics.q4.dates
        q4_unsupported_ready_parties = $Metrics.q4.parties
        q8_unsupported_ready_dates = $Metrics.q8.dates
        q8_unsupported_ready_parties = $Metrics.q8.parties
        ambiguous_fixtures_in_review = $AmbiguousInReview
        ambiguous_fixtures_total = $AmbiguousTotal
        q4_readiness_accuracy = $Metrics.q4.readiness / $Eligible
        q8_readiness_accuracy = $Metrics.q8.readiness / $Eligible
    }
}

$Work = Join-Path ([IO.Path]::GetTempPath()) ("intern-model-evaluation-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $Work | Out-Null
$PreviousRuntime = $env:INTERN_RUNTIME_DIR
$PreviousPath = $env:PATH
try {
    $env:INTERN_RUNTIME_DIR = $Runtime
    $env:PATH = "$Runtime;$PreviousPath"
    $Expected = Get-Content -LiteralPath $ExpectedPath -Raw | ConvertFrom-Json
    $Manifest = Get-Content -LiteralPath $ManifestPath -Raw | ConvertFrom-Json
    $Signed = @{}
    foreach ($File in $Manifest.files) { $Signed[[string]$File.file] = [string]$File.sha256 }
    $Records = [ordered]@{}
    foreach ($Fixture in $Expected.fixtures) {
        $Records[[string]$Fixture.file] = [ordered]@{
            fixture_sha256 = $Signed[[string]$Fixture.file]
            q4 = (New-PendingResult)
            q8 = (New-PendingResult)
        }
    }

    $ProjectorPath = Get-PinnedFile -Spec $Projector -Directory $Work
    Invoke-Variant -Variant "q4" -Spec $Models.q4 -ProjectorPath $ProjectorPath -Records $Records -Gold @($Expected.fixtures) -Work $Work
    Invoke-Variant -Variant "q8" -Spec $Models.q8 -ProjectorPath $ProjectorPath -Records $Records -Gold @($Expected.fixtures) -Work $Work
    $Summary = Measure-Summary -Records $Records -Gold @($Expected.fixtures)
    $Q4Qualifies = $Summary.q4_unsupported_ready_dates -eq 0 -and
        $Summary.q4_unsupported_ready_parties -eq 0 -and
        $Summary.q4_field_accuracy -ge ($Summary.q8_field_accuracy - 0.02) -and
        $Summary.q4_readiness_accuracy -eq 1
    $SelectedModel = if ($Q4Qualifies) { "q4" } else { "q8" }
    $Accepted = $Summary.q4_response_validity -eq 1 -and
        $Summary.q8_response_validity -eq 1 -and
        $Summary.q8_unsupported_ready_dates -eq 0 -and
        $Summary.q8_unsupported_ready_parties -eq 0 -and
        $Summary.q8_readiness_accuracy -eq 1 -and
        $Summary.ambiguous_fixtures_in_review -eq $Summary.ambiguous_fixtures_total
    $Reasons = if ($Accepted) { @() } else { @("One or more derived production model release gates failed.") }
    $Report = [ordered]@{
        schema_version = 2
        status = "completed"
        selected_model = $SelectedModel
        generated_at = [DateTime]::UtcNow.ToString("o")
        commit = (git -C $Repository rev-parse HEAD).Trim()
        release_inputs_sha256 = (& node (Join-Path $Repository "scripts/hash-release-inputs.mjs") "--root=$Repository").Trim()
        runner = [ordered]@{
            os = if ($env:RUNNER_OS) { $env:RUNNER_OS } else { "Windows" }
            arch = if ($env:RUNNER_ARCH) { $env:RUNNER_ARCH } else { $env:PROCESSOR_ARCHITECTURE }
            ci_run_id = if ($env:GITHUB_RUN_ID) { $env:GITHUB_RUN_ID } else { "local-$PID" }
        }
        models = [ordered]@{
            q4 = [ordered]@{ model_id = $Models.q4.model_id; filename = $Models.q4.filename; size = $Models.q4.size; model_sha256 = $Models.q4.model_sha256; projector_sha256 = $Models.q4.projector_sha256 }
            q8 = [ordered]@{ model_id = $Models.q8.model_id; filename = $Models.q8.filename; size = $Models.q8.size; model_sha256 = $Models.q8.model_sha256; projector_sha256 = $Models.q8.projector_sha256 }
        }
        runtime = [ordered]@{ llama_cpp_build = "b10361"; archive_sha256 = "36da9e9c1c094bf7842fab69e6cc0921125a67fa2611ba8f329a00804350302a" }
        prompt = [ordered]@{ path = "src-tauri/src/model/prompt.rs"; sha256 = (Get-FileHash -LiteralPath $PromptPath -Algorithm SHA256).Hash.ToLowerInvariant() }
        corpus = [ordered]@{
            manifest_path = "fixtures/manifest.json"
            manifest_sha256 = (Get-FileHash -LiteralPath $ManifestPath -Algorithm SHA256).Hash.ToLowerInvariant()
            expected_path = "fixtures/expected.json"
            expected_sha256 = (Get-FileHash -LiteralPath $ExpectedPath -Algorithm SHA256).Hash.ToLowerInvariant()
        }
        records = $Records
        summary = $Summary
        acceptance = [ordered]@{ status = if ($Accepted) { "accepted" } else { "rejected" }; reasons = $Reasons }
    }
    $OutputDirectory = Split-Path -Parent $OutputPath
    New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
    $Report | ConvertTo-Json -Depth 30 | Set-Content -LiteralPath $OutputPath -Encoding utf8NoBOM
    node (Join-Path $Repository "scripts/validate-model-evaluation.mjs") $OutputPath
    if ($LASTEXITCODE -ne 0) { throw "Generated production model evidence did not satisfy the release validator" }
}
finally {
    $env:INTERN_RUNTIME_DIR = $PreviousRuntime
    $env:PATH = $PreviousPath
    if (Test-Path -LiteralPath $Work) { Remove-Item -LiteralPath $Work -Recurse -Force }
}
