[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$OsInputPath,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$CorrelationInputPath,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$ContextSnapshotPath,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$CollectorSummaryPath,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$RunId,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$OutputPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$inputPaths = @($OsInputPath, $CorrelationInputPath, $ContextSnapshotPath, $CollectorSummaryPath)
foreach ($path in $inputPaths) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "找不到观测报告输入 path=$path"
    }
}
if (Test-Path -LiteralPath $OutputPath) {
    throw "观测报告已经存在，拒绝覆盖 path=$OutputPath"
}

function Read-Ndjson {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path
    )

    @(
        Get-Content -LiteralPath $Path -Encoding UTF8 |
            Where-Object { $_.Length -gt 0 } |
            ForEach-Object { $_ | ConvertFrom-Json }
    )
}

function Convert-HtmlText {
    param(
        [Parameter(Mandatory = $true)]
        [AllowEmptyString()]
        [string]$Value
    )

    [Net.WebUtility]::HtmlEncode($Value)
}

function Convert-UnixTime {
    param(
        [Parameter(Mandatory = $true)]
        [long]$UnixMilliseconds
    )

    [DateTimeOffset]::FromUnixTimeMilliseconds($UnixMilliseconds).ToLocalTime().ToString('yyyy-MM-dd HH:mm:ss.fff zzz')
}

function Get-PropertyValue {
    param(
        [Parameter(Mandatory = $true)]
        [object]$InputObject,
        [Parameter(Mandatory = $true)]
        [string]$Name
    )

    $property = $InputObject.PSObject.Properties[$Name]
    if ($null -eq $property) { return $null }
    $property.Value
}

function Get-LayerStatus {
    param(
        [Parameter(Mandatory = $true)]
        [AllowEmptyCollection()]
        [object[]]$Records,
        [Parameter(Mandatory = $true)]
        [string]$EmptyStatus
    )

    if ($Records.Count -eq 0) { return $EmptyStatus }
    if (@($Records | Where-Object { (Get-PropertyValue -InputObject $_ -Name 'observation_status') -eq 'missing' }).Count -gt 0) { return '缺失' }
    if (@($Records | Where-Object { (Get-PropertyValue -InputObject $_ -Name 'observation_status') -eq 'partial' }).Count -gt 0) { return '部分观测' }
    return '已观测'
}

function New-SemanticRows {
    param(
        [Parameter(Mandatory = $true)]
        [AllowEmptyCollection()]
        [object[]]$Records
    )

    if ($Records.Count -eq 0) {
        return '<p class="empty">没有相应语义证据；请结合本层缺口说明判断是未发生还是未捕获。</p>'
    }
    $rows = foreach ($record in $Records | Sort-Object event_timestamp_unix_ms, event_id) {
        $time = Convert-HtmlText -Value (Convert-UnixTime -UnixMilliseconds ([long]$record.event_timestamp_unix_ms))
        $kind = Convert-HtmlText -Value ([string](Get-PropertyValue -InputObject $record -Name 'content_kind'))
        $tool = Convert-HtmlText -Value ([string](Get-PropertyValue -InputObject $record -Name 'tool_name'))
        $operation = Convert-HtmlText -Value ([string](Get-PropertyValue -InputObject $record -Name 'operation_id'))
        $model = Convert-HtmlText -Value ([string](Get-PropertyValue -InputObject $record -Name 'model_id'))
        $requestId = Convert-HtmlText -Value ([string](Get-PropertyValue -InputObject $record -Name 'conversation_request_id'))
        $recordStatus = Convert-HtmlText -Value ([string](Get-PropertyValue -InputObject $record -Name 'record_status'))
        $tokens = Convert-HtmlText -Value ("$((Get-PropertyValue -InputObject $record -Name 'input_tokens')) / $((Get-PropertyValue -InputObject $record -Name 'output_tokens')) / $((Get-PropertyValue -InputObject $record -Name 'total_tokens'))")
        $error = Convert-HtmlText -Value ([string](Get-PropertyValue -InputObject $record -Name 'error'))
        $content = Convert-HtmlText -Value ([string]$record.content)
        "<tr><td>$time</td><td>$kind</td><td>$tool</td><td>$operation</td><td>$model</td><td>$requestId</td><td>$recordStatus</td><td>$tokens</td><td>$error</td><td><details><summary>查看正文</summary><pre>$content</pre></details></td></tr>"
    }
    "<div class='table-wrap'><table><thead><tr><th>时间</th><th>数据类型</th><th>Tool/MCP</th><th>操作 ID</th><th>模型</th><th>请求 ID</th><th>状态</th><th>输入/输出/总 Token</th><th>错误</th><th>观测正文</th></tr></thead><tbody>$($rows -join '')</tbody></table></div>"
}

function New-OsRows {
    param(
        [Parameter(Mandatory = $true)]
        [AllowEmptyCollection()]
        [object[]]$Records
    )

    if ($Records.Count -eq 0) {
        return '<p class="empty">该用户行为时间窗内没有归因到目标 Agent 的此类 OS 事件。</p>'
    }
    $groups = $Records | Group-Object {
        "$($_.action.kind)|$($_.process.image_name)|$($_.resource.identifier)|$(if ($null -eq $_.destination) { '' } else { $_.destination.identifier })"
    }
    $rows = foreach ($group in $groups | Sort-Object Count -Descending) {
        $event = $group.Group[0]
        $count = ($group.Group | ForEach-Object {
            if ($null -eq $_.aggregation) { 1 } else { [long]$_.aggregation.occurrence_count }
        } | Measure-Object -Sum).Sum
        $bytes = ($group.Group | ForEach-Object {
            if ($null -eq $_.aggregation) { [long]$(if ($null -eq $_.result.bytes_transferred) { 0 } else { $_.result.bytes_transferred }) }
            else { [long]$(if ($null -eq $_.aggregation.total_bytes_transferred) { 0 } else { $_.aggregation.total_bytes_transferred }) }
        } | Measure-Object -Sum).Sum
        $action = Convert-HtmlText -Value ([string]$event.action.kind)
        $analysis = Get-PropertyValue -InputObject $event -Name 'analysis'
        $processLabel = if ($null -ne $analysis -and $analysis.grouping -eq 'process_image_name') {
            "$($event.process.image_name)（按进程名称聚合）"
        }
        else {
            "$($event.process.image_name) (PID $($event.process.pid))"
        }
        $process = Convert-HtmlText -Value $processLabel
        $resource = Convert-HtmlText -Value ([string]$event.resource.identifier)
        $destination = Convert-HtmlText -Value ([string]$(if ($null -eq $event.destination) { '' } else { $event.destination.identifier }))
        "<tr><td>$action</td><td>$process</td><td>$resource</td><td>$destination</td><td>$count</td><td>$bytes</td></tr>"
    }
    "<div class='table-wrap'><table><thead><tr><th>动作</th><th>进程</th><th>资源</th><th>目标</th><th>次数</th><th>字节</th></tr></thead><tbody>$($rows -join '')</tbody></table></div>"
}

function New-ContextRows {
    param(
        [Parameter(Mandatory = $true)]
        [object[]]$Artifacts,
        [Parameter(Mandatory = $true)]
        [string[]]$Categories
    )

    $selected = @($Artifacts | Where-Object { $_.category -in $Categories })
    if ($selected.Count -eq 0) {
        return '<p class="empty">上下文快照中没有发现相应配置文件。</p>'
    }
    $rows = foreach ($artifact in $selected) {
        $category = Convert-HtmlText -Value ([string]$artifact.category)
        $path = Convert-HtmlText -Value ([string]$artifact.path)
        $sha256 = Convert-HtmlText -Value ([string]$artifact.sha256)
        $content = Convert-HtmlText -Value ([string]$artifact.content)
        "<tr><td>$category</td><td>$path</td><td><code>$sha256</code></td><td><details><summary>查看正文</summary><pre>$content</pre></details></td></tr>"
    }
    "<div class='table-wrap'><table><thead><tr><th>类别</th><th>路径</th><th>SHA-256</th><th>配置正文</th></tr></thead><tbody>$($rows -join '')</tbody></table></div>"
}

function New-LayerSection {
    param(
        [Parameter(Mandatory = $true)]
        [int]$Number,
        [Parameter(Mandatory = $true)]
        [string]$Title,
        [Parameter(Mandatory = $true)]
        [string]$Status,
        [Parameter(Mandatory = $true)]
        [string]$Gap,
        [Parameter(Mandatory = $true)]
        [string]$Body
    )

    $statusClass = if ($Status -eq '已观测') { 'observed' } elseif ($Status -eq '缺失') { 'missing' } else { 'partial' }
    "<section class='layer'><header><span class='number'>$Number</span><h3>$(Convert-HtmlText -Value $Title)</h3><span class='status $statusClass'>$(Convert-HtmlText -Value $Status)</span></header><p class='gap'>边界：$(Convert-HtmlText -Value $Gap)</p>$Body</section>"
}

$correlationEvents = Read-Ndjson -Path $CorrelationInputPath
$semanticEvents = $correlationEvents
$osEvents = Read-Ndjson -Path $OsInputPath
$contextSnapshot = Get-Content -LiteralPath $ContextSnapshotPath -Raw -Encoding UTF8 | ConvertFrom-Json
$collectorSummary = Get-Content -LiteralPath $CollectorSummaryPath -Raw -Encoding UTF8 | ConvertFrom-Json

$activityIds = @(
    $semanticEvents |
        Where-Object { $_.user_activity_id -and $_.user_activity_id -notlike 'workbuddy:unattributed:*' } |
        Sort-Object event_timestamp_unix_ms |
        Select-Object -ExpandProperty user_activity_id -Unique
)
if ($activityIds.Count -eq 0) {
    throw "关联语义证据中没有可报告的 user_activity_id path=$CorrelationInputPath"
}

$activitySections = for ($index = 0; $index -lt $activityIds.Count; $index += 1) {
    $activityId = $activityIds[$index]
    $activityEvents = @($semanticEvents | Where-Object { $_.user_activity_id -eq $activityId })
    $activityStart = [long]($activityEvents | Measure-Object event_timestamp_unix_ms -Minimum).Minimum
    $activityEnd = [long]($activityEvents | Measure-Object event_timestamp_unix_ms -Maximum).Maximum
    $activityCorrelations = @($correlationEvents | Where-Object { $_.user_activity_id -eq $activityId })
    $linkedOsEventIds = @($activityCorrelations | ForEach-Object { @($_.linked_os_event_ids) })
    $activityOsEvents = @($osEvents | Where-Object { $_.event_id -in $linkedOsEventIds })
    $promptEvents = @($activityEvents | Where-Object { $_.observation_layer -eq 'user_prompt' })
    $systemEvents = @($activityEvents | Where-Object { $_.observation_layer -eq 'base_system_instructions' })
    $policyEvents = @($activityEvents | Where-Object { $_.observation_layer -eq 'skills_agent_policy' })
    $toolsEvents = @($activityEvents | Where-Object { $_.observation_layer -eq 'available_tools_mcp' })
    $llmEvents = @($activityEvents | Where-Object { $_.observation_layer -eq 'llm_request_response' })
    $toolCallEvents = @($activityEvents | Where-Object { $_.observation_layer -eq 'tool_mcp_calls' })
    $toolResultEvents = @($activityEvents | Where-Object { $_.observation_layer -eq 'tool_result' })
    $finalEvents = @($activityEvents | Where-Object { $_.observation_layer -eq 'llm_final_result' })
    $processEvents = @($activityOsEvents | Where-Object { $_.resource.kind -eq 'process' })
    $fileEvents = @($activityOsEvents | Where-Object { $_.resource.kind -eq 'file' })
    $networkEvents = @($activityOsEvents | Where-Object { $_.resource.kind -eq 'network' })
    $correlationRelevant = @($activityCorrelations | Where-Object { $null -ne $_.expected_os_action })
    $confirmedCorrelations = @($correlationRelevant | Where-Object { $_.correlation_status -eq 'confirmed' }).Count
    $partialCorrelations = @($correlationRelevant | Where-Object { $_.correlation_status -eq 'partial' }).Count
    $unlinkedCorrelations = @($correlationRelevant | Where-Object { $_.correlation_status -eq 'unlinked' }).Count
    $credentialFindings = @($activityCorrelations | ForEach-Object { @($_.credential_findings) })
    $prompt = @($promptEvents | Where-Object { $_.content_kind -eq 'raw_user_prompt' -and $null -eq $_.tool_name } | Select-Object -First 1)
    $promptText = if ($prompt.Count -eq 0) { '未捕获原始用户输入' } else { [string]$prompt[0].content }
    $processBody = New-OsRows -Records $processEvents
    $fileBody = (New-OsRows -Records $fileEvents) + "<p class='note'>凭据命中：$($credentialFindings.Count)。文件内容仅在语义参数/结果或上下文快照中出现时可见；ETW 本身只提供文件元数据。</p>"
    $networkBody = (New-OsRows -Records $networkEvents) + "<p class='note'>当前内核证据为 TCP 目标与字节数；DNS、域名、TLS 身份和加密载荷仍需专用且可按 PID 归因的数据源。</p>"
    $coverageStatus = if ($collectorSummary.coverage_status -ne 'healthy') { '缺失' } elseif ($partialCorrelations -gt 0 -or $unlinkedCorrelations -gt 0) { '部分观测' } else { '已观测' }
    $coverageBody = "<dl><dt>coverage_status</dt><dd>$(Convert-HtmlText -Value ([string]$collectorSummary.coverage_status))</dd><dt>events_lost</dt><dd>$($collectorSummary.events_lost)</dd><dt>parse_failures</dt><dd>$($collectorSummary.parse_failures)</dd><dt>write_failures</dt><dd>$($collectorSummary.write_failures)</dd><dt>output_batches_dropped</dt><dd>$($collectorSummary.output_batches_dropped)</dd><dt>confirmed / partial / unlinked</dt><dd>$confirmedCorrelations / $partialCorrelations / $unlinkedCorrelations</dd><dt>展示时间范围</dt><dd>$(Convert-HtmlText -Value (Convert-UnixTime -UnixMilliseconds $activityStart)) — $(Convert-HtmlText -Value (Convert-UnixTime -UnixMilliseconds $activityEnd))</dd></dl>"

    $layers = @(
        (New-LayerSection -Number 1 -Title 'User Prompt' -Status (Get-LayerStatus -Records $promptEvents -EmptyStatus '缺失') -Gap '记录原始输入、时间和可见附件；UI 输入由用户手工完成。' -Body (New-SemanticRows -Records $promptEvents)),
        (New-LayerSection -Number 2 -Title 'Base/System Instructions' -Status (Get-LayerStatus -Records $systemEvents -EmptyStatus '缺失') -Gap '记录 WorkBuddy 落盘的包装提示；无法证明服务端另加的隐藏系统提示。' -Body (New-SemanticRows -Records $systemEvents)),
        (New-LayerSection -Number 3 -Title 'Skills and Agent Policy' -Status '部分观测' -Gap '组合请求包装中的规则与会话开始时的本地策略/Skill 快照。' -Body ((New-SemanticRows -Records $policyEvents) + (New-ContextRows -Artifacts @($contextSnapshot.artifacts) -Categories @('agent_policy', 'skill_policy')))),
        (New-LayerSection -Number 4 -Title 'Available Tools/MCP' -Status '部分观测' -Gap '记录连接器配置和实际被选择的 Tool；内置工具完整 Schema/版本若未落盘则仍缺失。' -Body ((New-SemanticRows -Records $toolsEvents) + (New-ContextRows -Artifacts @($contextSnapshot.artifacts) -Categories @('mcp_configuration', 'agent_configuration')))),
        (New-LayerSection -Number 5 -Title 'LLM Request/Response' -Status (Get-LayerStatus -Records $llmEvents -EmptyStatus '缺失') -Gap '记录客户端私有会话中的请求包装、模型响应、错误和用量；不等同于网络线上字节。' -Body (New-SemanticRows -Records $llmEvents)),
        (New-LayerSection -Number 6 -Title 'Tool/MCP Calls' -Status (Get-LayerStatus -Records $toolCallEvents -EmptyStatus '本次未发生') -Gap '记录 Tool 名称、参数和 callId；MCP 传输层需结合子进程和网络证据判断。' -Body (New-SemanticRows -Records $toolCallEvents)),
        (New-LayerSection -Number 7 -Title 'Process and Script' -Status $(if ($processEvents.Count -gt 0) { '部分观测' } else { '本次未发生' }) -Gap '仅保留目标 Agent 进程树；命令行可见，但标准输入正文只能从语义参数交叉证明。' -Body $processBody),
        (New-LayerSection -Number 8 -Title 'Filesystem/Credential' -Status $(if ($fileEvents.Count -gt 0 -or $credentialFindings.Count -gt 0) { '部分观测' } else { '本次未发生' }) -Gap 'ETW 提供文件动作元数据；语义与上下文内容用于凭据正文匹配，不能代替逐字节文件读取证明。' -Body $fileBody),
        (New-LayerSection -Number 9 -Title 'Network' -Status $(if ($networkEvents.Count -gt 0) { '部分观测' } else { '本次未发生' }) -Gap '只接受可按目标 PID 归因的网络证据；不落盘系统级、无法归因的 DNS 缓存。' -Body $networkBody),
        (New-LayerSection -Number 10 -Title 'ToolResult' -Status (Get-LayerStatus -Records $toolResultEvents -EmptyStatus '本次未发生') -Gap '记录返回正文、状态和错误；副作用由 OS 证据交叉验证。' -Body (New-SemanticRows -Records $toolResultEvents)),
        (New-LayerSection -Number 11 -Title 'LLM Final Result' -Status (Get-LayerStatus -Records $finalEvents -EmptyStatus '缺失') -Gap '每条用户行为取下一条用户输入前最后一个 assistant message；若窗口提前结束则不得认定完整。' -Body (New-SemanticRows -Records $finalEvents)),
        (New-LayerSection -Number 12 -Title 'Evidence Coverage' -Status $coverageStatus -Gap '零丢失、零解析/写入/队列丢弃只是必要条件；各层缺口仍单独保留。' -Body $coverageBody)
    )
    "<article class='activity'><div class='activity-head'><p>用户行为 $($index + 1)</p><h2>$(Convert-HtmlText -Value $promptText)</h2><code>$(Convert-HtmlText -Value $activityId)</code></div>$($layers -join '')</article>"
}

$generatedAt = [DateTimeOffset]::Now.ToString('yyyy-MM-dd HH:mm:ss zzz')
$html = @"
<!doctype html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>AgentReins WorkBuddy 12 层观测报告 - $(Convert-HtmlText -Value $RunId)</title>
<style>
:root{color-scheme:light;--ink:#172033;--muted:#667085;--line:#d9e1ec;--panel:#fff;--bg:#f3f6fa;--blue:#175cd3;--green:#067647;--amber:#b54708;--red:#b42318}*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--ink);font:14px/1.55 system-ui,-apple-system,"Segoe UI","Microsoft YaHei",sans-serif}main{width:min(1500px,calc(100% - 32px));margin:28px auto 60px}.hero,.activity{background:var(--panel);border:1px solid var(--line);border-radius:16px;box-shadow:0 8px 30px rgba(31,50,81,.06)}.hero{padding:28px;margin-bottom:20px}.hero h1{margin:0 0 8px;font-size:28px}.hero p{margin:4px 0;color:var(--muted)}.activity{padding:22px;margin-top:20px}.activity-head{border-bottom:2px solid var(--ink);padding-bottom:16px;margin-bottom:16px}.activity-head p{margin:0;color:var(--blue);font-weight:700}.activity-head h2{margin:6px 0 8px;font-size:20px}.layer{border:1px solid var(--line);border-radius:12px;padding:16px;margin:12px 0}.layer header{display:flex;align-items:center;gap:10px}.layer h3{font-size:16px;margin:0;flex:1}.number{display:grid;place-items:center;width:28px;height:28px;border-radius:8px;background:#eaf2ff;color:var(--blue);font-weight:800}.status{padding:3px 9px;border-radius:99px;font-size:12px;font-weight:700}.status.observed{background:#dcfae6;color:var(--green)}.status.partial{background:#fffaeb;color:var(--amber)}.status.missing{background:#fee4e2;color:var(--red)}.gap,.note,.empty{color:var(--muted)}.table-wrap{overflow:auto;border:1px solid var(--line);border-radius:8px}table{border-collapse:collapse;width:100%;min-width:850px}th,td{text-align:left;vertical-align:top;padding:9px 10px;border-bottom:1px solid var(--line)}th{background:#f8fafc;position:sticky;top:0}tr:last-child td{border-bottom:0}pre{white-space:pre-wrap;word-break:break-word;max-height:420px;overflow:auto;background:#0b1220;color:#e6edf7;padding:12px;border-radius:8px}code{word-break:break-all}dl{display:grid;grid-template-columns:max-content 1fr;gap:6px 16px}dt{font-weight:700}dd{margin:0}@media(max-width:700px){main{width:min(100% - 16px,1500px);margin-top:8px}.hero,.activity{border-radius:10px;padding:15px}.layer{padding:12px}.layer header{align-items:flex-start}.status{white-space:nowrap}}
</style>
</head>
<body><main>
<section class="hero"><h1>WorkBuddy 12 层观测报告</h1><p>Run ID：$(Convert-HtmlText -Value $RunId)</p><p>生成时间：$(Convert-HtmlText -Value $generatedAt)</p><p>报告按用户输入划分行为时间线；所有结论均保留证据来源与缺口，不包含阻断或治理判断。</p></section>
$($activitySections -join "`n")
</main></body></html>
"@

[IO.File]::WriteAllText([IO.Path]::GetFullPath($OutputPath), $html, [Text.UTF8Encoding]::new($false))
[pscustomobject]@{
    status = 'completed'
    run_id = $RunId
    user_activity_count = $activityIds.Count
    semantic_event_count = $semanticEvents.Count
    os_event_count = $osEvents.Count
    output_path = [IO.Path]::GetFullPath($OutputPath)
} | ConvertTo-Json -Compress
