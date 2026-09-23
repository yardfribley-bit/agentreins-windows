[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$SourceRoot,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$TestRoot
)

$ErrorActionPreference = 'Stop'
$source = [IO.Path]::GetFullPath($SourceRoot)
$target = [IO.Path]::GetFullPath($TestRoot)
$template = Join-Path $source 'files\editable-template.txt'

if (-not (Test-Path -LiteralPath $template -PathType Leaf)) {
    throw "找不到可编辑夹具模板 path=$template"
}

New-Item -ItemType Directory -Path $target -Force | Out-Null
Copy-Item -LiteralPath $template -Destination (Join-Path $target 'editable.txt') -Force
Copy-Item -LiteralPath (Join-Path $source 'files\read-only-input.txt') -Destination (Join-Path $target 'read-only-input.txt') -Force
Copy-Item -LiteralPath (Join-Path $source 'search') -Destination $target -Recurse -Force

[pscustomobject]@{
    source_root = $source
    test_root = $target
    editable_sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $target 'editable.txt')).Hash
    read_only_sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $target 'read-only-input.txt')).Hash
    search_file_count = @(Get-ChildItem -LiteralPath (Join-Path $target 'search') -File -Recurse).Count
} | ConvertTo-Json -Compress
