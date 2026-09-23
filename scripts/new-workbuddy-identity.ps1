param(
    [Parameter(Mandatory = $true)]
    [string]$WorkBuddyPath,
    [Parameter(Mandatory = $true)]
    [string]$OutputPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$workBuddy = [IO.Path]::GetFullPath($WorkBuddyPath)
$output = [IO.Path]::GetFullPath($OutputPath)
if (-not (Test-Path -LiteralPath $workBuddy)) {
    throw "WorkBuddy 主程序不存在 path=$workBuddy"
}
if (Test-Path -LiteralPath $output) {
    throw "WorkBuddy 身份文件已存在，拒绝覆盖 path=$output"
}
$parent = Split-Path -Parent $output
if (-not (Test-Path -LiteralPath $parent)) {
    New-Item -ItemType Directory -Path $parent | Out-Null
}
$signature = Get-AuthenticodeSignature -LiteralPath $workBuddy
if ($signature.Status -ne [Management.Automation.SignatureStatus]::Valid -or $null -eq $signature.SignerCertificate) {
    throw "WorkBuddy Authenticode 签名无效 path=$workBuddy status=$($signature.Status)"
}
$identity = [pscustomobject]@{
    executable_path = $workBuddy
    file_version = (Get-Item -LiteralPath $workBuddy).VersionInfo.FileVersion
    sha256 = (Get-FileHash -LiteralPath $workBuddy -Algorithm SHA256).Hash.ToLowerInvariant()
    signer_subject = $signature.SignerCertificate.Subject
}
[IO.File]::WriteAllText($output, ($identity | ConvertTo-Json), [Text.UTF8Encoding]::new($false))
$identity | ConvertTo-Json -Compress
