$ErrorActionPreference='Stop'
. (Join-Path $PSScriptRoot '../scripts/upload-agentreins-audit.ps1') -EvidenceRoot 'unused' -ConfigurationPath 'unused' -StateRoot 'unused' -FunctionsOnly
function Event([string]$kind, [string]$id, [long]$time, [string]$body) {
    [pscustomobject]@{
        action_kind=$kind; event_id=$id; event_timestamp_unix_ms=$time
        session_id='native-test-session'; user_activity_id='native-test-turn'
        content=$body; trace_id='native-test-trace'; tool_call_id=$null
        model_id='test-model'; process_id=123; conversation_request_id='native-test-request'
        source_record_id='test-source'; workspace_path='test-workspace'; operation_id=$null
    }
}
$events=@((Event 'user_input' 'u1' 1700000000000 'test user request'), (Event 'llm_request' 'q1' 1700000000001 'test model context'), (Event 'llm_result' 'r1' 1700000000002 'test response'))
$snapshot=New-Snapshot ([pscustomobject]@{Group=$events}) @()
if ($null -eq $snapshot -or $snapshot.schemaVersion -ne 1) { throw 'Snapshot contract missing' }
if (-not $snapshot.task.taskID.StartsWith('windows:')) { throw 'Windows task identity missing' }
if ($snapshot.sourceEvents.Count -ne 3 -or $snapshot.events.Count -ne 3) { throw 'Raw evidence was lost' }
if ($snapshot.sourceEvents[0].userIntent -ne 'test user request') { throw 'User request was lost' }
if ($snapshot.sourceEvents[1].modelPrompt -ne 'test model context') { throw 'Model context was lost' }
$again=New-Snapshot ([pscustomobject]@{Group=$events}) @()
if ($again.task.taskID -ne $snapshot.task.taskID) { throw 'Task IDs must be deterministic for retries' }
$empty=New-Snapshot ([pscustomobject]@{Group=@($events[1])}) @()
if ($null -ne $empty) { throw 'Must not invent a task without a user request' }
Write-Output 'Windows audit snapshot tests passed'
