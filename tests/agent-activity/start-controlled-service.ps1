[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$SourceRoot,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$RuntimeRoot,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$TokenFile,

    [Parameter(Mandatory = $true)]
    [ValidateRange(1, 65535)]
    [int]$Port
)

$ErrorActionPreference = 'Stop'
$source = [IO.Path]::GetFullPath($SourceRoot)
$runtime = [IO.Path]::GetFullPath($RuntimeRoot)
$token = [IO.Path]::GetFullPath($TokenFile)
$evidence = Join-Path $runtime 'evidence'
$pidFile = Join-Path $runtime 'controlled-service.pid'

if (Test-Path -LiteralPath $pidFile) {
    throw "受控服务 PID 文件已经存在 path=$pidFile"
}
if (@(Get-NetTCPConnection -LocalPort $Port -ErrorAction SilentlyContinue).Count -ne 0) {
    throw "受控服务端口已被占用 port=$Port"
}
if (-not (Test-Path -LiteralPath $token -PathType Leaf)) {
    throw "找不到测试凭据文件 path=$token"
}

New-Item -ItemType Directory -Path $evidence -Force | Out-Null
$servicePath = Join-Path $source 'services\controlled-service.mjs'
$requestLog = Join-Path $evidence 'workbuddy-manual-requests.full.ndjson'
$stdoutLog = Join-Path $evidence 'workbuddy-manual-service.stdout.log'
$stderrLog = Join-Path $evidence 'workbuddy-manual-service.stderr.log'
$arguments = @(
    $servicePath,
    '--host', '127.0.0.1',
    '--port', $Port.ToString(),
    '--log', $requestLog,
    '--token-file', $token
)
$service = Start-Process `
    -FilePath (Get-Command node).Source `
    -ArgumentList $arguments `
    -PassThru `
    -WindowStyle Hidden `
    -RedirectStandardOutput $stdoutLog `
    -RedirectStandardError $stderrLog

$ready = $false
foreach ($attempt in 1..50) {
    if (@(Get-NetTCPConnection -LocalAddress 127.0.0.1 -LocalPort $Port -State Listen -ErrorAction SilentlyContinue).Count -eq 1) {
        $ready = $true
        break
    }
    Start-Sleep -Milliseconds 100
}
if (-not $ready) {
    if (-not $service.HasExited) {
        Stop-Process -Id $service.Id -Force
    }
    $stderr = Get-Content -LiteralPath $stderrLog -Raw -ErrorAction SilentlyContinue
    throw "受控服务未按时监听 port=$Port stderr=$stderr"
}

[IO.File]::WriteAllText($pidFile, $service.Id.ToString(), [Text.Encoding]::ASCII)
[pscustomobject]@{
    pid = $service.Id
    pid_file = $pidFile
    base_url = "http://127.0.0.1:$Port"
    request_log = $requestLog
    stdout_log = $stdoutLog
    stderr_log = $stderrLog
} | ConvertTo-Json -Compress
