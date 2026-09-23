param(
    [Parameter(Mandatory = $true)]
    [string]$EvidenceRoot,
    [Parameter(Mandatory = $true)]
    [string]$ArchivePath,
    [Parameter(Mandatory = $true)]
    [string]$ExchangePath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$evidence = [IO.Path]::GetFullPath($EvidenceRoot)
$archive = [IO.Path]::GetFullPath($ArchivePath)
if (-not (Test-Path -LiteralPath $evidence)) {
    throw "证据目录不存在 path=$evidence"
}
foreach ($path in @($archive, $ExchangePath)) {
    if (Test-Path -LiteralPath $path) {
        throw "证据归档目标已存在，拒绝覆盖 path=$path"
    }
}

[Reflection.Assembly]::LoadWithPartialName("System.IO.Compression") | Out-Null
[Reflection.Assembly]::LoadWithPartialName("System.IO.Compression.FileSystem") | Out-Null
$evidenceParent = Split-Path -Parent $evidence
$archiveStream = [IO.File]::Open($archive, [IO.FileMode]::CreateNew)
$zip = $null
$archiveError = $null
try {
    $zip = [IO.Compression.ZipArchive]::new(
        $archiveStream,
        [IO.Compression.ZipArchiveMode]::Create,
        $false
    )
    foreach ($file in Get-ChildItem -LiteralPath $evidence -File -Recurse | Sort-Object FullName) {
        $entryName = $file.FullName.Substring($evidenceParent.Length).TrimStart("\").Replace("\", "/")
        [IO.Compression.ZipFileExtensions]::CreateEntryFromFile(
            $zip,
            $file.FullName,
            $entryName,
            [IO.Compression.CompressionLevel]::Optimal
        ) | Out-Null
    }
}
catch {
    $archiveError = $_
}
finally {
    if ($null -ne $zip) {
        $zip.Dispose()
    }
    else {
        $archiveStream.Dispose()
    }
}
if ($null -ne $archiveError) {
    Remove-Item -LiteralPath $archive -Force
    throw $archiveError
}
$bytes = [IO.File]::ReadAllBytes($archive)
[IO.File]::WriteAllBytes($ExchangePath, $bytes)
$sourceHash = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
$exchangeHash = (Get-FileHash -LiteralPath $ExchangePath -Algorithm SHA256).Hash.ToLowerInvariant()
if ($sourceHash -ne $exchangeHash) {
    throw "证据归档回传哈希不一致 source=$sourceHash exchange=$exchangeHash"
}
[pscustomobject]@{
    source_path = $archive
    exchange_path = $ExchangePath
    bytes = $bytes.LongLength
    sha256 = $sourceHash
} | ConvertTo-Json -Compress
