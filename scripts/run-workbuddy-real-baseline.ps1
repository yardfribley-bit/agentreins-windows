param(
    [Parameter(Mandatory = $true)]
    [string]$SourceRoot,
    [Parameter(Mandatory = $true)]
    [string]$EvidenceRoot,
    [Parameter(Mandatory = $true)]
    [string]$WorkBuddyPath,
    [Parameter(Mandatory = $true)]
    [string]$ExpectedSha256,
    [Parameter(Mandatory = $true)]
    [string]$ExpectedSignerThumbprint,
    [Parameter(Mandatory = $true)]
    [string]$InteractiveUser,
    [Parameter(Mandatory = $true)]
    [int]$InteractiveSessionId
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$collectorPath = Join-Path $SourceRoot "target\debug\etw-collector.exe"
if (-not (Test-Path -LiteralPath $collectorPath)) {
    throw "未找到 ETW Collector path=$collectorPath"
}
if (-not (Test-Path -LiteralPath $WorkBuddyPath)) {
    throw "未找到 WorkBuddy 主程序 path=$WorkBuddyPath"
}
if (@(Get-Process | Where-Object { $_.ProcessName -eq "WorkBuddy" }).Count -ne 0) {
    throw "WorkBuddy 已在运行，拒绝混入既有会话"
}

$actualSha256 = (Get-FileHash -LiteralPath $WorkBuddyPath -Algorithm SHA256).Hash
if ($actualSha256 -ne $ExpectedSha256) {
    throw "WorkBuddy 哈希不匹配 path=$WorkBuddyPath expected=$ExpectedSha256 actual=$actualSha256"
}
$signature = Get-AuthenticodeSignature -LiteralPath $WorkBuddyPath
if ($signature.Status -ne "Valid" -or $signature.SignerCertificate.Thumbprint -ne $ExpectedSignerThumbprint) {
    throw "WorkBuddy 签名不匹配 path=$WorkBuddyPath status=$($signature.Status) thumbprint=$($signature.SignerCertificate.Thumbprint)"
}
$workBuddyVersion = (Get-Item -LiteralPath $WorkBuddyPath).VersionInfo.FileVersion

New-Item -ItemType Directory -Path $EvidenceRoot -Force | Out-Null
$fullNdjsonPath = Join-Path $EvidenceRoot "workbuddy-real-process.full.ndjson"
$filteredNdjsonPath = Join-Path $EvidenceRoot "workbuddy-real-process.filtered.ndjson"
$fullCsvPath = Join-Path $EvidenceRoot "workbuddy-real-process.full.csv"
$filteredCsvPath = Join-Path $EvidenceRoot "workbuddy-real-process.filtered.csv"
$stdoutPath = Join-Path $EvidenceRoot "workbuddy-real-collector-stdout.json"
$stderrPath = Join-Path $EvidenceRoot "workbuddy-real-collector-stderr.ndjson"
$baselinePath = Join-Path $EvidenceRoot "workbuddy-real-baseline.json"
$baselineFullCsvPath = Join-Path $EvidenceRoot "workbuddy-real-baseline.full.csv"
$baselineFilteredCsvPath = Join-Path $EvidenceRoot "workbuddy-real-baseline.filtered.csv"
$healthFullCsvPath = Join-Path $EvidenceRoot "collector-health.full.csv"
$healthFilteredCsvPath = Join-Path $EvidenceRoot "collector-health.filtered.csv"
$manifestPath = Join-Path $EvidenceRoot "manifest.csv"
$identityPath = Join-Path $EvidenceRoot "workbuddy-identity.json"
$exporterPath = Join-Path $SourceRoot "scripts\export-process-evidence-csv.ps1"
if (-not (Test-Path -LiteralPath $exporterPath)) {
    throw "未找到 CSV 导出脚本 path=$exporterPath"
}
foreach ($artifactPath in @($fullNdjsonPath, $filteredNdjsonPath, $fullCsvPath, $filteredCsvPath, $stdoutPath, $stderrPath, $baselinePath, $baselineFullCsvPath, $baselineFilteredCsvPath, $healthFullCsvPath, $healthFilteredCsvPath, $manifestPath, $identityPath)) {
    if (Test-Path -LiteralPath $artifactPath) {
        Remove-Item -LiteralPath $artifactPath
    }
}
$workBuddyIdentity = [pscustomobject]@{
    executable_path = $WorkBuddyPath
    file_version = $workBuddyVersion
    sha256 = $actualSha256.ToLowerInvariant()
    signer_subject = $signature.SignerCertificate.Subject
}
[IO.File]::WriteAllText($identityPath, ($workBuddyIdentity | ConvertTo-Json), [Text.UTF8Encoding]::new($false))

$collector = $null
$taskName = "AgentReins-WorkBuddy-Baseline-$PID"
$taskCreated = $false
$launchedProcessIds = @()
try {
    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $collectorPath
    $startInfo.Arguments = "--duration-seconds 18 --full-output $fullNdjsonPath --filtered-output $filteredNdjsonPath --identity $identityPath"
    $startInfo.UseShellExecute = $false
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    $collector = [System.Diagnostics.Process]::Start($startInfo)
    Start-Sleep -Seconds 2

    $createOutput = & schtasks.exe /Create /TN $taskName /TR "`"$WorkBuddyPath`"" /SC ONCE /ST "23:59" /RU $InteractiveUser /RL LIMITED /IT /F 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "创建交互式启动任务失败 task=$taskName output=$createOutput exit_code=$LASTEXITCODE"
    }
    $taskCreated = $true
    $runOutput = & schtasks.exe /Run /TN $taskName 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "启动交互式任务失败 task=$taskName output=$runOutput exit_code=$LASTEXITCODE"
    }

    $launchDeadline = (Get-Date).AddSeconds(15)
    do {
        Start-Sleep -Milliseconds 500
        $launchedProcesses = @(
            Get-Process |
                Where-Object { $_.ProcessName -eq "WorkBuddy" -and $_.SessionId -eq $InteractiveSessionId }
        )
    } while ($launchedProcesses.Count -eq 0 -and (Get-Date) -lt $launchDeadline)
    if ($launchedProcesses.Count -eq 0) {
        throw "WorkBuddy 未在交互会话启动 user=$InteractiveUser session_id=$InteractiveSessionId"
    }
    $launchedProcessIds = @($launchedProcesses.Id)

    $beforeCpu = ($launchedProcesses | Measure-Object CPU -Sum).Sum
    Start-Sleep -Seconds 5
    $sampledProcesses = @(
        Get-Process |
            Where-Object { $_.ProcessName -eq "WorkBuddy" -and $_.SessionId -eq $InteractiveSessionId }
    )
    $afterCpu = ($sampledProcesses | Measure-Object CPU -Sum).Sum
    $sampledProcessIds = @($sampledProcesses.Id)
    $connections = @(
        Get-NetTCPConnection |
            Where-Object { $sampledProcessIds -contains $_.OwningProcess }
    )
    $logicalProcessors = (Get-CimInstance Win32_ComputerSystem).NumberOfLogicalProcessors
    $resourceSample = [pscustomobject]@{
        sample_seconds = 5
        process_count = $sampledProcesses.Count
        working_set_mb = [math]::Round((($sampledProcesses | Measure-Object WorkingSet64 -Sum).Sum / 1MB), 2)
        private_memory_mb = [math]::Round((($sampledProcesses | Measure-Object PrivateMemorySize64 -Sum).Sum / 1MB), 2)
        cpu_percent_normalized = [math]::Round((($afterCpu - $beforeCpu) / 5 / $logicalProcessors * 100), 3)
        established_connections = @($connections | Where-Object { $_.State -eq "Established" }).Count
        loopback_listeners = @($connections | Where-Object { $_.State -eq "Listen" -and $_.LocalAddress -in @("127.0.0.1", "::1") }).Count
    }

    $sampledProcesses | Stop-Process -Force
    $stopDeadline = (Get-Date).AddSeconds(15)
    do {
        Start-Sleep -Milliseconds 500
        $remainingProcesses = @(
            Get-Process |
                Where-Object { $_.ProcessName -eq "WorkBuddy" -and $_.SessionId -eq $InteractiveSessionId }
        )
    } while ($remainingProcesses.Count -gt 0 -and (Get-Date) -lt $stopDeadline)
    if ($remainingProcesses.Count -gt 0) {
        throw "WorkBuddy 受控停止失败 remaining_process_ids=$($remainingProcesses.Id -join ',')"
    }

    if (-not $collector.WaitForExit(30000)) {
        throw "等待 ETW Collector 退出超时 process_id=$($collector.Id)"
    }
    $collectorStandardOutput = $collector.StandardOutput.ReadToEnd()
    $collectorStandardError = $collector.StandardError.ReadToEnd()
    [IO.File]::WriteAllText($stdoutPath, $collectorStandardOutput, [Text.UTF8Encoding]::new($false))
    [IO.File]::WriteAllText($stderrPath, $collectorStandardError, [Text.UTF8Encoding]::new($false))
}
finally {
    $remainingWorkBuddy = @(
        Get-Process |
            Where-Object { $_.ProcessName -eq "WorkBuddy" -and $_.SessionId -eq $InteractiveSessionId }
    )
    if ($remainingWorkBuddy.Count -gt 0) {
        $remainingWorkBuddy | Stop-Process -Force
    }
    if ($taskCreated) {
        $deleteOutput = & schtasks.exe /Delete /TN $taskName /F 2>&1
        if ($LASTEXITCODE -ne 0) {
            Write-Warning "删除交互式启动任务失败 task=$taskName output=$deleteOutput"
        }
    }
    if ($null -ne $collector -and -not $collector.HasExited) {
        Stop-Process -Id $collector.Id -Force
        $traceName = "AgentReins-Gate0-$($collector.Id)"
        $stopOutput = & logman.exe stop $traceName -ets 2>&1
        if ($LASTEXITCODE -ne 0) {
            Write-Warning "清理 ETW 会话失败 trace_name=$traceName output=$stopOutput"
        }
    }
}

if ($collector.ExitCode -ne 0) {
    throw "ETW Collector 失败 exit_code=$($collector.ExitCode) stderr=$collectorStandardError"
}
$exportSummary = & $exporterPath -FullInputPath $fullNdjsonPath -FilteredInputPath $filteredNdjsonPath -FullCsvPath $fullCsvPath -FilteredCsvPath $filteredCsvPath |
    ConvertFrom-Json
$fullEventCount = $exportSummary.full_rows
$filteredEventCount = $exportSummary.filtered_rows
$startCount = $exportSummary.start_count
$stopCount = $exportSummary.stop_count
$candidateCount = $exportSummary.candidate_count
$verifiedCount = $exportSummary.verified_count
$verifiedStartCount = $exportSummary.verified_start_count
$verifiedStopCount = $exportSummary.verified_stop_count
$inheritedCount = $exportSummary.inherited_count
$fileActionCount = $exportSummary.file_action_count
$networkActionCount = $exportSummary.network_action_count
$invalidTimestampCount = $exportSummary.invalid_timestamp_count
$imageNames = @($exportSummary.image_names)
$result = [pscustomobject]@{
    captured_at = (Get-Date).ToString("o")
    workbuddy_version = $workBuddyVersion
    workbuddy_sha256 = $actualSha256
    signer_subject = $signature.SignerCertificate.Subject
    interactive_user = $InteractiveUser
    interactive_session_id = $InteractiveSessionId
    launched_process_ids = $launchedProcessIds
    exit_method = "controlled_force"
    resource_sample = $resourceSample
    full_event_count = $fullEventCount
    filtered_event_count = $filteredEventCount
    start_count = $startCount
    stop_count = $stopCount
    candidate_count = $candidateCount
    verified_count = $verifiedCount
    verified_start_count = $verifiedStartCount
    verified_stop_count = $verifiedStopCount
    inherited_count = $inheritedCount
    file_action_count = $fileActionCount
    network_action_count = $networkActionCount
    invalid_timestamp_count = $invalidTimestampCount
    image_names = @($imageNames | Sort-Object)
    stderr_bytes = (Get-Item -LiteralPath $stderrPath).Length
    remaining_processes = @(
        Get-Process |
            Where-Object { $_.ProcessName -eq "WorkBuddy" -and $_.SessionId -eq $InteractiveSessionId }
    ).Count
    remaining_trace_sessions = @(& logman.exe query -ets | Select-String "AgentReins-Gate0").Count
    remaining_scheduled_tasks = @(Get-ScheduledTask -TaskName $taskName -ErrorAction SilentlyContinue).Count
}
$result | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $baselinePath -Encoding UTF8
$result | Select-Object captured_at,workbuddy_version,workbuddy_sha256,signer_subject,interactive_user,interactive_session_id,@{Name="launched_process_ids";Expression={$_.launched_process_ids -join ";"}},exit_method,@{Name="sample_seconds";Expression={$_.resource_sample.sample_seconds}},@{Name="process_count";Expression={$_.resource_sample.process_count}},@{Name="working_set_mb";Expression={$_.resource_sample.working_set_mb}},@{Name="private_memory_mb";Expression={$_.resource_sample.private_memory_mb}},@{Name="cpu_percent_normalized";Expression={$_.resource_sample.cpu_percent_normalized}},@{Name="established_connections";Expression={$_.resource_sample.established_connections}},@{Name="loopback_listeners";Expression={$_.resource_sample.loopback_listeners}},full_event_count,filtered_event_count,start_count,stop_count,candidate_count,verified_count,verified_start_count,verified_stop_count,inherited_count,file_action_count,network_action_count,invalid_timestamp_count,@{Name="image_names";Expression={$_.image_names -join ";"}},stderr_bytes,remaining_processes,remaining_trace_sessions,remaining_scheduled_tasks | Export-Csv -LiteralPath $baselineFullCsvPath -NoTypeInformation -Encoding UTF8
$result | Select-Object captured_at,workbuddy_version,signer_subject,exit_method,@{Name="sample_seconds";Expression={$_.resource_sample.sample_seconds}},@{Name="process_count";Expression={$_.resource_sample.process_count}},@{Name="working_set_mb";Expression={$_.resource_sample.working_set_mb}},@{Name="private_memory_mb";Expression={$_.resource_sample.private_memory_mb}},@{Name="cpu_percent_normalized";Expression={$_.resource_sample.cpu_percent_normalized}},@{Name="established_connections";Expression={$_.resource_sample.established_connections}},@{Name="loopback_listeners";Expression={$_.resource_sample.loopback_listeners}},full_event_count,filtered_event_count,start_count,stop_count,candidate_count,verified_count,inherited_count,file_action_count,network_action_count,invalid_timestamp_count,stderr_bytes,remaining_processes,remaining_trace_sessions,remaining_scheduled_tasks | Export-Csv -LiteralPath $baselineFilteredCsvPath -NoTypeInformation -Encoding UTF8
$collectorSummary = Get-Content -LiteralPath $stdoutPath -Raw -Encoding UTF8 | ConvertFrom-Json
$healthFull = [pscustomobject]@{
    captured_at = $result.captured_at
    session_id = $collectorSummary.session_id
    full_output_path = $collectorSummary.full_output_path
    filtered_output_path = $collectorSummary.filtered_output_path
    provider_events_received = $collectorSummary.provider_events_received
    events_handled = $collectorSummary.events_handled
    parsed_events = $collectorSummary.parsed_events
    process_events_parsed = $collectorSummary.process_events_parsed
    thread_events_parsed = $collectorSummary.thread_events_parsed
    file_events_parsed = $collectorSummary.file_events_parsed
    network_events_parsed = $collectorSummary.network_events_parsed
    full_events_written = $collectorSummary.full_events_written
    filtered_events_written = $collectorSummary.filtered_events_written
    parse_failures = $collectorSummary.parse_failures
    write_failures = $collectorSummary.write_failures
    output_queue_capacity = $collectorSummary.output_queue_capacity
    peak_output_queue_depth = $collectorSummary.peak_output_queue_depth
    output_source_events = $collectorSummary.output_source_events
    output_batches_generated = $collectorSummary.output_batches_generated
    source_events_collapsed = $collectorSummary.source_events_collapsed
    output_batches_dropped = $collectorSummary.output_batches_dropped
    retained_detail_source_events = $collectorSummary.retained_detail_source_events
    retained_summary_source_events = $collectorSummary.retained_summary_source_events
    retained_review_source_events = $collectorSummary.retained_review_source_events
    discarded_source_events = $collectorSummary.discarded_source_events
    output_queue_wait_count = $collectorSummary.output_queue_wait_count
    output_queue_wait_ms = $collectorSummary.output_queue_wait_ms
    peak_aggregation_keys = $collectorSummary.peak_aggregation_keys
    identity_collision_count = $collectorSummary.identity_collision_count
    events_lost = $collectorSummary.events_lost
    log_buffers_lost = $collectorSummary.log_buffers_lost
    realtime_buffers_lost = $collectorSummary.realtime_buffers_lost
    buffers_written = $collectorSummary.buffers_written
    coverage_status = $collectorSummary.coverage_status
    stderr_bytes = $result.stderr_bytes
}
$healthFull |
    Export-Csv -LiteralPath $healthFullCsvPath -NoTypeInformation -Encoding UTF8
$healthFull |
    Select-Object captured_at,session_id,provider_events_received,events_handled,parsed_events,process_events_parsed,thread_events_parsed,file_events_parsed,network_events_parsed,full_events_written,filtered_events_written,parse_failures,write_failures,output_queue_capacity,peak_output_queue_depth,output_source_events,output_batches_generated,source_events_collapsed,output_batches_dropped,retained_detail_source_events,retained_summary_source_events,retained_review_source_events,discarded_source_events,output_queue_wait_count,output_queue_wait_ms,peak_aggregation_keys,identity_collision_count,events_lost,log_buffers_lost,realtime_buffers_lost,buffers_written,coverage_status,stderr_bytes |
    Export-Csv -LiteralPath $healthFilteredCsvPath -NoTypeInformation -Encoding UTF8
$manifestRows = @(
    [pscustomobject]@{
        dataset = "workbuddy-real-process"
        captured_at = $result.captured_at
        full_csv = (Split-Path -Leaf $fullCsvPath)
        filtered_csv = (Split-Path -Leaf $filteredCsvPath)
        full_rows = $result.full_event_count
        filtered_rows = $result.filtered_event_count
        prefilter_full_available = $true
        source_host = $env:COMPUTERNAME
        filter_rule = "WorkBuddy 实例归因；有效明细保留；背景活动聚合；重复或无事实事件丢弃"
    },
    [pscustomobject]@{
        dataset = "workbuddy-real-baseline"
        captured_at = $result.captured_at
        full_csv = (Split-Path -Leaf $baselineFullCsvPath)
        filtered_csv = (Split-Path -Leaf $baselineFilteredCsvPath)
        full_rows = 1
        filtered_rows = 1
        prefilter_full_available = "not_applicable"
        source_host = $env:COMPUTERNAME
        filter_rule = "过滤版移除用户、进程 ID 和文件哈希"
    },
    [pscustomobject]@{
        dataset = "collector-health"
        captured_at = $result.captured_at
        full_csv = (Split-Path -Leaf $healthFullCsvPath)
        filtered_csv = (Split-Path -Leaf $healthFilteredCsvPath)
        full_rows = 1
        filtered_rows = 1
        prefilter_full_available = "not_applicable"
        source_host = $env:COMPUTERNAME
        filter_rule = "过滤版移除本地输出路径"
    }
)
$manifestRows |
    Export-Csv -LiteralPath $manifestPath -NoTypeInformation -Encoding UTF8
$result | ConvertTo-Json -Depth 4 -Compress

if (
    $result.filtered_event_count -lt 2 -or
    $result.start_count -lt 1 -or
    $result.stop_count -lt 1 -or
    $verifiedStartCount -lt 1 -or
    $verifiedStartCount -ne $verifiedStopCount -or
    $result.file_action_count -lt 1 -or
    $result.invalid_timestamp_count -ne 0 -or
    $collectorSummary.coverage_status -ne "healthy" -or
    $collectorSummary.output_batches_dropped -ne 0 -or
    $result.full_event_count -lt $result.filtered_event_count -or
    (Import-Csv -LiteralPath $fullCsvPath | Measure-Object).Count -ne $result.full_event_count -or
    (Import-Csv -LiteralPath $filteredCsvPath | Measure-Object).Count -ne $result.filtered_event_count
) {
    throw "WorkBuddy ETW 生命周期不完整 result=$($result | ConvertTo-Json -Depth 4 -Compress)"
}
if ($result.stderr_bytes -ne 0 -or $result.remaining_processes -ne 0 -or $result.remaining_trace_sessions -ne 0 -or $result.remaining_scheduled_tasks -ne 0) {
    throw "WorkBuddy 基线存在警告或残留状态 result=$($result | ConvertTo-Json -Depth 4 -Compress)"
}
