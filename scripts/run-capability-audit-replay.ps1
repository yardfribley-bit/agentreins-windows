[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$AuditExecutablePath,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$SemanticInput,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$OsInput,

    [Parameter()]
    [string]$WorkspaceRoot,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$OutputPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$auditPath = [IO.Path]::GetFullPath($AuditExecutablePath)
$semanticFile = [IO.Path]::GetFullPath($SemanticInput)
$osFile = [IO.Path]::GetFullPath($OsInput)
$outputFile = [IO.Path]::GetFullPath($OutputPath)
foreach ($requiredPath in @($auditPath, $semanticFile, $osFile)) {
    if (-not (Test-Path -LiteralPath $requiredPath -PathType Leaf)) {
        throw "Capability Audit 回放依赖不存在 path=$requiredPath"
    }
}
if (Test-Path -LiteralPath $outputFile) {
    throw "Capability Audit 报告已存在，拒绝覆盖 path=$outputFile"
}
$outputDirectory = Split-Path -Parent $outputFile
if (-not (Test-Path -LiteralPath $outputDirectory)) {
    New-Item -ItemType Directory -Path $outputDirectory | Out-Null
}

$arguments = @(
    '--semantic-input', $semanticFile,
    '--os-input', $osFile
)
if (-not [string]::IsNullOrWhiteSpace($WorkspaceRoot)) {
    $arguments += @('--workspace-root', [IO.Path]::GetFullPath($WorkspaceRoot))
}
$arguments += @('--output', $outputFile)
$stdout = & $auditPath @arguments
if ($LASTEXITCODE -ne 0) {
    throw "Capability Audit 回放失败 exit_code=$LASTEXITCODE"
}
$summary = $stdout | ConvertFrom-Json
$report = Get-Content -LiteralPath $outputFile -Raw -Encoding UTF8 | ConvertFrom-Json
if ($report.mode -ne 'audit') {
    throw "Capability Audit 报告模式错误 mode=$($report.mode)"
}
if ($report.summary.enforcement_actions_applied -ne 0) {
    throw "Capability Audit 不得应用控制 effect_count=$($report.summary.enforcement_actions_applied)"
}
if (@($report.decisions | Where-Object { $_.control_effect.applied -ne $false }).Count -ne 0) {
    throw 'Capability Audit 报告包含已应用控制效果'
}
$summary | ConvertTo-Json -Compress
