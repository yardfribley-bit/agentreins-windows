[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$SemanticRunRoot,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$SourceManifest
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Read-PublishedNdjson {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path,
        [Parameter(Mandatory = $true)]
        [long]$PublishedBytes
    )
    if ($PublishedBytes -lt 0 -or $PublishedBytes -gt [int]::MaxValue) {
        throw "已发布 NDJSON 边界超出当前发布器可读范围 published_bytes=$PublishedBytes path=$Path"
    }
    $contents = [IO.File]::ReadAllBytes($Path)
    if ($contents.LongLength -lt $PublishedBytes) {
        throw "NDJSON 短于已发布边界 published_bytes=$PublishedBytes actual_bytes=$($contents.LongLength) path=$Path"
    }
    if ($PublishedBytes -eq 0) {
        return @()
    }
    $text = [Text.Encoding]::UTF8.GetString($contents, 0, [int]$PublishedBytes)
    return @($text -split "`n" | Where-Object { -not [string]::IsNullOrWhiteSpace($_) } | ForEach-Object { $_ | ConvertFrom-Json })
}

$semanticRun = [IO.Path]::GetFullPath($SemanticRunRoot)
$sourceManifestPath = [IO.Path]::GetFullPath($SourceManifest)
$semanticStatePath = Join-Path $semanticRun 'semantic-observer-process.json'
foreach ($requiredPath in @($semanticStatePath, $sourceManifestPath)) {
    if (-not (Test-Path -LiteralPath $requiredPath -PathType Leaf)) {
        throw "MCP 会话发布依赖不存在 path=$requiredPath"
    }
}

$semanticState = Get-Content -LiteralPath $semanticStatePath -Raw -Encoding UTF8 | ConvertFrom-Json
if ([string]::IsNullOrWhiteSpace([string]$semanticState.session_id)) {
    throw "语义 Observer 状态缺少 session_id path=$semanticStatePath"
}
if (-not (Test-Path -LiteralPath $semanticState.semantic_manifest_path -PathType Leaf)) {
    throw "语义证据 manifest 不存在 path=$($semanticState.semantic_manifest_path)"
}
$semanticManifest = Get-Content -LiteralPath $semanticState.semantic_manifest_path -Raw -Encoding UTF8 | ConvertFrom-Json
if ($semanticManifest.session_id -ne $semanticState.session_id) {
    throw "语义证据与 Observer 会话不匹配 observer_session=$($semanticState.session_id) manifest_session=$($semanticManifest.session_id)"
}
$semanticRecords = @(Read-PublishedNdjson -Path $semanticManifest.semantic_file -PublishedBytes ([long]$semanticManifest.published_bytes))
if ($semanticRecords.Count -ne [long]$semanticManifest.semantic_events) {
    throw "语义证据计数不闭合 expected=$($semanticManifest.semantic_events) actual=$($semanticRecords.Count) path=$($semanticManifest.semantic_file)"
}

$source = Get-Content -LiteralPath $sourceManifestPath -Raw -Encoding UTF8 | ConvertFrom-Json
if ($source.schema_version -ne '1.0.0') {
    throw "不支持的 MCP 源 manifest 契约 expected=1.0.0 actual=$($source.schema_version) path=$sourceManifestPath"
}
if (-not (Test-Path -LiteralPath $source.mcp_file -PathType Leaf)) {
    throw "MCP 协议证据不存在 path=$($source.mcp_file)"
}
$actualBytes = (Get-Item -LiteralPath $source.mcp_file).Length
if ($actualBytes -lt [long]$source.published_bytes) {
    throw "MCP 协议证据短于已发布边界 published_bytes=$($source.published_bytes) actual_bytes=$actualBytes path=$($source.mcp_file)"
}
$mcpRecords = @(Read-PublishedNdjson -Path $source.mcp_file -PublishedBytes ([long]$source.published_bytes))
if ($mcpRecords.Count -ne [long]$source.protocol_records) {
    throw "MCP 协议证据计数不闭合 expected=$($source.protocol_records) actual=$($mcpRecords.Count) path=$($source.mcp_file)"
}
$mcpToolRequests = @($mcpRecords | Where-Object { $_.direction -eq 'request' } | ForEach-Object {
    $message = $_.raw_json | ConvertFrom-Json
    if ($message.method -eq 'tools/call') {
        $metadata = $message.params._meta
        $conversationId = [string]$metadata.'workbuddy.ai/conversationId'
        $requestId = [string]$metadata.'workbuddy.ai/requestId'
        $messageId = [string]$metadata.'workbuddy.ai/messageId'
        if ([string]::IsNullOrWhiteSpace($conversationId) -or [string]::IsNullOrWhiteSpace($requestId) -or [string]::IsNullOrWhiteSpace($messageId)) {
            throw "MCP tools/call 缺少 WorkBuddy 原生元数据 jsonrpc_id=$($message.id)"
        }
        [pscustomobject]@{
            jsonrpc_id = [string]$message.id
            conversation_id = $conversationId
            request_id = $requestId
            message_id = $messageId
        }
    }
})
if ($mcpRecords.Count -gt 0 -and $mcpToolRequests.Count -eq 0) {
    throw "MCP 协议证据没有可用于会话归属的 tools/call 原生元数据 path=$($source.mcp_file)"
}
foreach ($request in $mcpToolRequests) {
    $matches = @($semanticRecords | Where-Object {
        if ($_.content_kind -ne 'tool_call' -or $_.agent_session_id -ne $request.conversation_id -or $_.source_record_id -ne $request.message_id) {
            return $false
        }
        return @($_.native_evidence.identifiers | Where-Object {
            $_.source -eq 'work_buddy' -and $_.kind -eq 'provider_trace' -and $_.value -eq $request.request_id
        }).Count -eq 1
    })
    if ($matches.Count -ne 1) {
        throw "MCP tools/call 无法通过 WorkBuddy 原生 conversationId/requestId/messageId 唯一归属当前语义会话 jsonrpc_id=$($request.jsonrpc_id) matches=$($matches.Count) observer_session=$($semanticState.session_id)"
    }
}

$targetPath = Join-Path $semanticRun 'mcp-protocol-manifest.json'
$temporaryPath = "$targetPath.tmp"
if (Test-Path -LiteralPath $temporaryPath) {
    throw "MCP 会话发布临时文件已存在，拒绝覆盖 path=$temporaryPath"
}
$publication = [ordered]@{
    schema_version = '1.1.0'
    session_id = [string]$semanticState.session_id
    mcp_file = [IO.Path]::GetFullPath([string]$source.mcp_file)
    published_bytes = [long]$source.published_bytes
    protocol_records = [long]$source.protocol_records
    complete = [bool]$source.complete
    source_manifest = $sourceManifestPath
}
[IO.File]::WriteAllText(
    $temporaryPath,
    ($publication | ConvertTo-Json -Compress),
    [Text.UTF8Encoding]::new($false)
)
Move-Item -LiteralPath $temporaryPath -Destination $targetPath -Force

[pscustomobject]@{
    session_id = [string]$semanticState.session_id
    published_manifest = $targetPath
    mcp_file = $publication.mcp_file
    published_bytes = $publication.published_bytes
    protocol_records = $publication.protocol_records
    complete = $publication.complete
} | ConvertTo-Json -Compress
