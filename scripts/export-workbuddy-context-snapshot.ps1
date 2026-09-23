[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$WorkBuddyHome,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$ProjectRoot,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$FullOutputPath,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$FilteredOutputPath,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$FullCsvPath,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$FilteredCsvPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$workBuddy = [IO.Path]::GetFullPath($WorkBuddyHome)
$project = [IO.Path]::GetFullPath($ProjectRoot)
$fullOutput = [IO.Path]::GetFullPath($FullOutputPath)
$filteredOutput = [IO.Path]::GetFullPath($FilteredOutputPath)
$fullCsv = [IO.Path]::GetFullPath($FullCsvPath)
$filteredCsv = [IO.Path]::GetFullPath($FilteredCsvPath)

foreach ($path in @($workBuddy, $project)) {
    if (-not (Test-Path -LiteralPath $path -PathType Container)) {
        throw "找不到 WorkBuddy 上下文目录 path=$path"
    }
}
foreach ($path in @($fullOutput, $filteredOutput, $fullCsv, $filteredCsv)) {
    if (Test-Path -LiteralPath $path) {
        throw "上下文快照输出已经存在，拒绝覆盖 path=$path"
    }
}

$candidatePaths = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
$rootPolicyNames = @('SOUL.md', 'IDENTITY.md', 'USER.md', 'MEMORY.md', 'AGENTS.md', 'CLAUDE.md')
foreach ($root in @($workBuddy, $project)) {
    foreach ($name in $rootPolicyNames) {
        $candidate = Join-Path $root $name
        if (Test-Path -LiteralPath $candidate -PathType Leaf) {
            $candidatePaths.Add([IO.Path]::GetFullPath($candidate)) | Out-Null
        }
    }
}

$rootConfigurationNames = @('mcp-approvals.json', 'settings.json', 'config.json')
foreach ($name in $rootConfigurationNames) {
    $candidate = Join-Path $workBuddy $name
    if (Test-Path -LiteralPath $candidate -PathType Leaf) {
        $candidatePaths.Add([IO.Path]::GetFullPath($candidate)) | Out-Null
    }
}
$discoveryRoots = @('skills', 'connectors') |
    ForEach-Object { Join-Path $workBuddy $_ } |
    Where-Object { Test-Path -LiteralPath $_ -PathType Container }
$discoveryNames = @('SKILL.md', 'mcp.json', 'connector-meta.json', 'settings.json', 'config.json')
foreach ($root in $discoveryRoots) {
    Get-ChildItem -LiteralPath $root -Recurse -File -ErrorAction Stop |
        Where-Object { $_.Name -in $discoveryNames } |
        ForEach-Object { $candidatePaths.Add($_.FullName) | Out-Null }
}

function Get-ArtifactCategory {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path
    )

    $leaf = Split-Path -Leaf $Path
    if ($leaf -eq 'SKILL.md') { return 'skill_policy' }
    if ($leaf -in @('mcp.json', 'mcp-approvals.json', 'connector-meta.json')) { return 'mcp_configuration' }
    if ($leaf -in @('settings.json', 'config.json')) { return 'agent_configuration' }
    return 'agent_policy'
}

$capturedAt = [DateTimeOffset]::UtcNow
$artifacts = @(
    $candidatePaths |
        Sort-Object |
        ForEach-Object {
            $file = Get-Item -LiteralPath $_
            [pscustomobject]@{
                category = Get-ArtifactCategory -Path $file.FullName
                path = $file.FullName
                name = $file.Name
                size_bytes = $file.Length
                modified_at = $file.LastWriteTimeUtc.ToString('o')
                sha256 = (Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
                content = [IO.File]::ReadAllText($file.FullName)
            }
        }
)

$fullSnapshot = [pscustomobject]@{
    schema_version = '0.4.0'
    captured_at_unix_ms = $capturedAt.ToUnixTimeMilliseconds()
    captured_at = $capturedAt.ToString('o')
    agent_kind = 'work_buddy'
    workbuddy_home = $workBuddy
    project_root = $project
    artifacts = $artifacts
}
$filteredArtifacts = @(
    $artifacts | ForEach-Object {
        [pscustomobject]@{
            category = $_.category
            path = $_.path
            name = $_.name
            size_bytes = $_.size_bytes
            modified_at = $_.modified_at
            sha256 = $_.sha256
            content = $null
        }
    }
)
$filteredSnapshot = [pscustomobject]@{
    schema_version = $fullSnapshot.schema_version
    captured_at_unix_ms = $fullSnapshot.captured_at_unix_ms
    captured_at = $fullSnapshot.captured_at
    agent_kind = $fullSnapshot.agent_kind
    workbuddy_home = $fullSnapshot.workbuddy_home
    project_root = $fullSnapshot.project_root
    artifacts = $filteredArtifacts
}

[IO.File]::WriteAllText($fullOutput, ($fullSnapshot | ConvertTo-Json -Depth 6), [Text.UTF8Encoding]::new($false))
[IO.File]::WriteAllText($filteredOutput, ($filteredSnapshot | ConvertTo-Json -Depth 6), [Text.UTF8Encoding]::new($false))
$artifacts | Export-Csv -LiteralPath $fullCsv -NoTypeInformation -Encoding UTF8
$filteredArtifacts | Export-Csv -LiteralPath $filteredCsv -NoTypeInformation -Encoding UTF8

[pscustomobject]@{
    status = 'completed'
    artifact_count = $artifacts.Count
    full_output_path = $fullOutput
    filtered_output_path = $filteredOutput
    full_csv_path = $fullCsv
    filtered_csv_path = $filteredCsv
} | ConvertTo-Json -Compress
