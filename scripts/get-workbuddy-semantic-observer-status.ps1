[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$RunRoot
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$run = [IO.Path]::GetFullPath($RunRoot)
$statePath = Join-Path $run 'semantic-observer-process.json'
if (-not (Test-Path -LiteralPath $statePath -PathType Leaf)) {
    throw "语义 Observer 状态文件不存在 path=$statePath"
}
$state = Get-Content -LiteralPath $statePath -Raw -Encoding UTF8 | ConvertFrom-Json
$process = $null
try {
    $process = [Diagnostics.Process]::GetProcessById([int]$state.observer_process_id)
}
catch [ArgumentException] {
    $process = $null
}
$runningStatus = 'stopped'
$running = $false
$processIdentityError = $null
if ($null -ne $process) {
    try {
        $mainModule = $process.MainModule
        if ($null -eq $mainModule) {
            $runningStatus = 'unknown'
            $running = $null
            $processIdentityError = '当前权限无法读取语义 Observer 进程模块'
        }
        else {
            $observedPath = $mainModule.FileName
            $observedStartTime = $process.StartTime.ToUniversalTime().ToString('o')
            if ($observedPath -eq $state.observer_path -and $observedStartTime -eq $state.observer_started_at) {
                $runningStatus = 'running'
                $running = $true
            }
            else {
                $runningStatus = 'identity_mismatch'
            }
        }
    }
    catch [ComponentModel.Win32Exception] {
        $runningStatus = 'unknown'
        $running = $null
        $processIdentityError = $_.Exception.Message
    }
}
$health = $null
if (Test-Path -LiteralPath $state.health_output_path -PathType Leaf) {
    $healthLine = Get-Content -LiteralPath $state.health_output_path -Tail 1 -Encoding UTF8
    if (-not [string]::IsNullOrWhiteSpace($healthLine)) {
        $health = $healthLine | ConvertFrom-Json
    }
}
$publishedManifest = $null
$semanticManifestPath = $null
if ($state.PSObject.Properties.Name -contains 'semantic_manifest_path') {
    $semanticManifestPath = [string]$state.semantic_manifest_path
}
if (-not [string]::IsNullOrWhiteSpace($semanticManifestPath) -and (Test-Path -LiteralPath $semanticManifestPath -PathType Leaf)) {
    $publishedManifest = Get-Content -LiteralPath $semanticManifestPath -Raw -Encoding UTF8 | ConvertFrom-Json
}

[pscustomobject]@{
    running = $running
    running_status = $runningStatus
    process_identity_error = $processIdentityError
    observer_process_id = $state.observer_process_id
    observer_started_at = $state.observer_started_at
    run_root = $state.run_root
    project_root = $state.project_root
    identity_path = $state.identity_path
    session_id = $state.session_id
    semantic_manifest_path = $semanticManifestPath
    published_manifest = $publishedManifest
    health = $health
} | ConvertTo-Json -Depth 8 -Compress
