[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$EvidenceRoot,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$CredentialFile,

    [Parameter(Mandatory = $true)]
    [ValidateRange(1, [long]::MaxValue)]
    [long]$OsSourceRows,

    [Parameter(Mandatory = $true)]
    [ValidateRange(1, [int]::MaxValue)]
    [int]$LogicalRounds,

    [Parameter(Mandatory = $true)]
    [ValidateRange(1, [int]::MaxValue)]
    [int]$AnalyzedSessions
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Write-ProgressRecord {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path,
        [Parameter(Mandatory = $true)]
        [string]$Stage,
        [Parameter(Mandatory = $true)]
        [string]$Status
    )

    $record = [ordered]@{
        recorded_at = [DateTimeOffset]::UtcNow.ToString('o')
        stage = $Stage
        status = $Status
    }
    [IO.File]::AppendAllLines($Path, [string[]]@(($record | ConvertTo-Json -Compress)), [Text.UTF8Encoding]::new($false))
}

function Get-NonEmptyLineCount {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path
    )

    $count = 0L
    foreach ($line in [IO.File]::ReadLines($Path, [Text.UTF8Encoding]::new($false))) {
        if (-not [string]::IsNullOrWhiteSpace($line)) {
            $count += 1
        }
    }
    $count
}

function Get-PlaintextOccurrenceCount {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path,
        [Parameter(Mandatory = $true)]
        [string]$Plaintext
    )

    $count = 0L
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

$evidence = [IO.Path]::GetFullPath($EvidenceRoot)
$credentialPath = [IO.Path]::GetFullPath($CredentialFile)
$summaryPath = Join-Path $evidence 'gate0-summary.json'
$manifestPath = Join-Path $evidence 'manifest.csv'
$progressPath = Join-Path $evidence 'finalization-progress.ndjson'
foreach ($path in @($evidence, $credentialPath)) {
    if (-not (Test-Path -LiteralPath $path)) {
        throw "Gate 0 收口依赖不存在 path=$path"
    }
}
foreach ($path in @($summaryPath, $manifestPath)) {
    if (Test-Path -LiteralPath $path) {
        throw "Gate 0 收口输出已存在，拒绝覆盖 path=$path"
    }
}
if (Test-Path -LiteralPath $progressPath) {
    Write-ProgressRecord -Path $progressPath -Stage 'resume_after_failure' -Status 'started'
}

$artifactNames = @(
    'workbuddy-semantic.full.ndjson',
    'workbuddy-os.full.ndjson',
    'workbuddy-os.filtered.ndjson',
    'workbuddy-correlation.full.ndjson',
    'workbuddy-correlation.filtered.ndjson',
    'workbuddy-correlation.full.csv',
    'workbuddy-correlation.filtered.csv',
    'workbuddy-context.full.json',
    'workbuddy-context.filtered.json',
    'workbuddy-context.full.csv',
    'workbuddy-context.filtered.csv',
    'credential-catalog.full.json',
    'workbuddy-gate0-report.full.html',
    'workbuddy-gate0-report.filtered.html',
    'session-manifest.csv',
    'source-manifest.csv',
    'correlator-stdout.json',
    'correlator-stderr.ndjson'
)
foreach ($artifactName in $artifactNames) {
    $artifactPath = Join-Path $evidence $artifactName
    if (-not (Test-Path -LiteralPath $artifactPath -PathType Leaf)) {
        throw "Gate 0 证据不完整 artifact=$artifactName path=$artifactPath"
    }
}
$semanticPartDirectory = Join-Path $evidence 'semantic-parts'
if (-not (Test-Path -LiteralPath $semanticPartDirectory -PathType Container)) {
    throw "Gate 0 分轮语义证据目录不存在 path=$semanticPartDirectory"
}
$semanticPartNames = @(Get-ChildItem -LiteralPath $semanticPartDirectory -File -Filter '*.ndjson' | Sort-Object Name | ForEach-Object {
    "semantic-parts/$($_.Name)"
})
if ($semanticPartNames.Count -eq 0) {
    throw "Gate 0 分轮语义证据为空 path=$semanticPartDirectory"
}

Write-ProgressRecord -Path $progressPath -Stage 'validate_inputs' -Status 'completed'
$token = (Get-Content -LiteralPath $credentialPath -Raw -Encoding UTF8).TrimEnd()
if ($token.Length -eq 0) {
    throw "测试凭据为空 path=$credentialPath"
}
$filteredNames = @(
    'workbuddy-correlation.filtered.ndjson',
    'workbuddy-correlation.filtered.csv',
    'workbuddy-context.filtered.json',
    'workbuddy-context.filtered.csv',
    'workbuddy-gate0-report.filtered.html',
    'workbuddy-os.filtered.ndjson'
)
$filteredOccurrences = 0L
foreach ($artifactName in $filteredNames) {
    $filteredOccurrences += Get-PlaintextOccurrenceCount -Path (Join-Path $evidence $artifactName) -Plaintext $token
}
if ($filteredOccurrences -ne 0) {
    throw "过滤证据仍包含测试凭据原文 occurrence_count=$filteredOccurrences"
}
$fullOccurrences = Get-PlaintextOccurrenceCount -Path (Join-Path $evidence 'workbuddy-semantic.full.ndjson') -Plaintext $token
Write-ProgressRecord -Path $progressPath -Stage 'credential_scan' -Status 'completed'

$lineCounts = @{}
foreach ($artifactName in @(
    'workbuddy-semantic.full.ndjson',
    'workbuddy-os.full.ndjson',
    'workbuddy-os.filtered.ndjson',
    'workbuddy-correlation.full.ndjson',
    'workbuddy-correlation.filtered.ndjson'
)) {
    $lineCounts[$artifactName] = Get-NonEmptyLineCount -Path (Join-Path $evidence $artifactName)
}
$matchedSourceRows = 0L
foreach ($line in [IO.File]::ReadLines((Join-Path $evidence 'workbuddy-os.filtered.ndjson'), [Text.UTF8Encoding]::new($false))) {
    if ([string]::IsNullOrWhiteSpace($line)) {
        continue
    }
    $event = $line | ConvertFrom-Json
    $matchedSourceRows += [long]$event.analysis.source_record_count
}
Write-ProgressRecord -Path $progressPath -Stage 'count_evidence' -Status 'completed'

$context = Get-Content -LiteralPath (Join-Path $evidence 'workbuddy-context.full.json') -Raw -Encoding UTF8 | ConvertFrom-Json
$fullReportText = Get-Content -LiteralPath (Join-Path $evidence 'workbuddy-gate0-report.full.html') -Raw -Encoding UTF8
$activityCount = ([regex]::Matches($fullReportText, "<article class='activity'>")).Count
$summary = [ordered]@{
    logical_rounds = $LogicalRounds
    analyzed_sessions = $AnalyzedSessions
    semantic_events = $lineCounts['workbuddy-semantic.full.ndjson']
    os_full_source_rows = $OsSourceRows
    os_full_retained_rows = $lineCounts['workbuddy-os.full.ndjson']
    os_filtered_source_rows = $OsSourceRows
    os_filtered_matched_source_rows = $matchedSourceRows
    os_filtered_retained_rows = $lineCounts['workbuddy-os.filtered.ndjson']
    correlation_full_rows = $lineCounts['workbuddy-correlation.full.ndjson']
    correlation_filtered_rows = $lineCounts['workbuddy-correlation.filtered.ndjson']
    context_artifacts = @($context.artifacts).Count
    report_user_activities = $activityCount
    filtered_plaintext_credential_occurrences = $filteredOccurrences
    full_semantic_plaintext_credential_occurrences = $fullOccurrences
    evidence_root = $evidence
}
$temporarySummaryPath = "$summaryPath.$([Guid]::NewGuid().ToString('N')).tmp"
[IO.File]::WriteAllText($temporarySummaryPath, ($summary | ConvertTo-Json -Depth 5), [Text.UTF8Encoding]::new($false))
Move-Item -LiteralPath $temporarySummaryPath -Destination $summaryPath
Write-ProgressRecord -Path $progressPath -Stage 'publish_summary' -Status 'completed'

$manifestNames = @($artifactNames) + $semanticPartNames + @('gate0-summary.json')
$manifestRows = foreach ($artifactName in $manifestNames) {
    $artifactPath = Join-Path $evidence $artifactName
    [pscustomobject]@{
        artifact = $artifactName
        bytes = (Get-Item -LiteralPath $artifactPath).Length
        sha256 = (Get-FileHash -LiteralPath $artifactPath -Algorithm SHA256).Hash.ToLowerInvariant()
        evidence_scope = 'workbuddy_8_round_gate0'
    }
}
$temporaryManifestPath = "$manifestPath.$([Guid]::NewGuid().ToString('N')).tmp"
$manifestRows | Export-Csv -LiteralPath $temporaryManifestPath -NoTypeInformation -Encoding UTF8
Move-Item -LiteralPath $temporaryManifestPath -Destination $manifestPath
Write-ProgressRecord -Path $progressPath -Stage 'publish_manifest' -Status 'completed'

$summary | ConvertTo-Json -Compress
