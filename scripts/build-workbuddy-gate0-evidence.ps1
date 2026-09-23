[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$SourceRoot,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$ProjectRoot,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$SessionMappingPath,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$OsFullInputPath,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$OsFilteredInputPath,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$CollectorSummaryPath,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$WorkBuddyHome,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$LiveProjectRoot,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$CredentialFile,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$CredentialLabel,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$CredentialKind,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$EvidenceRoot,

    [Parameter(Mandatory = $true)]
    [ValidateRange(1, 60000)]
    [int]$CorrelationWindowMs
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Invoke-CapturedProcess {
    param(
        [Parameter(Mandatory = $true)]
        [string]$FilePath,
        [Parameter(Mandatory = $true)]
        [string]$Arguments,
        [Parameter(Mandatory = $true)]
        [string]$Label
    )

    $startInfo = [Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $FilePath
    $startInfo.Arguments = $Arguments
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    $process = [Diagnostics.Process]::Start($startInfo)
    $process.WaitForExit()
    $stdout = $process.StandardOutput.ReadToEnd()
    $stderr = $process.StandardError.ReadToEnd()
    if ($process.ExitCode -ne 0) {
        throw "$Label 失败 exit_code=$($process.ExitCode) stdout=$stdout stderr=$stderr"
    }
    [pscustomobject]@{
        stdout = $stdout
        stderr = $stderr
    }
}

function Test-PlaintextOccurrence {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path,
        [Parameter(Mandatory = $true)]
        [string]$Plaintext
    )

    $count = 0
    foreach ($line in [IO.File]::ReadLines($Path, [Text.UTF8Encoding]::new($false))) {
        $offset = 0
        while ($offset -le $line.Length - $Plaintext.Length) {
            $position = $line.IndexOf($Plaintext, $offset, [StringComparison]::Ordinal)
            if ($position -lt 0) {
                break
            }
            $count += 1
            $offset = $position + $Plaintext.Length
        }
    }
    $count
}

function Get-OsCorrelationTime {
    param(
        [Parameter(Mandatory = $true)]
        [object]$Event
    )

    $eventTime = [long]$Event.event_timestamp_unix_ms
    $observedTime = [long]$Event.observed_at_unix_ms
    if ([Math]::Abs($eventTime - $observedTime) -gt 60000) {
        return $observedTime
    }
    $eventTime
}

function Get-ActivityWindows {
    param(
        [Parameter(Mandatory = $true)]
        [string]$CorrelationPath,
        [Parameter(Mandatory = $true)]
        [int]$PaddingMilliseconds
    )

    $boundaries = @{}
    foreach ($line in [IO.File]::ReadLines($CorrelationPath, [Text.UTF8Encoding]::new($false))) {
        if ([string]::IsNullOrWhiteSpace($line)) {
            continue
        }
        $event = $line | ConvertFrom-Json
        $activityId = [string]$event.user_activity_id
        if ([string]::IsNullOrWhiteSpace($activityId) -or $activityId -like 'workbuddy:unattributed:*') {
            continue
        }
        $timestamp = [long]$event.event_timestamp_unix_ms
        if (-not $boundaries.ContainsKey($activityId)) {
            $boundaries[$activityId] = [pscustomobject]@{
                activity_id = $activityId
                start_unix_ms = $timestamp
                end_unix_ms = $timestamp
            }
            continue
        }
        if ($timestamp -lt $boundaries[$activityId].start_unix_ms) {
            $boundaries[$activityId].start_unix_ms = $timestamp
        }
        if ($timestamp -gt $boundaries[$activityId].end_unix_ms) {
            $boundaries[$activityId].end_unix_ms = $timestamp
        }
    }
    if ($boundaries.Count -eq 0) {
        throw "关联证据中没有可裁剪的用户行为时间窗 path=$CorrelationPath"
    }
    @(
        $boundaries.Values |
            ForEach-Object {
                [pscustomobject]@{
                    activity_id = $_.activity_id
                    start_unix_ms = [long]$_.start_unix_ms - $PaddingMilliseconds
                    end_unix_ms = [long]$_.end_unix_ms + $PaddingMilliseconds
                }
            } |
            Sort-Object start_unix_ms, activity_id
    )
}

function Export-OsTaskWindows {
    param(
        [Parameter(Mandatory = $true)]
        [string]$InputPath,
        [Parameter(Mandatory = $true)]
        [string]$OutputPath,
        [Parameter(Mandatory = $true)]
        [object[]]$Windows
    )

    $encoding = [Text.UTF8Encoding]::new($false)
    $writer = [IO.StreamWriter]::new($OutputPath, $false, $encoding)
    $sourceRows = 0
    $retainedRows = 0
    try {
        foreach ($line in [IO.File]::ReadLines($InputPath, $encoding)) {
            if ([string]::IsNullOrWhiteSpace($line)) {
                continue
            }
            $sourceRows += 1
            $event = $line | ConvertFrom-Json
            $timestamp = Get-OsCorrelationTime -Event $event
            $isRelevant = $false
            foreach ($window in $Windows) {
                if ($timestamp -ge $window.start_unix_ms -and $timestamp -le $window.end_unix_ms) {
                    $isRelevant = $true
                    break
                }
            }
            if ($isRelevant) {
                $writer.WriteLine($line)
                $retainedRows += 1
            }
        }
    }
    finally {
        $writer.Dispose()
    }
    [pscustomobject]@{
        source_rows = $sourceRows
        retained_rows = $retainedRows
        output_path = [IO.Path]::GetFullPath($OutputPath)
    }
}

function Export-OsAnalysisView {
    param(
        [Parameter(Mandatory = $true)]
        [string]$InputPath,
        [Parameter(Mandatory = $true)]
        [string]$OutputPath,
        [Parameter(Mandatory = $true)]
        [object[]]$Windows
    )

    $encoding = [Text.UTF8Encoding]::new($false)
    $groups = @{}
    $sourceRows = 0
    $matchedSourceRows = 0
    foreach ($line in [IO.File]::ReadLines($InputPath, $encoding)) {
        if ([string]::IsNullOrWhiteSpace($line)) {
            continue
        }
        $sourceRows += 1
        $event = $line | ConvertFrom-Json
        $timestamp = Get-OsCorrelationTime -Event $event
        $activityId = $null
        foreach ($window in $Windows) {
            if ($timestamp -ge $window.start_unix_ms -and $timestamp -le $window.end_unix_ms) {
                $activityId = [string]$window.activity_id
                break
            }
        }
        if ($null -eq $activityId) {
            continue
        }
        $matchedSourceRows += 1
        $destination = if ($null -eq $event.destination) { '' } else { [string]$event.destination.identifier }
        $statusCode = if ($null -eq $event.result.status_code) { '' } else { [string]$event.result.status_code }
        $success = if ($null -eq $event.result.success) { '' } else { [string]$event.result.success }
        $key = @(
            $activityId,
            [string]$event.action.kind,
            [string]$event.process.image_name,
            [string]$event.resource.kind,
            [string]$event.resource.identifier,
            $destination,
            $statusCode,
            $success,
            [string]$event.retention.class,
            [string]$event.retention.reason
        ) -join ([char]0x1f)
        $occurrences = if ($null -eq $event.aggregation) { 1 } else { [long]$event.aggregation.occurrence_count }
        $bytes = if ($null -ne $event.aggregation -and $null -ne $event.aggregation.total_bytes_transferred) {
            [long]$event.aggregation.total_bytes_transferred
        }
        elseif ($null -ne $event.result.bytes_transferred) {
            [long]$event.result.bytes_transferred
        }
        else {
            $null
        }
        if (-not $groups.ContainsKey($key)) {
            $groups[$key] = [pscustomobject]@{
                event = $event
                activity_id = $activityId
                source_record_count = 1L
                occurrence_count = $occurrences
                first_timestamp_unix_ms = $timestamp
                last_timestamp_unix_ms = $timestamp
                total_bytes_transferred = $bytes
            }
            continue
        }
        $group = $groups[$key]
        $group.source_record_count += 1
        $group.occurrence_count += $occurrences
        if ($timestamp -lt $group.first_timestamp_unix_ms) {
            $group.first_timestamp_unix_ms = $timestamp
        }
        if ($timestamp -gt $group.last_timestamp_unix_ms) {
            $group.last_timestamp_unix_ms = $timestamp
        }
        if ($null -ne $bytes) {
            if ($null -eq $group.total_bytes_transferred) {
                $group.total_bytes_transferred = 0L
            }
            $group.total_bytes_transferred += $bytes
        }
    }

    $writer = [IO.StreamWriter]::new($OutputPath, $false, $encoding)
    try {
        foreach ($group in $groups.Values | Sort-Object activity_id, first_timestamp_unix_ms) {
            $event = $group.event
            $event.aggregation = [pscustomobject]@{
                occurrence_count = $group.occurrence_count
                first_event_timestamp_unix_ms = $group.first_timestamp_unix_ms
                last_event_timestamp_unix_ms = $group.last_timestamp_unix_ms
                total_bytes_transferred = $group.total_bytes_transferred
            }
            $event | Add-Member -NotePropertyName analysis -NotePropertyValue ([pscustomobject]@{
                activity_id = $group.activity_id
                grouping = 'process_image_name'
                source_record_count = $group.source_record_count
                representative_event_id = [string]$event.event_id
            }) -Force
            $writer.WriteLine(($event | ConvertTo-Json -Depth 12 -Compress))
        }
    }
    finally {
        $writer.Dispose()
    }
    [pscustomobject]@{
        source_rows = $sourceRows
        matched_source_rows = $matchedSourceRows
        retained_rows = $groups.Count
        output_path = [IO.Path]::GetFullPath($OutputPath)
    }
}

$source = [IO.Path]::GetFullPath($SourceRoot)
$projects = [IO.Path]::GetFullPath($ProjectRoot)
$mappingFile = [IO.Path]::GetFullPath($SessionMappingPath)
$osFull = [IO.Path]::GetFullPath($OsFullInputPath)
$osFiltered = [IO.Path]::GetFullPath($OsFilteredInputPath)
$collectorSummary = [IO.Path]::GetFullPath($CollectorSummaryPath)
$workBuddyDirectory = [IO.Path]::GetFullPath($WorkBuddyHome)
$liveProjects = [IO.Path]::GetFullPath($LiveProjectRoot)
$credentialPath = [IO.Path]::GetFullPath($CredentialFile)
$evidence = [IO.Path]::GetFullPath($EvidenceRoot)
$semanticCollectorPath = Join-Path $source 'target\debug\workbuddy-semantic-collector.exe'
$correlatorPath = Join-Path $source 'target\debug\evidence-correlator.exe'
$correlationExporterPath = Join-Path $source 'scripts\export-correlated-evidence-csv.ps1'
$contextExporterPath = Join-Path $source 'scripts\export-workbuddy-context-snapshot.ps1'
$reportGeneratorPath = Join-Path $source 'scripts\new-workbuddy-observation-report.ps1'
foreach ($requiredPath in @($projects, $mappingFile, $osFull, $osFiltered, $collectorSummary, $workBuddyDirectory, $liveProjects, $credentialPath, $semanticCollectorPath, $correlatorPath, $correlationExporterPath, $contextExporterPath, $reportGeneratorPath)) {
    if (-not (Test-Path -LiteralPath $requiredPath)) {
        throw "找不到 Gate 0 证据构建依赖 path=$requiredPath"
    }
}
if (Test-Path -LiteralPath $evidence) {
    throw "Gate 0 证据目录已存在，拒绝覆盖 path=$evidence"
}

$mapping = Get-Content -LiteralPath $mappingFile -Raw -Encoding UTF8 | ConvertFrom-Json
if ($mapping.schema_version -ne '0.1.0' -or @($mapping.sessions).Count -eq 0) {
    throw "会话映射格式无效 path=$mappingFile schema_version=$($mapping.schema_version)"
}
$token = (Get-Content -LiteralPath $credentialPath -Raw -Encoding UTF8).TrimEnd()
if ($token.Length -eq 0) {
    throw "测试凭据为空 path=$credentialPath"
}

New-Item -ItemType Directory -Path $evidence | Out-Null
$temporaryRoot = Join-Path $evidence 'semantic-parts'
New-Item -ItemType Directory -Path $temporaryRoot | Out-Null
$paths = [ordered]@{
    semantic_full = Join-Path $evidence 'workbuddy-semantic.full.ndjson'
    os_full = Join-Path $evidence 'workbuddy-os.full.ndjson'
    os_filtered = Join-Path $evidence 'workbuddy-os.filtered.ndjson'
    correlation_full = Join-Path $evidence 'workbuddy-correlation.full.ndjson'
    correlation_filtered = Join-Path $evidence 'workbuddy-correlation.filtered.ndjson'
    correlation_full_csv = Join-Path $evidence 'workbuddy-correlation.full.csv'
    correlation_filtered_csv = Join-Path $evidence 'workbuddy-correlation.filtered.csv'
    credential_catalog = Join-Path $evidence 'credential-catalog.full.json'
    context_full = Join-Path $evidence 'workbuddy-context.full.json'
    context_filtered = Join-Path $evidence 'workbuddy-context.filtered.json'
    context_full_csv = Join-Path $evidence 'workbuddy-context.full.csv'
    context_filtered_csv = Join-Path $evidence 'workbuddy-context.filtered.csv'
    report_full = Join-Path $evidence 'workbuddy-gate0-report.full.html'
    report_filtered = Join-Path $evidence 'workbuddy-gate0-report.filtered.html'
    session_manifest = Join-Path $evidence 'session-manifest.csv'
    source_manifest = Join-Path $evidence 'source-manifest.csv'
    summary = Join-Path $evidence 'gate0-summary.json'
    manifest = Join-Path $evidence 'manifest.csv'
    correlator_stdout = Join-Path $evidence 'correlator-stdout.json'
    correlator_stderr = Join-Path $evidence 'correlator-stderr.ndjson'
}
$utf8 = [Text.UTF8Encoding]::new($false)
$semanticWriter = [IO.StreamWriter]::new($paths.semantic_full, $false, $utf8)
$sessionRows = [Collections.Generic.List[object]]::new()
try {
    $index = 0
    foreach ($session in $mapping.sessions) {
        $index += 1
        $projectFile = Join-Path $projects ([string]$session.relative_path)
        if (-not (Test-Path -LiteralPath $projectFile -PathType Leaf)) {
            throw "会话映射引用的项目文件不存在 scenario_id=$($session.scenario_id) path=$projectFile"
        }
        $partPath = Join-Path $temporaryRoot ("{0:D2}-{1}.ndjson" -f $index, $session.scenario_id)
        $arguments = '--input "{0}" --since-unix-ms {1} --until-unix-ms {2} --session-id "{3}" --process-id {4} --output "{5}"' -f $projectFile, $session.since_unix_ms, $session.until_unix_ms, $mapping.os_session_id, $session.process_id, $partPath
        $result = Invoke-CapturedProcess -FilePath $semanticCollectorPath -Arguments $arguments -Label "WorkBuddy 语义转换 scenario_id=$($session.scenario_id)"
        $eventCount = 0
        $activityIds = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
        foreach ($line in [IO.File]::ReadLines($partPath, $utf8)) {
            if ([string]::IsNullOrWhiteSpace($line)) {
                continue
            }
            $event = $line | ConvertFrom-Json
            $semanticWriter.WriteLine($line)
            $eventCount += 1
            $activityIds.Add([string]$event.user_activity_id) | Out-Null
        }
        $sessionRows.Add([pscustomobject]@{
            logical_round = $session.logical_round
            scenario_id = $session.scenario_id
            source_file = $session.relative_path
            process_id = $session.process_id
            since_unix_ms = $session.since_unix_ms
            until_unix_ms = $session.until_unix_ms
            semantic_events = $eventCount
            user_activity_ids = @($activityIds | Sort-Object) -join ';'
            collector_summary = $result.stdout.Trim()
        })
    }
}
finally {
    $semanticWriter.Dispose()
}
$sessionRows | Export-Csv -LiteralPath $paths.session_manifest -NoTypeInformation -Encoding UTF8

$sourceArtifacts = @(
    [pscustomobject]@{ role = 'session_mapping'; path = $mappingFile },
    [pscustomobject]@{ role = 'os_full'; path = $osFull },
    [pscustomobject]@{ role = 'os_filtered'; path = $osFiltered },
    [pscustomobject]@{ role = 'collector_summary'; path = $collectorSummary },
    [pscustomobject]@{ role = 'credential_source'; path = $credentialPath }
)
foreach ($session in $mapping.sessions) {
    $sourceArtifacts += [pscustomobject]@{
        role = "workbuddy_session:$($session.scenario_id)"
        path = Join-Path $projects ([string]$session.relative_path)
    }
}
$sourceManifestRows = foreach ($sourceArtifact in $sourceArtifacts) {
    [pscustomobject]@{
        role = $sourceArtifact.role
        path = $sourceArtifact.path
        bytes = (Get-Item -LiteralPath $sourceArtifact.path).Length
        sha256 = (Get-FileHash -LiteralPath $sourceArtifact.path -Algorithm SHA256).Hash.ToLowerInvariant()
    }
}
$sourceManifestRows | Export-Csv -LiteralPath $paths.source_manifest -NoTypeInformation -Encoding UTF8

$credentialCatalog = @(
    [pscustomobject]@{
        label = $CredentialLabel
        kind = $CredentialKind
        value = $token
        fingerprint = "sha256:$((Get-FileHash -LiteralPath $credentialPath -Algorithm SHA256).Hash.ToLowerInvariant())"
    }
)
[IO.File]::WriteAllText($paths.credential_catalog, (ConvertTo-Json -InputObject $credentialCatalog), $utf8)

$correlatorArguments = '--semantic-input "{0}" --os-input "{1}" --credential-catalog "{2}" --full-output "{3}" --filtered-output "{4}" --window-ms {5}' -f $paths.semantic_full, $osFiltered, $paths.credential_catalog, $paths.correlation_full, $paths.correlation_filtered, $CorrelationWindowMs
$correlatorResult = Invoke-CapturedProcess -FilePath $correlatorPath -Arguments $correlatorArguments -Label 'Gate 0 证据关联'
[IO.File]::WriteAllText($paths.correlator_stdout, $correlatorResult.stdout, $utf8)
[IO.File]::WriteAllText($paths.correlator_stderr, $correlatorResult.stderr, $utf8)

$correlationSummary = & $correlationExporterPath -FullInputPath $paths.correlation_full -FilteredInputPath $paths.correlation_filtered -FullCsvPath $paths.correlation_full_csv -FilteredCsvPath $paths.correlation_filtered_csv | ConvertFrom-Json
$activityWindows = Get-ActivityWindows -CorrelationPath $paths.correlation_full -PaddingMilliseconds $CorrelationWindowMs
$osFullSummary = Export-OsTaskWindows -InputPath $osFull -OutputPath $paths.os_full -Windows $activityWindows
$osFilteredSummary = Export-OsAnalysisView -InputPath $osFiltered -OutputPath $paths.os_filtered -Windows $activityWindows
$contextSummary = & $contextExporterPath -WorkBuddyHome $workBuddyDirectory -ProjectRoot $liveProjects -FullOutputPath $paths.context_full -FilteredOutputPath $paths.context_filtered -FullCsvPath $paths.context_full_csv -FilteredCsvPath $paths.context_filtered_csv | ConvertFrom-Json
$fullReport = & $reportGeneratorPath -OsInputPath $paths.os_filtered -CorrelationInputPath $paths.correlation_full -ContextSnapshotPath $paths.context_full -CollectorSummaryPath $collectorSummary -RunId 'workbuddy-8-round-gate0-full' -OutputPath $paths.report_full | ConvertFrom-Json
$filteredReport = & $reportGeneratorPath -OsInputPath $paths.os_filtered -CorrelationInputPath $paths.correlation_filtered -ContextSnapshotPath $paths.context_filtered -CollectorSummaryPath $collectorSummary -RunId 'workbuddy-8-round-gate0-filtered' -OutputPath $paths.report_filtered | ConvertFrom-Json

$filteredFiles = @($paths.correlation_filtered, $paths.correlation_filtered_csv, $paths.context_filtered, $paths.context_filtered_csv, $paths.report_filtered, $paths.os_filtered)
$filteredPlaintextOccurrences = 0
foreach ($filteredFile in $filteredFiles) {
    $filteredPlaintextOccurrences += Test-PlaintextOccurrence -Path $filteredFile -Plaintext $token
}
if ($filteredPlaintextOccurrences -ne 0) {
    throw "过滤证据仍包含测试凭据原文 occurrence_count=$filteredPlaintextOccurrences"
}
$fullPlaintextOccurrences = Test-PlaintextOccurrence -Path $paths.semantic_full -Plaintext $token

$gateSummary = [ordered]@{
    logical_rounds = @($mapping.sessions.logical_round | Sort-Object -Unique).Count
    analyzed_sessions = @($mapping.sessions).Count
    semantic_events = ($sessionRows | Measure-Object semantic_events -Sum).Sum
    os_full_source_rows = $osFullSummary.source_rows
    os_full_retained_rows = $osFullSummary.retained_rows
    os_filtered_source_rows = $osFilteredSummary.source_rows
    os_filtered_matched_source_rows = $osFilteredSummary.matched_source_rows
    os_filtered_retained_rows = $osFilteredSummary.retained_rows
    correlation_full_rows = $correlationSummary.full_rows
    correlation_filtered_rows = $correlationSummary.filtered_rows
    context_artifacts = @($contextSummary.artifacts).Count
    report_user_activities = $fullReport.user_activity_count
    filtered_plaintext_credential_occurrences = $filteredPlaintextOccurrences
    full_semantic_plaintext_credential_occurrences = $fullPlaintextOccurrences
    evidence_root = $evidence
}
[IO.File]::WriteAllText($paths.summary, ($gateSummary | ConvertTo-Json -Depth 5), $utf8)

$artifactPaths = @(
    $paths.semantic_full,
    $paths.os_full,
    $paths.os_filtered,
    $paths.correlation_full,
    $paths.correlation_filtered,
    $paths.correlation_full_csv,
    $paths.correlation_filtered_csv,
    $paths.context_full,
    $paths.context_filtered,
    $paths.context_full_csv,
    $paths.context_filtered_csv,
    $paths.credential_catalog,
    $paths.report_full,
    $paths.report_filtered,
    $paths.session_manifest,
    $paths.source_manifest,
    $paths.summary,
    $paths.correlator_stdout,
    $paths.correlator_stderr
)
$manifestArtifacts = @($artifactPaths | ForEach-Object {
    [pscustomobject]@{
        name = Split-Path -Leaf $_
        path = $_
    }
})
$manifestArtifacts += @(Get-ChildItem -LiteralPath $temporaryRoot -File -Filter '*.ndjson' | Sort-Object Name | ForEach-Object {
    [pscustomobject]@{
        name = "semantic-parts/$($_.Name)"
        path = $_.FullName
    }
})
$manifestRows = foreach ($artifact in $manifestArtifacts) {
    [pscustomobject]@{
        artifact = $artifact.name
        bytes = (Get-Item -LiteralPath $artifact.path).Length
        sha256 = (Get-FileHash -LiteralPath $artifact.path -Algorithm SHA256).Hash.ToLowerInvariant()
        evidence_scope = 'workbuddy_8_round_gate0'
    }
}
$manifestRows | Export-Csv -LiteralPath $paths.manifest -NoTypeInformation -Encoding UTF8

$gateSummary | ConvertTo-Json -Compress
