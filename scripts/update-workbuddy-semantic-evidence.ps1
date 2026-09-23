[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$SourceRoot,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$CollectorPath,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$ProjectRoot,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$StatePath,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$OutputPath,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$PublishedManifestPath,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$SessionId,

    [Parameter(Mandatory = $true)]
    [uint32]$WorkBuddyProcessId,

    [Parameter(Mandatory = $true)]
    [long]$SinceUnixMs,

    [Parameter(Mandatory = $true)]
    [long]$UntilUnixMs
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if ($WorkBuddyProcessId -eq 0) {
    throw 'WorkBuddyProcessId 不得为 0'
}
if ($SinceUnixMs -lt 0 -or $UntilUnixMs -lt 0 -or $SinceUnixMs -gt $UntilUnixMs) {
    throw "语义同步时间窗无效 since_unix_ms=$SinceUnixMs until_unix_ms=$UntilUnixMs"
}

$source = [IO.Path]::GetFullPath($SourceRoot)
$projects = [IO.Path]::GetFullPath($ProjectRoot)
$stateFile = [IO.Path]::GetFullPath($StatePath)
$outputFile = [IO.Path]::GetFullPath($OutputPath)
$publishedManifestFile = [IO.Path]::GetFullPath($PublishedManifestPath)
$collectorPath = [IO.Path]::GetFullPath($CollectorPath)
foreach ($requiredPath in @($projects, $collectorPath)) {
    if (-not (Test-Path -LiteralPath $requiredPath)) {
        throw "找不到增量语义同步依赖 path=$requiredPath"
    }
}

foreach ($parent in @((Split-Path -Parent $stateFile), (Split-Path -Parent $outputFile), (Split-Path -Parent $publishedManifestFile)) | Sort-Object -Unique) {
    if (-not (Test-Path -LiteralPath $parent)) {
        New-Item -ItemType Directory -Path $parent | Out-Null
    }
}

$utf8 = [Text.UTF8Encoding]::new($false)
$seenEventIds = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
$unattributedEventCount = 0L
if (Test-Path -LiteralPath $outputFile) {
    foreach ($line in [IO.File]::ReadLines($outputFile, $utf8)) {
        if ([string]::IsNullOrWhiteSpace($line)) {
            continue
        }
        $existingEvent = $line | ConvertFrom-Json
        if ([string]::IsNullOrWhiteSpace([string]$existingEvent.event_id)) {
            throw "已有语义输出缺少 event_id path=$outputFile"
        }
        if ($seenEventIds.Add([string]$existingEvent.event_id)) {
            $activityId = [string]$existingEvent.user_activity_id
            if ([string]::IsNullOrWhiteSpace($activityId) -or $activityId -like 'workbuddy:unattributed:*') {
                $unattributedEventCount += 1
            }
        }
    }
}

$existingState = $null
$existingSourceRecordIds = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
$existingFileStates = @{}
if (Test-Path -LiteralPath $stateFile) {
    $existingState = Get-Content -LiteralPath $stateFile -Raw -Encoding UTF8 | ConvertFrom-Json
    if ($existingState.schema_version -ne '0.1.0') {
        throw "不支持的语义同步状态版本 path=$stateFile schema_version=$($existingState.schema_version)"
    }
    foreach ($sourceRecordId in @($existingState.processed_source_record_ids)) {
        $existingSourceRecordIds.Add([string]$sourceRecordId) | Out-Null
    }
    foreach ($fileState in @($existingState.source_files)) {
        $existingFileStates[[string]$fileState.path] = $fileState
    }
}

$projectFiles = @(Get-ChildItem -LiteralPath $projects -Recurse -File -Filter '*.jsonl' | Sort-Object FullName)
if ($projectFiles.Count -eq 0) {
    throw "WorkBuddy 项目目录中没有 JSONL 文件 project_root=$projects"
}

$temporaryRoot = Join-Path ([IO.Path]::GetTempPath()) "agentreins-semantic-$([Guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory -Path $temporaryRoot | Out-Null
$newLines = [Collections.Generic.List[string]]::new()
$newSourceRecordIds = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
$fileStates = [Collections.Generic.List[object]]::new()
$filesProcessed = 0
$filesUnchanged = 0
try {
    foreach ($projectFile in $projectFiles) {
        $currentFileState = [pscustomobject]@{
            path = $projectFile.FullName
            length = $projectFile.Length
            last_write_time_utc = $projectFile.LastWriteTimeUtc.ToString('o')
        }
        $fileStates.Add($currentFileState)
        if ($projectFile.Length -eq 0) {
            continue
        }
        $previousFileState = $existingFileStates[$projectFile.FullName]
        if ($null -ne $previousFileState) {
            if ([long]$projectFile.Length -lt [long]$previousFileState.length) {
                throw "WorkBuddy 会话文件长度减少，拒绝静默接受截断 path=$($projectFile.FullName) previous_length=$($previousFileState.length) current_length=$($projectFile.Length)"
            }
            if ([long]$projectFile.Length -eq [long]$previousFileState.length -and $currentFileState.last_write_time_utc -eq [string]$previousFileState.last_write_time_utc) {
                $filesUnchanged += 1
                continue
            }
        }
        $filesProcessed += 1
        $temporaryOutput = Join-Path $temporaryRoot "$filesProcessed.ndjson"
        $arguments = '--input "{0}" --since-unix-ms {1} --until-unix-ms {2} --session-id "{3}" --process-id {4} --output "{5}"' -f $projectFile.FullName, $SinceUnixMs, $UntilUnixMs, $SessionId, $WorkBuddyProcessId, $temporaryOutput
        $startInfo = [Diagnostics.ProcessStartInfo]::new()
        $startInfo.FileName = $collectorPath
        $startInfo.Arguments = $arguments
        $startInfo.UseShellExecute = $false
        $startInfo.CreateNoWindow = $true
        $startInfo.RedirectStandardOutput = $true
        $startInfo.RedirectStandardError = $true
        $collector = [Diagnostics.Process]::Start($startInfo)
        $collector.WaitForExit()
        $stdout = $collector.StandardOutput.ReadToEnd()
        $stderr = $collector.StandardError.ReadToEnd()
        if ($collector.ExitCode -ne 0) {
            throw "WorkBuddy 语义文件解析失败 path=$($projectFile.FullName) exit_code=$($collector.ExitCode) stdout=$stdout stderr=$stderr"
        }
        foreach ($line in [IO.File]::ReadLines($temporaryOutput, $utf8)) {
            if ([string]::IsNullOrWhiteSpace($line)) {
                continue
            }
            $event = $line | ConvertFrom-Json
            $sourceRecordId = [string]$event.source_record_id
            $newSourceRecordIds.Add($sourceRecordId) | Out-Null
            if ($seenEventIds.Add([string]$event.event_id)) {
                $newLines.Add($line)
                $activityId = [string]$event.user_activity_id
                if ([string]::IsNullOrWhiteSpace($activityId) -or $activityId -like 'workbuddy:unattributed:*') {
                    $unattributedEventCount += 1
                }
            }
        }
    }

    if ($newLines.Count -gt 0) {
        [IO.File]::AppendAllLines($outputFile, $newLines, $utf8)
    }
    elseif (-not (Test-Path -LiteralPath $outputFile)) {
        [IO.File]::WriteAllText($outputFile, '', $utf8)
    }

    foreach ($sourceRecordId in $newSourceRecordIds) {
        $existingSourceRecordIds.Add($sourceRecordId) | Out-Null
    }
    $state = [ordered]@{
        schema_version = '0.1.0'
        updated_at = [DateTimeOffset]::UtcNow.ToString('o')
        session_id = $SessionId
        workbuddy_process_id = $WorkBuddyProcessId
        since_unix_ms = $SinceUnixMs
        until_unix_ms = $UntilUnixMs
        source_files = $fileStates
        processed_source_record_ids = @($existingSourceRecordIds | Sort-Object)
        emitted_event_ids = @($seenEventIds | Sort-Object)
    }
    $temporaryState = "$stateFile.$([Guid]::NewGuid().ToString('N')).tmp"
    [IO.File]::WriteAllText($temporaryState, ($state | ConvertTo-Json -Depth 6), $utf8)
    Move-Item -LiteralPath $temporaryState -Destination $stateFile -Force

    $publishedOutput = Get-Item -LiteralPath $outputFile
    $publishedManifest = [ordered]@{
        schema_version = '0.1.0'
        published_at_unix_ms = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
        session_id = $SessionId
        semantic_file = $outputFile
        published_bytes = [long]$publishedOutput.Length
        semantic_events = [long]$seenEventIds.Count
    }
    $temporaryManifest = "$publishedManifestFile.$([Guid]::NewGuid().ToString('N')).tmp"
    [IO.File]::WriteAllText($temporaryManifest, ($publishedManifest | ConvertTo-Json -Depth 4), $utf8)
    Move-Item -LiteralPath $temporaryManifest -Destination $publishedManifestFile -Force
}
finally {
    if (Test-Path -LiteralPath $temporaryRoot) {
        Remove-Item -LiteralPath $temporaryRoot -Recurse -Force
    }
}

[pscustomobject]@{
    project_files_found = $projectFiles.Count
    project_files_processed = $filesProcessed
    project_files_unchanged = $filesUnchanged
    unique_source_records = $existingSourceRecordIds.Count
    total_event_ids = $seenEventIds.Count
    unattributed_event_count = $unattributedEventCount
    new_events_written = $newLines.Count
    output_path = $outputFile
    state_path = $stateFile
    published_manifest_path = $publishedManifestFile
    published_bytes = [long](Get-Item -LiteralPath $outputFile).Length
} | ConvertTo-Json -Compress
