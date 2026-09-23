param(
    [Parameter(Mandatory = $true)]
    [string]$SourceRoot,
    [Parameter(Mandatory = $true)]
    [string]$EvidenceRoot
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$source = [IO.Path]::GetFullPath($SourceRoot)
$evidence = [IO.Path]::GetFullPath($EvidenceRoot)
$segmentsPath = Join-Path $evidence "segments"
$exporterPath = Join-Path $source "scripts\export-process-evidence-csv.ps1"
foreach ($requiredPath in @($segmentsPath, $exporterPath)) {
    if (-not (Test-Path -LiteralPath $requiredPath)) {
        throw "未找到滚动证据导出依赖 path=$requiredPath"
    }
}
$paths = [ordered]@{
    full_ndjson = Join-Path $evidence "observer-combined.full.ndjson"
    filtered_ndjson = Join-Path $evidence "observer-combined.filtered.ndjson"
    full_csv = Join-Path $evidence "observer-combined.full.csv"
    filtered_csv = Join-Path $evidence "observer-combined.filtered.csv"
    manifest = Join-Path $evidence "export-manifest.csv"
}
foreach ($path in $paths.Values) {
    if (Test-Path -LiteralPath $path) {
        throw "滚动证据导出文件已存在，拒绝覆盖 path=$path"
    }
}

$manifestFile = Get-ChildItem -LiteralPath $segmentsPath -Filter "*-manifest-*.json" |
    Sort-Object Name |
    Select-Object -Last 1
if ($null -eq $manifestFile) {
    throw "未找到滚动证据 manifest path=$segmentsPath"
}
$rollingManifest = Get-Content -LiteralPath $manifestFile.FullName -Raw -Encoding UTF8 | ConvertFrom-Json
if ($rollingManifest.complete -ne $true) {
    throw "滚动证据最终 manifest 未完成 path=$($manifestFile.FullName)"
}

$utf8 = [Text.UTF8Encoding]::new($false)
$fullWriter = [IO.StreamWriter]::new($paths.full_ndjson, $false, $utf8)
$filteredWriter = [IO.StreamWriter]::new($paths.filtered_ndjson, $false, $utf8)
try {
    foreach ($segment in @($rollingManifest.segments | Sort-Object index)) {
        $fullPath = Join-Path $segmentsPath $segment.full_file
        $filteredPath = Join-Path $segmentsPath $segment.filtered_file
        foreach ($requiredPath in @($fullPath, $filteredPath)) {
            if (-not (Test-Path -LiteralPath $requiredPath)) {
                throw "最终 manifest 引用的分段不存在 path=$requiredPath"
            }
        }
        foreach ($line in [IO.File]::ReadLines($fullPath, $utf8)) {
            $fullWriter.WriteLine($line)
        }
        foreach ($line in [IO.File]::ReadLines($filteredPath, $utf8)) {
            $filteredWriter.WriteLine($line)
        }
    }
}
finally {
    $fullWriter.Dispose()
    $filteredWriter.Dispose()
}

$summary = & $exporterPath -FullInputPath $paths.full_ndjson -FilteredInputPath $paths.filtered_ndjson -FullCsvPath $paths.full_csv -FilteredCsvPath $paths.filtered_csv |
    ConvertFrom-Json
$capturedAt = (Get-Date).ToString("o")
$manifestRows = @(
    [pscustomobject]@{
        dataset = "persistent-observer-process"
        captured_at = $capturedAt
        source_manifest = $manifestFile.Name
        source_segments = @($rollingManifest.segments).Count
        full_file = Split-Path -Leaf $paths.full_csv
        filtered_file = Split-Path -Leaf $paths.filtered_csv
        full_rows = $summary.full_rows
        filtered_rows = $summary.filtered_rows
        full_sha256 = (Get-FileHash -LiteralPath $paths.full_csv -Algorithm SHA256).Hash.ToLowerInvariant()
        filtered_sha256 = (Get-FileHash -LiteralPath $paths.filtered_csv -Algorithm SHA256).Hash.ToLowerInvariant()
        filter_rule = "WorkBuddy实例归因；过滤版移除命令行等敏感明细"
    }
)
$manifestRows | Export-Csv -LiteralPath $paths.manifest -NoTypeInformation -Encoding UTF8
$manifestRows[0] | ConvertTo-Json -Compress
