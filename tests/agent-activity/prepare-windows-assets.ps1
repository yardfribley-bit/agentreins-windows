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
    [string]$TokenFile
)

$ErrorActionPreference = 'Stop'
$source = [IO.Path]::GetFullPath($SourceRoot)
$runtime = [IO.Path]::GetFullPath($RuntimeRoot)
$token = [IO.Path]::GetFullPath($TokenFile)

foreach ($path in @($SourceRoot, $RuntimeRoot, $TokenFile)) {
    if (-not [IO.Path]::IsPathRooted($path)) {
        throw "路径必须是绝对路径 path=$path"
    }
}
if (-not (Test-Path -LiteralPath $source -PathType Container)) {
    throw "找不到测试资产目录 path=$source"
}
if (-not (Test-Path -LiteralPath $token -PathType Leaf)) {
    throw "找不到测试凭据文件 path=$token"
}
if ((Get-Content -LiteralPath $token -Raw).TrimEnd().Length -eq 0) {
    throw "测试凭据文件为空 path=$token"
}

$runtimeFixtures = Join-Path $runtime 'fixtures'
$runtimeConfigs = Join-Path $runtime 'configs'
$runtimePackages = Join-Path $runtime 'packages'
$runtimeEvidence = Join-Path $runtime 'evidence'
New-Item -ItemType Directory -Path $runtimeFixtures, $runtimeConfigs, $runtimePackages, $runtimeEvidence -Force | Out-Null

& (Join-Path $source 'fixtures\reset-fixtures.ps1') `
    -SourceRoot (Join-Path $source 'fixtures') `
    -TestRoot $runtimeFixtures | Out-Null

$skillSource = Join-Path $source 'skills\weather'
$skillPackage = Join-Path $runtimePackages 'agentreins-weather-skill.zip'
if (Test-Path -LiteralPath $skillPackage) {
    Remove-Item -LiteralPath $skillPackage -Force
}
Compress-Archive -Path (Join-Path $skillSource '*') -DestinationPath $skillPackage

$mcpServer = Join-Path $source 'mcp\weather\server.mjs'
$mcpDirectory = Split-Path -Parent $mcpServer
$mcpProtocolLog = Join-Path $runtimeEvidence 'mcp-protocol.ndjson'
$mcpProtocolManifest = Join-Path $runtimeEvidence 'mcp-protocol-manifest.json'
foreach ($mcpEvidencePath in @($mcpProtocolLog, $mcpProtocolManifest)) {
    if (Test-Path -LiteralPath $mcpEvidencePath) {
        Remove-Item -LiteralPath $mcpEvidencePath -Force
    }
}
$mcpTemplate = Get-Content -LiteralPath (Join-Path $source 'mcp\weather\mcp.workbuddy.template.json') -Raw
$mcpConfig = $mcpTemplate.Replace('__MCP_SERVER_PATH__', ($mcpServer -replace '\\', '\\'))
$mcpConfig = $mcpConfig.Replace('__MCP_SERVER_DIRECTORY__', ($mcpDirectory -replace '\\', '\\'))
$mcpConfig = $mcpConfig.Replace('__MCP_PROTOCOL_LOG_PATH__', ($mcpProtocolLog -replace '\\', '\\'))
$mcpConfig = $mcpConfig.Replace('__MCP_PROTOCOL_MANIFEST_PATH__', ($mcpProtocolManifest -replace '\\', '\\'))
$mcpConfigPath = Join-Path $runtimeConfigs 'agentreins-weather.mcp.json'
[IO.File]::WriteAllText($mcpConfigPath, $mcpConfig, [Text.UTF8Encoding]::new($false))
Get-Content -LiteralPath $mcpConfigPath -Raw | ConvertFrom-Json | Out-Null

$assetFiles = Get-ChildItem -LiteralPath $source -File -Recurse |
    Sort-Object FullName |
    ForEach-Object {
        [pscustomobject]@{
            path = $_.FullName
            sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $_.FullName).Hash
        }
    }

[pscustomobject]@{
    source_root = $source
    runtime_root = $runtime
    token_file = $token
    skill_package = $skillPackage
    skill_package_sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $skillPackage).Hash
    mcp_config = $mcpConfigPath
    mcp_config_sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $mcpConfigPath).Hash
    mcp_protocol_manifest = $mcpProtocolManifest
    evidence_root = $runtimeEvidence
    assets = @($assetFiles)
} | ConvertTo-Json -Depth 5
