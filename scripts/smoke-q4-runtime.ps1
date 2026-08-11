[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$LlamaServerPath,
    [string]$ModelManifestPath = (Join-Path (Resolve-Path (Join-Path $PSScriptRoot "..")).Path "src-tauri/resources/model-manifest.json")
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$Server = (Resolve-Path -LiteralPath $LlamaServerPath).Path
$Manifest = Get-Content -LiteralPath $ModelManifestPath -Raw | ConvertFrom-Json
$ApprovedModels = @{
    "qwen2.5-vl-3b-instruct-q4-k-m" = @{ name = "Qwen2.5-VL-3B-Instruct-Q4_K_M.gguf"; size = 1929901056L; sha256 = "d02fe9b69ad8cadbbd228e387667af66612c44bed29ffc8eb1e7caf9ac486c12" }
    "qwen2.5-vl-3b-instruct-q8-0" = @{ name = "Qwen2.5-VL-3B-Instruct-Q8_0.gguf"; size = 3285474304L; sha256 = "fa8aeb3b6bf6152774e87d13e09892aa065f4e0c4abe90806cd8ab18ff72d9fe" }
}
$SelectedModel = $ApprovedModels[[string]$Manifest.model_id]
if ($Manifest.schema_version -ne 1 -or $null -eq $SelectedModel -or $Manifest.files.Count -ne 2) {
    throw "Production selected-model manifest is missing or changed"
}
$Expected = @{}
$Expected[[string]$SelectedModel.name] = @{ size = $SelectedModel.size; sha256 = $SelectedModel.sha256 }
$Expected["mmproj-Qwen2.5-VL-3B-Instruct-f16.gguf"] = @{ size = 1338428128; sha256 = "b9160fe9d814d1fadf68395677468534778b39ac33c2e7561b7b218626e60d5e" }
$Work = Join-Path ([IO.Path]::GetTempPath()) ("intern-selected-model-smoke-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $Work | Out-Null
$Process = $null

try {
    foreach ($File in $Manifest.files) {
        $Pin = $Expected[[string]$File.name]
        if ($null -eq $Pin -or [long]$File.size -ne $Pin.size -or [string]$File.sha256 -ne $Pin.sha256 -or
            -not ([string]$File.url).StartsWith("https://huggingface.co/ggml-org/Qwen2.5-VL-3B-Instruct-GGUF/resolve/main/")) {
            throw "Unapproved production model pin: $($File.name)"
        }
        $Destination = Join-Path $Work ([string]$File.name)
        Invoke-WebRequest -Uri ([string]$File.url) -OutFile $Destination -MaximumRedirection 5
        if ((Get-Item -LiteralPath $Destination).Length -ne $Pin.size) { throw "Model size mismatch: $($File.name)" }
        if ((Get-FileHash -LiteralPath $Destination -Algorithm SHA256).Hash.ToLowerInvariant() -ne $Pin.sha256) { throw "Model SHA-256 mismatch: $($File.name)" }
    }

    $Listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
    $Listener.Start()
    $Port = ([Net.IPEndPoint]$Listener.LocalEndpoint).Port
    $Listener.Stop()
    $ApiKey = [guid]::NewGuid().ToString("N")
    $Model = Join-Path $Work ([string]$SelectedModel.name)
    $Projector = Join-Path $Work "mmproj-Qwen2.5-VL-3B-Instruct-f16.gguf"
    $Log = Join-Path $Work "llama-server.log"
    $ErrorLog = Join-Path $Work "llama-server-error.log"
    $Arguments = @("--host", "127.0.0.1", "--port", $Port, "--api-key", $ApiKey, "--model", $Model, "--mmproj", $Projector, "--ctx-size", "8192", "--parallel", "1", "--n-gpu-layers", "0")
    $Process = Start-Process -FilePath $Server -ArgumentList $Arguments -RedirectStandardOutput $Log -RedirectStandardError $ErrorLog -PassThru -WindowStyle Hidden
    $Headers = @{ Authorization = "Bearer $ApiKey" }
    $Healthy = $false
    for ($Attempt = 0; $Attempt -lt 180; $Attempt += 1) {
        if ($Process.HasExited) { throw "llama-server exited during selected-model startup: $(Get-Content -LiteralPath $Log -Raw) $(Get-Content -LiteralPath $ErrorLog -Raw)" }
        try { Invoke-RestMethod -Uri "http://127.0.0.1:$Port/health" -Headers $Headers -TimeoutSec 5 | Out-Null; $Healthy = $true; break } catch { Start-Sleep -Seconds 2 }
    }
    if (-not $Healthy) { throw "Timed out starting the production selected-model runtime" }

    $Body = @{
        model = $Manifest.model_id
        temperature = 0
        max_tokens = 96
        messages = @(@{ role = "user"; content = "Extract only supported facts from this representative document. Reply in one line with the employee and exact agreement date. UNTRUSTED DOCUMENT: Employment Agreement between Northstar Lantern Works LLC and Mira Vale. Agreement date: February 14, 2025. END DOCUMENT." })
    } | ConvertTo-Json -Depth 8 -Compress
    $Response = Invoke-RestMethod -Method Post -Uri "http://127.0.0.1:$Port/v1/chat/completions" -Headers $Headers -ContentType "application/json" -Body $Body -TimeoutSec 300
    $Content = [string]$Response.choices[0].message.content
    $HasDate = $Content.Contains("February 14, 2025") -or $Content.Contains("2025-02-14")
    if (-not $Content.Contains("Mira Vale") -or -not $HasDate) { throw "Production selected-model runtime omitted supported smoke facts: $Content" }
    Write-Host "Pinned llama.cpp b10361 loaded the exact accepted model+projector production pair and extracted representative supported facts."
}
finally {
    if ($null -ne $Process -and -not $Process.HasExited) { Stop-Process -Id $Process.Id -Force }
    if (Test-Path -LiteralPath $Work) { Remove-Item -LiteralPath $Work -Recurse -Force }
}
