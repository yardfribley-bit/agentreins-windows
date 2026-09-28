[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$EvidenceRoot,
    [Parameter(Mandatory = $true)][string]$ConfigurationPath,
    [Parameter(Mandatory = $true)][string]$StateRoot,
    [string]$OsEvidenceRoot = $EvidenceRoot,
    [switch]$FunctionsOnly
)

# The observer owns collection. This process only reads published evidence and
# sends task documents accepted by the same audit server as the macOS client.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Read-Json([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { throw "Missing file: $Path" }
    return Get-Content -LiteralPath $Path -Raw -Encoding UTF8 | ConvertFrom-Json
}

function Save-Json([string]$Path, $Value) {
    $temporary = "$Path.$([Guid]::NewGuid().ToString('N')).tmp"
    $json = ConvertTo-Json -InputObject $Value -Depth 100 -Compress
    [IO.File]::WriteAllText($temporary, $json, [Text.UTF8Encoding]::new($false))
    Move-Item -LiteralPath $temporary -Destination $Path -Force
}

function Digest([string]$Value) {
    $sha = [Security.Cryptography.SHA256]::Create()
    try {
        return ([BitConverter]::ToString($sha.ComputeHash([Text.Encoding]::UTF8.GetBytes($Value)))).Replace('-', '').ToLowerInvariant()
    } finally { $sha.Dispose() }
}

function Stable-UUID([string]$Value) {
    $hex = (Digest $Value).Substring(0, 32)
    return "$($hex.Substring(0,8))-$($hex.Substring(8,4))-$($hex.Substring(12,4))-$($hex.Substring(16,4))-$($hex.Substring(20,12))"
}

function Iso-Time([long]$Milliseconds) {
    return [DateTimeOffset]::FromUnixTimeMilliseconds($Milliseconds).UtcDateTime.ToString('o')
}

function Local-Private-IPv4 {
    $ips = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    foreach ($adapter in [Net.NetworkInformation.NetworkInterface]::GetAllNetworkInterfaces()) {
        if ($adapter.OperationalStatus -ne [Net.NetworkInformation.OperationalStatus]::Up) { continue }
        foreach ($address in $adapter.GetIPProperties().UnicastAddresses) {
            if ($address.Address.AddressFamily -ne [Net.Sockets.AddressFamily]::InterNetwork) { continue }
            $bytes = $address.Address.GetAddressBytes()
            if ($bytes[0] -eq 10 -or
                ($bytes[0] -eq 172 -and $bytes[1] -ge 16 -and $bytes[1] -le 31) -or
                ($bytes[0] -eq 192 -and $bytes[1] -eq 168)) {
                [void]$ips.Add($address.Address.ToString())
            }
        }
    }
    return @($ips | Sort-Object)
}

function Published-Lines([string]$Path, [long]$Bytes) {
    if ($Bytes -lt 0 -or $Bytes -gt 67108864) { throw "Published semantic evidence exceeds 64 MiB; no partial upload" }
    $file = [IO.File]::Open($Path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::ReadWrite)
    try {
        if ($file.Length -lt $Bytes) { throw "Semantic manifest exceeds file length" }
        $buffer = [Array]::CreateInstance([byte], [int]$Bytes)
        $offset = 0
        while ($offset -lt $Bytes) {
            $count = $file.Read($buffer, $offset, [Math]::Min(1048576, $Bytes - $offset))
            if ($count -eq 0) { throw "Unexpected end of semantic evidence" }
            $offset += $count
        }
        $body = [Text.UTF8Encoding]::new($false, $true).GetString($buffer)
        if ($body.Length -gt 0 -and -not $body.EndsWith("`n")) { throw "Semantic publication ends mid-record" }
        return @($body.Split("`n") | Where-Object { $_.Trim().Length -gt 0 })
    } finally { $file.Dispose() }
}

function Read-Matched-OsEvents([string]$RunRoot, [string[]]$Operations) {
    if ($Operations.Count -eq 0) { return @() }
    $matches = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    foreach ($operation in $Operations) { [void]$matches.Add($operation) }
    $segments = Join-Path $RunRoot 'segments'
    if (-not (Test-Path -LiteralPath $segments -PathType Container)) { return @() }
    $manifest = Get-ChildItem -LiteralPath $segments -Filter '*manifest*.json' -File |
        Sort-Object LastWriteTimeUtc | Select-Object -Last 1
    if ($null -eq $manifest) { return @() }
    $published = Read-Json $manifest.FullName
    $result = [Collections.Generic.List[object]]::new()
    foreach ($segment in @($published.segments)) {
        $path = [string]$segment.filtered_file
        if (-not [IO.Path]::IsPathRooted($path)) { $path = Join-Path $segments $path }
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "Published OS segment missing: $path" }
        $remaining = [long]$segment.filtered_events
        foreach ($line in [IO.File]::ReadLines($path)) {
            if ($remaining -le 0) { break }
            $remaining--
            if ([string]::IsNullOrWhiteSpace($line)) { continue }
            $event = $line | ConvertFrom-Json
            if ($null -ne $event.operation_id -and $matches.Contains([string]$event.operation_id)) {
                $result.Add($event)
            }
        }
        if ($remaining -gt 0) { throw "OS segment publication count is not closed: $path" }
    }
    return @($result.ToArray())
}

function New-Snapshot($Group, [object[]]$OsEvents) {
    $semantic = @($Group.Group | Sort-Object event_timestamp_unix_ms, event_id)
    $users = @($semantic | Where-Object action_kind -eq 'user_input')
    $requests = @($semantic | Where-Object action_kind -eq 'llm_request')
    if ($users.Count -eq 0 -or $requests.Count -eq 0) { return $null }
    $first = $users | Select-Object -First 1
    $started = [long]$first.event_timestamp_unix_ms
    $session = [string]$first.session_id
    $activity = [string]$first.user_activity_id
    if ([string]::IsNullOrWhiteSpace($activity)) { return $null }
    $taskID = "windows:" + (Digest "$session`:$activity")
    $sourceEvents = @($semantic | ForEach-Object {
        $event = $_
        $isRequest = $event.action_kind -eq 'llm_request'
        $isUser = $event.action_kind -eq 'user_input'
        [ordered]@{
            id = Stable-UUID ([string]$event.event_id)
            kind = if ($isRequest) { 'model' } elseif ($isUser) { 'user' } else { 'agent' }
            ruleId = 'windows-observation'
            path = '-'
            command = $null
            agent = 'workbuddy'
            op = if ($isRequest) { 'prompt' } else { [string]$event.action_kind }
            severity = 'info'
            ts = Iso-Time ([long]$event.event_timestamp_unix_ms)
            startedAt = Iso-Time ([long]$event.event_timestamp_unix_ms)
            action = 'seen'
            sessionId = [string]$event.session_id
            traceId = $event.trace_id
            turnId = $activity
            toolCallId = $event.tool_call_id
            userIntent = if ($isUser) { [string]$event.content } else { $null }
            modelPrompt = if ($isRequest) { [string]$event.content } else { $null }
            modelResponse = if ($event.action_kind -eq 'llm_result') { [string]$event.content } else { $null }
            model = $event.model_id
            processId = $event.process_id
            attributionConfidence = 'unknown'
            source = 'windows_workbuddy_semantic'
            windowsNativeEvent = $event
        }
    })
    $outbound = @($requests | ForEach-Object {
        $event = $_
        [ordered]@{
            id = "outbound:$($event.event_id)"
            eventID = [string]$event.event_id
            capturedAt = Iso-Time ([long]$event.event_timestamp_unix_ms)
            model = $event.model_id
            destination = $null
            traceID = $event.trace_id
            requestID = $event.conversation_request_id
            userIntent = [string]$first.content
            body = [string]$event.content
            bodyBytes = [Text.Encoding]::UTF8.GetByteCount([string]$event.content)
            bodySHA256 = Digest ([string]$event.content)
            evidenceSource = 'windows_workbuddy_semantic'
            sourceLocator = $event.source_record_id
            captureMode = 'agentRecordedContext'
            completeness = 'unknown'
            confidence = 'unknown'
        }
    })
    $files = @($OsEvents | Where-Object { [string]$_.resource.kind -eq 'file' })
    $network = @($OsEvents | Where-Object { [string]$_.resource.kind -eq 'network' })
    $fileActivities = @($files | ForEach-Object {
        $operation = switch ([string]$_.action.kind) {
            'file_read' { 'read' }
            'file_delete' { 'delete' }
            'file_rename' { 'rename' }
            'file_write' { 'modify' }
            default { 'read' }
        }
        [ordered]@{
            operation = $operation; path = [string]$_.resource.identifier
            beforeDigest = $null; afterDigest = $null; patch = $null
            toolCallID = $null; attribution = 'unknown'
            windowsNativeEvent = $_
        }
    })
    $networkFlows = @($network | ForEach-Object {
        $destination = if ($null -ne $_.destination) { [string]$_.destination.identifier } else { [string]$_.resource.identifier }
        [ordered]@{
            id = [string]$_.event_id
            category = 'UNKNOWN / NEEDS ATTRIBUTION'
            domain = $null; ip = $destination; port = $null
            agent = 'WorkBuddy'; purpose = 'OS network event; payload not established'
            grade = 'Unknown'; reason = 'ETW event has no independently verified task-to-payload link'
            firstObservedAt = Iso-Time ([long]$_.event_timestamp_unix_ms)
            lastObservedAt = Iso-Time ([long]$_.event_timestamp_unix_ms)
            connectionCount = 1
            sourceEvent = [ordered]@{
                id = Stable-UUID ([string]$_.event_id)
                kind = 'network'; ruleId = 'windows-etw'; path = '-'; command = $null
                agent = 'workbuddy'; op = [string]$_.action.kind; severity = 'info'
                ts = Iso-Time ([long]$_.event_timestamp_unix_ms); action = 'seen'
                source = 'windows_etw'; remoteHost = $destination
                attributionConfidence = 'unknown'
            }
            windowsNativeEvent = $_
        }
    })
    $responses = @($semantic | Where-Object action_kind -eq 'llm_result' | ForEach-Object {
        [ordered]@{
            eventID = Stable-UUID ([string]$_.event_id)
            receivedAt = Iso-Time ([long]$_.event_timestamp_unix_ms)
            traceID = $_.trace_id; model = $_.model_id
            content = [string]$_.content; recordedReasoning = $null
            evidenceSource = 'windows_workbuddy_semantic'
        }
    })
    $models = @($semantic | ForEach-Object { $_.model_id } | Where-Object { $_ } | Sort-Object -Unique)
    $traces = @($semantic | ForEach-Object { $_.trace_id } | Where-Object { $_ } | Sort-Object -Unique)
    $sourceIds = @($semantic | ForEach-Object { [string]$_.event_id }) + @($OsEvents | ForEach-Object { [string]$_.event_id })
    return [ordered]@{
        schemaVersion = 1
        revision = 'windows-etw-semantic-v1'
        generatedAt = Iso-Time ([long]($semantic | Select-Object -Last 1).event_timestamp_unix_ms)
        task = [ordered]@{
            taskID = $taskID; projectID = [string]$first.workspace_path
            agentID = 'workbuddy'; sessionID = $session; turnID = $activity
            startedAt = Iso-Time $started; endedAt = $null
        }
        capture = [ordered]@{
            state = 'degraded'; sources = @('agentTranscript', 'operatingSystem')
            eventCount = $semantic.Count + $OsEvents.Count
            gaps = @(); missingCapabilities = @('macos_specific_adapters', 'unproven_os_task_attribution')
        }
        conversation = [ordered]@{
            userInputs = @($users | ForEach-Object { [string]$_.content })
            modelResponses = @($semantic | Where-Object action_kind -eq 'llm_result' | ForEach-Object { [string]$_.content })
            reasoning = @(); models = $models; traceIDs = $traces
        }
        metrics = [ordered]@{ semanticEvents = $semantic.Count; nativeOsEvents = $OsEvents.Count }
        outboundRequests = $outbound
        externalRequests = @()
        modelResponses = $responses
        journey = [ordered]@{ stages = @(); limitations = @('Windows task journey not reconstructed') }
        activities = [ordered]@{
            tools = @($semantic | Where-Object action_kind -eq 'tool_call')
            mcpCalls = @(); mcpObservation = 'notEstablishedByCapturedSources'
            toolResults = @($semantic | Where-Object action_kind -eq 'tool_result')
            commands = @(); files = $fileActivities; network = @()
            networkFlows = $networkFlows; memory = @()
        }
        analysis = [ordered]@{
            modelEconomics = $null; contextExposure = $null
            memoryEvidence = [ordered]@{ assessment = 'unknown' }
            relaySecurity = $null; egressAudit = $null
            memoryCommits = @(); codeFindings = @(); verification = @()
        }
        events = @($semantic) + @($OsEvents)
        sourceEvents = $sourceEvents
        evidenceReferences = $sourceIds
    }
}

function Send-Packet([Uri]$Endpoint, [string]$DeviceID, [string]$Token, [string]$PacketPath, [int]$Sequence) {
    $body = Get-Content -LiteralPath $PacketPath -Raw -Encoding UTF8
    if ([Text.Encoding]::UTF8.GetByteCount($body) -gt 64MB) { throw 'Task exceeds audit server request limit; no partial upload' }
    Add-Type -AssemblyName System.Net.Http
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    $handler = [Net.Http.HttpClientHandler]::new()
    $handler.AllowAutoRedirect = $false
    $client = [Net.Http.HttpClient]::new($handler)
    try {
        $client.Timeout = [TimeSpan]::FromSeconds(60)
        $client.DefaultRequestHeaders.Authorization = [Net.Http.Headers.AuthenticationHeaderValue]::new('Bearer', $Token)
        [void]$client.DefaultRequestHeaders.Add('X-AgentReins-Device', $DeviceID)
        $content = [Net.Http.StringContent]::new($body, [Text.Encoding]::UTF8, 'application/json')
        try {
            $response = $client.PostAsync($Endpoint, $content).GetAwaiter().GetResult()
            try {
                if (-not $response.IsSuccessStatusCode) { throw "Audit server HTTP $([int]$response.StatusCode)" }
                $receipt = $response.Content.ReadAsStringAsync().GetAwaiter().GetResult() | ConvertFrom-Json
                if ($receipt.persisted -ne $true -or $receipt.serverStatus -ne 'committed' -or
                    [int]$receipt.sequence -ne $Sequence -or [string]::IsNullOrWhiteSpace([string]$receipt.receiptID)) {
                    throw 'Audit server did not confirm durable receipt'
                }
                return [string]$receipt.receiptID
            } finally { $response.Dispose() }
        } finally { $content.Dispose() }
    } finally { $client.Dispose(); $handler.Dispose() }
}

if ($FunctionsOnly) { return }
$configuration = Read-Json $ConfigurationPath
if ($configuration.uploadEnabled -ne $true) { return }
$endpoint = [Uri]$configuration.endpoint
if ($endpoint.Scheme -ne 'https' -or [string]::IsNullOrWhiteSpace([string]$configuration.token) -or
    [string]::IsNullOrWhiteSpace([string]$configuration.deviceID)) {
    throw 'Cloud audit requires HTTPS endpoint, device ID and token'
}
$enabledAt = [DateTimeOffset]::Parse([string]$configuration.enabledAt).ToUnixTimeMilliseconds()
New-Item -ItemType Directory -Path $StateRoot -Force | Out-Null
$pointer = Read-Json (Join-Path $EvidenceRoot 'current-semantic-run.json')
$run = [string]$pointer.run_root
$observer = Read-Json (Join-Path $run 'semantic-observer-process.json')
$manifest = Read-Json ([string]$observer.semantic_manifest_path)
if ([string]$manifest.session_id -ne [string]$observer.session_id) { throw 'Semantic session mismatch' }
if ([string]$pointer.session_id -ne [string]$observer.session_id) { throw 'Semantic pointer session mismatch' }
$events = @((Published-Lines ([string]$manifest.semantic_file) ([long]$manifest.published_bytes)) |
    ForEach-Object { $_ | ConvertFrom-Json })
if ($events.Count -ne [long]$manifest.semantic_events) { throw 'Semantic publication count mismatch' }
$groups = @($events | Where-Object {
    -not [string]::IsNullOrWhiteSpace([string]$_.user_activity_id) -and
    [long]$_.event_timestamp_unix_ms -ge $enabledAt
} | Group-Object session_id, user_activity_id)
$statePath = Join-Path $StateRoot 'upload-state.json'
$savedState = if (Test-Path -LiteralPath $statePath) { Read-Json $statePath } else { $null }
$state = [ordered]@{ deviceID = [string]$configuration.deviceID; tasks = @{} }
if ($null -ne $savedState -and [string]$savedState.deviceID -ne [string]$configuration.deviceID) {
    throw 'Upload state belongs to another enrolled device; refusing to reuse receipts'
}
if ($null -ne $savedState -and $null -ne $savedState.tasks) {
    foreach ($property in $savedState.tasks.PSObject.Properties) {
        $state.tasks[$property.Name] = $property.Value
    }
}
foreach ($group in $groups) {
    $firstUser = @($group.Group | Where-Object action_kind -eq 'user_input' | Sort-Object event_timestamp_unix_ms | Select-Object -First 1)
    $firstRequest = @($group.Group | Where-Object action_kind -eq 'llm_request' | Sort-Object event_timestamp_unix_ms | Select-Object -First 1)
    if ($firstUser.Count -eq 0 -or $firstRequest.Count -eq 0 -or
        [long]$firstUser[0].event_timestamp_unix_ms -lt $enabledAt -or
        [long]$firstRequest[0].event_timestamp_unix_ms -lt $enabledAt) { continue }
    $operations = @($group.Group | ForEach-Object { $_.operation_id } | Where-Object { $_ } | Sort-Object -Unique)
    $osPointerPath = Join-Path $OsEvidenceRoot 'current-os-run.json'
    $osEvents = if (Test-Path -LiteralPath $osPointerPath) {
        $osPointer = Read-Json $osPointerPath
        if ([string]$osPointer.session_id -eq [string]$observer.session_id) {
            Read-Matched-OsEvents ([string]$osPointer.run_root) $operations
        } else { @() }
    } else { @() }
    $snapshot = New-Snapshot $group $osEvents
    if ($null -eq $snapshot) { continue }
    $key = [string]$snapshot.task.taskID
    $fingerprint = Digest (ConvertTo-Json -InputObject $snapshot -Depth 100 -Compress)
    $entry = $state.tasks[$key]
    if ($null -ne $entry -and -not [string]::IsNullOrWhiteSpace([string]$entry.pendingPath)) {
        if ($null -ne $entry.nextRetryAt -and [DateTimeOffset]::Parse([string]$entry.nextRetryAt) -gt [DateTimeOffset]::UtcNow) { continue }
        try {
            $receipt = Send-Packet $endpoint ([string]$configuration.deviceID) ([string]$configuration.token) ([string]$entry.pendingPath) ([int]$entry.sequence)
        } catch {
            $entry.failures = [int]$entry.failures + 1
            $entry.nextRetryAt = [DateTimeOffset]::UtcNow.AddSeconds([Math]::Min(300, [Math]::Pow(2, [Math]::Min(8, $entry.failures)) * 5)).ToString('o')
            $state.tasks[$key] = $entry
            Save-Json $statePath $state
            throw
        }
        $completedPath = [string]$entry.pendingPath
        $entry.receiptID = $receipt
        $entry.pendingPath = $null
        $entry.failures = 0
        $entry.nextRetryAt = $null
        $state.tasks[$key] = $entry
        Save-Json $statePath $state
        Remove-Item -LiteralPath $completedPath -Force
    }
    if ($null -ne $entry -and $entry.fingerprint -eq $fingerprint) { continue }
    $sequence = if ($null -eq $entry) { 1 } else { [int]$entry.sequence + 1 }
    $packet = [ordered]@{
        deviceID = [string]$configuration.deviceID; sequence = $sequence; snapshot = $snapshot
        identity = [ordered]@{
            localUser = [Environment]::UserName
            deviceName = [Environment]::MachineName
            localIPs = @(Local-Private-IPv4)
            observedAt = [DateTime]::UtcNow.ToString('o')
        }
    }
    $packetPath = Join-Path $StateRoot "$($key.Substring(8)).$sequence.json"
    Save-Json $packetPath $packet
    $state.tasks[$key] = [pscustomobject]@{
        sequence = $sequence; fingerprint = $fingerprint; receiptID = $null
        pendingPath = $packetPath; failures = 0; nextRetryAt = $null
    }
    Save-Json $statePath $state
    try {
        $receipt = Send-Packet $endpoint ([string]$configuration.deviceID) ([string]$configuration.token) $packetPath $sequence
    } catch {
        $state.tasks[$key].failures = 1
        $state.tasks[$key].nextRetryAt = [DateTimeOffset]::UtcNow.AddSeconds(10).ToString('o')
        Save-Json $statePath $state
        throw
    }
    $state.tasks[$key].receiptID = $receipt
    $state.tasks[$key].pendingPath = $null
    Save-Json $statePath $state
    Remove-Item -LiteralPath $packetPath -Force
}
