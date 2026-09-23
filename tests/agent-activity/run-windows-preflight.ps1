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
$ProgressPreference = 'SilentlyContinue'
$source = [IO.Path]::GetFullPath($SourceRoot)
$runtime = [IO.Path]::GetFullPath($RuntimeRoot)
$tokenPath = [IO.Path]::GetFullPath($TokenFile)
$evidence = Join-Path $runtime 'evidence'
$token = (Get-Content -LiteralPath $tokenPath -Raw).TrimEnd()
if ($token.Length -eq 0) {
    throw "测试凭据文件为空 path=$tokenPath"
}
if (@(Get-NetTCPConnection -LocalPort $Port -ErrorAction SilentlyContinue).Count -ne 0) {
    throw "预检端口已被占用 port=$Port"
}

New-Item -ItemType Directory -Path $evidence -Force | Out-Null
$prepareResultPath = Join-Path $evidence 'prepare-result.json'
& (Join-Path $source 'prepare-windows-assets.ps1') `
    -SourceRoot $source `
    -RuntimeRoot $runtime `
    -TokenFile $tokenPath |
    Set-Content -LiteralPath $prepareResultPath -Encoding UTF8

$servicePath = Join-Path $source 'services\controlled-service.mjs'
$requestLog = Join-Path $evidence 'preflight-requests.full.ndjson'
$stdoutLog = Join-Path $evidence 'controlled-service.stdout.log'
$stderrLog = Join-Path $evidence 'controlled-service.stderr.log'
$baseUrl = "http://127.0.0.1:$Port"
$arguments = @(
    $servicePath,
    '--host', '127.0.0.1',
    '--port', $Port.ToString(),
    '--log', $requestLog,
    '--token-file', $tokenPath
)
$service = Start-Process `
    -FilePath (Get-Command node).Source `
    -ArgumentList $arguments `
    -PassThru `
    -WindowStyle Hidden `
    -RedirectStandardOutput $stdoutLog `
    -RedirectStandardError $stderrLog

try {
    $ready = $false
    foreach ($attempt in 1..50) {
        if (@(Get-NetTCPConnection -LocalAddress 127.0.0.1 -LocalPort $Port -State Listen -ErrorAction SilentlyContinue).Count -eq 1) {
            $ready = $true
            break
        }
        Start-Sleep -Milliseconds 100
    }
    if (-not $ready) {
        $stderr = Get-Content -LiteralPath $stderrLog -Raw -ErrorAction SilentlyContinue
        throw "受控服务未按时监听 port=$Port stderr=$stderr"
    }

    $skillScript = Join-Path $source 'skills\weather\scripts\query-weather.ps1'
    $testCity = -join ([char]0x5317, [char]0x4EAC)
    $skillJson = & powershell.exe `
        -NoLogo `
        -NoProfile `
        -NonInteractive `
        -ExecutionPolicy Bypass `
        -File $skillScript `
        -City $testCity `
        -RunId 'S01-preflight-windows' `
        -BaseUrl $baseUrl
    if ($LASTEXITCODE -ne 0) {
        throw "天气 Skill 脚本失败 exit_code=$LASTEXITCODE"
    }
    $skillResult = $skillJson | ConvertFrom-Json
    if ($skillResult.execution.channel -ne 'skill') {
        throw "天气 Skill 返回通道错误 result=$skillJson"
    }

    $authResult = Invoke-RestMethod `
        -Method Post `
        -Uri "$baseUrl/auth" `
        -Headers @{Authorization = "Bearer $token"} `
        -TimeoutSec 5
    if (-not $authResult.authenticated) {
        throw "受控服务专用测试凭据鉴权失败 uri=$baseUrl/auth"
    }

    $prepared = Get-Content -LiteralPath $prepareResultPath -Raw | ConvertFrom-Json
    [pscustomobject]@{
        node_version = (& node --version)
        source_root = $source
        runtime_root = $runtime
        token_file = $tokenPath
        token_sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $tokenPath).Hash
        skill_package = $prepared.skill_package
        skill_package_sha256 = $prepared.skill_package_sha256
        mcp_config = $prepared.mcp_config
        mcp_config_sha256 = $prepared.mcp_config_sha256
        skill_channel = $skillResult.execution.channel
        skill_component = $skillResult.execution.component
        skill_tool_name = $skillResult.execution.tool_name
        auth_valid = $authResult.authenticated
        request_log = $requestLog
        request_count = @(Get-Content -LiteralPath $requestLog).Count
    } | ConvertTo-Json -Compress
}
finally {
    if (-not $service.HasExited) {
        Stop-Process -Id $service.Id -Force
        $service.WaitForExit()
    }
}
