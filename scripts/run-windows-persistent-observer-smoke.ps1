param(
    [Parameter(Mandatory = $true)]
    [string]$SourceRoot,
    [Parameter(Mandatory = $true)]
    [string]$EvidenceRoot,
    [Parameter(Mandatory = $true)]
    [long]$RollSizeBytes,
    [Parameter(Mandatory = $true)]
    [int]$RollIntervalSeconds
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$source = [IO.Path]::GetFullPath($SourceRoot)
$evidence = [IO.Path]::GetFullPath($EvidenceRoot)
if ($RollSizeBytes -lt 1 -or $RollIntervalSeconds -lt 1) {
    throw "滚动参数必须为正整数 roll_size_bytes=$RollSizeBytes roll_interval_seconds=$RollIntervalSeconds"
}
$collectorPath = Join-Path $source "target\debug\etw-collector.exe"
$fixturePath = Join-Path $source "target\debug\WorkBuddy.exe"
foreach ($requiredPath in @($collectorPath, $fixturePath)) {
    if (-not (Test-Path -LiteralPath $requiredPath)) {
        throw "未找到常驻观测烟测依赖 path=$requiredPath"
    }
}
if (Test-Path -LiteralPath $evidence) {
    throw "证据目录已存在，拒绝覆盖 path=$evidence"
}
if (@(Get-Process | Where-Object { $_.ProcessName -eq "WorkBuddy" -and $_.Path -eq $fixturePath }).Count -ne 0) {
    throw "发现已运行的 WorkBuddy 测试夹具，拒绝混入既有夹具实例 path=$fixturePath"
}
New-Item -ItemType Directory -Path $evidence | Out-Null

$paths = [ordered]@{
    identity = Join-Path $evidence "fixture-identity.json"
    stop_signal = Join-Path $evidence "observer-stop.requested"
    health = Join-Path $evidence "observer-health.ndjson"
    output_directory = Join-Path $evidence "segments"
    stdout = Join-Path $evidence "observer-stdout.json"
    stderr = Join-Path $evidence "observer-stderr.ndjson"
}
$identity = [pscustomobject]@{
    executable_path = $fixturePath
    file_version = "0.1.0"
    sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $fixturePath).Hash.ToLowerInvariant()
    signer_subject = "UNSIGNED_TEST_FIXTURE"
}
[IO.File]::WriteAllText(
    $paths.identity,
    ($identity | ConvertTo-Json),
    [Text.UTF8Encoding]::new($false)
)

$collector = $null
try {
    $collectorArguments = "--observe-until-stopped --stop-signal $($paths.stop_signal) --health-output $($paths.health) --health-interval-seconds 1 --output-directory $($paths.output_directory) --roll-size-bytes $RollSizeBytes --roll-interval-seconds $RollIntervalSeconds --identity $($paths.identity)"
    $startInfo = [Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $collectorPath
    $startInfo.Arguments = $collectorArguments
    $startInfo.UseShellExecute = $false
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    $collector = [Diagnostics.Process]::Start($startInfo)

    $healthDeadline = [DateTime]::UtcNow.AddSeconds(10)
    while ((-not (Test-Path -LiteralPath $paths.health) -or (Get-Item -LiteralPath $paths.health).Length -eq 0) -and [DateTime]::UtcNow -lt $healthDeadline) {
        if ($collector.HasExited) {
            $earlyStdout = $collector.StandardOutput.ReadToEnd()
            $earlyStderr = $collector.StandardError.ReadToEnd()
            [IO.File]::WriteAllText($paths.stdout, $earlyStdout, [Text.UTF8Encoding]::new($false))
            [IO.File]::WriteAllText($paths.stderr, $earlyStderr, [Text.UTF8Encoding]::new($false))
            throw "常驻 Observer 在首个健康快照前退出 exit_code=$($collector.ExitCode) stderr=$earlyStderr"
        }
        Start-Sleep -Milliseconds 100
    }
    if (-not (Test-Path -LiteralPath $paths.health) -or (Get-Item -LiteralPath $paths.health).Length -eq 0) {
        throw "常驻 Observer 未在期限内写入首个健康快照 process_id=$($collector.Id)"
    }

    & $fixturePath
    if ($LASTEXITCODE -ne 0) {
        throw "第一轮 WorkBuddy 测试夹具失败 exit_code=$LASTEXITCODE"
    }
    Start-Sleep -Seconds 1
    & $fixturePath
    if ($LASTEXITCODE -ne 0) {
        throw "第二轮 WorkBuddy 测试夹具失败 exit_code=$LASTEXITCODE"
    }
    Start-Sleep -Seconds 2
    [IO.File]::WriteAllText($paths.stop_signal, "stop", [Text.UTF8Encoding]::new($false))
    if (-not $collector.WaitForExit(15000)) {
        throw "常驻 Observer 未在停止信号后退出 process_id=$($collector.Id)"
    }
    [IO.File]::WriteAllText($paths.stdout, $collector.StandardOutput.ReadToEnd(), [Text.UTF8Encoding]::new($false))
    [IO.File]::WriteAllText($paths.stderr, $collector.StandardError.ReadToEnd(), [Text.UTF8Encoding]::new($false))
}
finally {
    if ($null -ne $collector -and -not $collector.HasExited) {
        $traceName = "AgentReins-Gate0-$($collector.Id)"
        & logman.exe stop $traceName -ets 2>&1 | Out-Null
        Stop-Process -Id $collector.Id -Force
    }
}

if ($collector.ExitCode -ne 0) {
    throw "常驻 Observer 失败 exit_code=$($collector.ExitCode) stderr=$(Get-Content -LiteralPath $paths.stderr -Raw -Encoding UTF8)"
}
$summary = Get-Content -LiteralPath $paths.stdout -Raw -Encoding UTF8 | ConvertFrom-Json
$health = @(Get-Content -LiteralPath $paths.health -Encoding UTF8 | ForEach-Object { $_ | ConvertFrom-Json })
$permissionProcesses = @($health | ForEach-Object { $_.agent_resource_sample.permission_processes })
$permissionProcessesMissingIntegrity = @($permissionProcesses | Where-Object {
    $_.integrity_rid -le 0 -or [string]::IsNullOrWhiteSpace([string]$_.integrity_level)
})
$permissionPrivileges = @($permissionProcesses | ForEach-Object { $_.privileges })
$fullFiles = @(Get-ChildItem -LiteralPath $paths.output_directory -Filter "*.full.ndjson" | Sort-Object Name)
$filteredFiles = @(Get-ChildItem -LiteralPath $paths.output_directory -Filter "*.filtered.ndjson" | Sort-Object Name)
$manifestFiles = @(Get-ChildItem -LiteralPath $paths.output_directory -Filter "*-manifest-*.json" | Sort-Object Name)
$events = @($filteredFiles | ForEach-Object { Get-Content -LiteralPath $_.FullName -Encoding UTF8 } | ForEach-Object { $_ | ConvertFrom-Json })
$finalManifest = Get-Content -LiteralPath $manifestFiles[-1].FullName -Raw -Encoding UTF8 | ConvertFrom-Json
$manifestFilesPresent = @($finalManifest.segments | Where-Object {
    (Test-Path -LiteralPath (Join-Path $paths.output_directory $_.full_file)) -and
    (Test-Path -LiteralPath (Join-Path $paths.output_directory $_.filtered_file))
}).Count
$rootStarts = @($events | Where-Object { $_.phase -eq "live" -and $_.action.kind -eq "process_start" -and $_.actor.identity_status -eq "verified" })
$rootStops = @($events | Where-Object { $_.phase -eq "live" -and $_.action.kind -eq "process_stop" -and $_.actor.identity_status -eq "verified" })
$rootInstances = @($rootStarts | ForEach-Object { $_.actor.root_process_instance_id } | Sort-Object -Unique)
$aggregatedReads = @(
    $events | Where-Object {
        $_.process.image_name -eq "WorkBuddy.exe" -and
        $_.action.kind -eq "file_read" -and
        $_.aggregation.occurrence_count -ge 2
    }
)
$networkEvents = @($events | Where-Object { $_.resource.kind -eq 'network' })
$networkEventsMissingOperationId = @($networkEvents | Where-Object {
    $_.resource.kind -eq 'network' -and [string]::IsNullOrWhiteSpace([string]$_.operation_id)
})
$networkLifecycleOperationCount = @(
    $networkEvents |
        Where-Object { -not [string]::IsNullOrWhiteSpace([string]$_.operation_id) } |
        Group-Object operation_id |
        Where-Object {
            $actionKinds = @($_.Group | ForEach-Object { $_.action.kind } | Sort-Object -Unique)
            @($actionKinds | Where-Object { $_ -in @('network_connect', 'network_accept') }).Count -ge 1 -and
            @($actionKinds | Where-Object { $_ -in @('network_send', 'network_receive', 'network_disconnect') }).Count -ge 1
        }
).Count
$traceName = "AgentReins-Gate0-$($collector.Id)"
$result = [pscustomobject]@{
    collector_exit_code = $collector.ExitCode
    stop_reason = $summary.stop_reason
    health_snapshot_count = $health.Count
    healthy_snapshot_count = @($health | Where-Object { $_.coverage_status -eq "healthy" }).Count
    active_snapshot_count = @($health | Where-Object { $_.active_workbuddy_root_instances -ge 1 }).Count
    resource_snapshot_count = @($health | Where-Object { $null -ne $_.agent_resource_sample }).Count
    resource_active_snapshot_count = @($health | Where-Object {
        $_.agent_resource_sample.process_count -ge 1 -and
        $_.agent_resource_sample.working_set_bytes -gt 0 -and
        $_.agent_resource_sample.private_memory_bytes -gt 0
    }).Count
    permission_process_sample_count = $permissionProcesses.Count
    permission_processes_missing_integrity = $permissionProcessesMissingIntegrity.Count
    permission_privilege_sample_count = $permissionPrivileges.Count
    distinct_root_instances = $rootInstances.Count
    root_start_count = $rootStarts.Count
    root_stop_count = $rootStops.Count
    aggregated_read_count = $aggregatedReads.Count
    full_segment_count = $fullFiles.Count
    filtered_segment_count = $filteredFiles.Count
    manifest_count = $manifestFiles.Count
    manifest_complete = $finalManifest.complete
    manifest_segment_count = @($finalManifest.segments).Count
    manifest_files_present = $manifestFilesPresent
    summary_segments_published = $summary.segments_published
    configured_roll_size_bytes = $RollSizeBytes
    configured_roll_interval_seconds = $RollIntervalSeconds
    events_lost = $summary.events_lost
    parse_failures = $summary.parse_failures
    write_failures = $summary.write_failures
    output_batches_dropped = $summary.output_batches_dropped
    identity_collision_count = $summary.identity_collision_count
    network_event_count = $networkEvents.Count
    network_events_missing_operation_id = $networkEventsMissingOperationId.Count
    network_lifecycle_operation_count = $networkLifecycleOperationCount
    network_native_lifecycle_status = if ($networkEventsMissingOperationId.Count -eq 0 -and $networkLifecycleOperationCount -ge 1) { "passed" } else { "not_passed" }
    remaining_trace_sessions = @(& logman.exe query -ets | Select-String -SimpleMatch $traceName).Count
    stderr_bytes = (Get-Item -LiteralPath $paths.stderr).Length
}
$result | ConvertTo-Json -Compress

if ($result.stop_reason -ne "stop_signal" -or $result.health_snapshot_count -lt 2 -or $result.healthy_snapshot_count -ne $result.health_snapshot_count) {
    throw "常驻生命周期或健康快照不符合预期 result=$($result | ConvertTo-Json -Compress)"
}
if ($result.active_snapshot_count -lt 1 -or $result.distinct_root_instances -ne 2 -or $result.root_start_count -ne 2 -or $result.root_stop_count -ne 2) {
    throw "WorkBuddy 动态实例观测不符合预期 result=$($result | ConvertTo-Json -Compress)"
}
if ($result.resource_snapshot_count -ne $result.health_snapshot_count -or $result.resource_active_snapshot_count -lt 1) {
    throw "目标 Agent 资源采样不符合预期 result=$($result | ConvertTo-Json -Compress)"
}
if ($result.permission_process_sample_count -lt 1 -or $result.permission_processes_missing_integrity -ne 0 -or $result.permission_privilege_sample_count -lt 1) {
    throw "目标 Agent Permission 采样不符合预期 result=$($result | ConvertTo-Json -Compress)"
}
if ($result.aggregated_read_count -lt 2) {
    throw "健康快照过早刷新了 60 秒摘要窗口 result=$($result | ConvertTo-Json -Compress)"
}
if ($result.full_segment_count -lt 2 -or $result.full_segment_count -ne $result.filtered_segment_count -or $result.manifest_count -lt 2 -or -not $result.manifest_complete -or $result.manifest_segment_count -ne $result.full_segment_count -or $result.manifest_files_present -ne $result.manifest_segment_count -or $result.summary_segments_published -ne $result.manifest_segment_count) {
    throw "滚动证据或 manifest 不符合预期 result=$($result | ConvertTo-Json -Compress)"
}
if ($result.events_lost -ne 0 -or $result.parse_failures -ne 0 -or $result.write_failures -ne 0 -or $result.output_batches_dropped -ne 0 -or $result.identity_collision_count -ne 0) {
    throw "常驻 Observer 覆盖状态异常 result=$($result | ConvertTo-Json -Compress)"
}
if ($result.network_event_count -lt 1) {
    throw "未采集到 WorkBuddy 夹具网络事件 result=$($result | ConvertTo-Json -Compress)"
}
if ($result.network_events_missing_operation_id -eq 0 -and $result.network_lifecycle_operation_count -lt 1) {
    throw "TCP 连接生命周期未复用 operation_id result=$($result | ConvertTo-Json -Compress)"
}
if ($result.remaining_trace_sessions -ne 0 -or $result.stderr_bytes -ne 0) {
    throw "常驻 Observer 存在残留会话或错误输出 result=$($result | ConvertTo-Json -Compress)"
}
