[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$InputNdjson,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$TokenFile,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$OutputRoot
)

$ErrorActionPreference = 'Stop'
$inputPath = [IO.Path]::GetFullPath($InputNdjson)
$tokenPath = [IO.Path]::GetFullPath($TokenFile)
$output = [IO.Path]::GetFullPath($OutputRoot)
$token = (Get-Content -LiteralPath $tokenPath -Raw).TrimEnd()
if ($token.Length -eq 0) {
    throw "测试凭据文件为空 path=$tokenPath"
}

New-Item -ItemType Directory -Path $output -Force | Out-Null
$fullCsv = Join-Path $output 'preflight-requests.full.csv'
$filteredCsv = Join-Path $output 'preflight-requests.filtered.csv'
$manifestCsv = Join-Path $output 'manifest.csv'

$events = Get-Content -LiteralPath $inputPath |
    Where-Object { $_.Length -gt 0 } |
    ForEach-Object { $_ | ConvertFrom-Json }
if (@($events).Count -eq 0) {
    throw "预检请求日志为空 path=$inputPath"
}

$fullRows = @($events | ForEach-Object {
    [pscustomobject]@{
        received_at = $_.received_at
        method = $_.method
        url = $_.url
        remote_address = $_.remote_address
        headers_json = ($_.headers | ConvertTo-Json -Compress)
        body = $_.body
    }
})
$fullRows | Export-Csv -LiteralPath $fullCsv -NoTypeInformation -Encoding UTF8

$filteredRows = @($fullRows | ForEach-Object {
    [pscustomobject]@{
        received_at = $_.received_at
        method = $_.method
        url = $_.url
        remote_address = $_.remote_address
        headers_json = $_.headers_json.Replace($token, '[REDACTED:controlled-service-token]')
        body = $_.body.Replace($token, '[REDACTED:controlled-service-token]')
    }
})
$filteredRows | Export-Csv -LiteralPath $filteredCsv -NoTypeInformation -Encoding UTF8

$fullText = Get-Content -LiteralPath $fullCsv -Raw
$filteredText = Get-Content -LiteralPath $filteredCsv -Raw
if (-not $fullText.Contains($token)) {
    throw "完整版 CSV 未保留专用测试凭据 path=$fullCsv"
}
if ($filteredText.Contains($token)) {
    throw "过滤版 CSV 仍包含专用测试凭据 path=$filteredCsv"
}

[pscustomobject]@{
    dataset = 'agent-activity-preflight'
    exported_at = [DateTimeOffset]::UtcNow.ToString('o')
    source_ndjson = $inputPath
    full_csv = (Split-Path -Leaf $fullCsv)
    filtered_csv = (Split-Path -Leaf $filteredCsv)
    full_rows = $fullRows.Count
    filtered_rows = $filteredRows.Count
    full_sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $fullCsv).Hash
    filtered_sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $filteredCsv).Hash
    credential_sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $tokenPath).Hash
    full_contains_plaintext_credential = $true
    filtered_contains_plaintext_credential = $false
} | Export-Csv -LiteralPath $manifestCsv -NoTypeInformation -Encoding UTF8

[pscustomobject]@{
    full_csv = $fullCsv
    filtered_csv = $filteredCsv
    manifest_csv = $manifestCsv
    full_rows = $fullRows.Count
    filtered_rows = $filteredRows.Count
    filtered_plaintext_credential_count = 0
} | ConvertTo-Json -Compress
