[CmdletBinding()]
param(
    [string]$WorkBuddyPath = (Join-Path $env:LOCALAPPDATA 'Programs\WorkBuddy\WorkBuddy.exe'),
    [string]$ProjectRoot = (Join-Path $env:USERPROFILE '.workbuddy')
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if (-not (Test-Path -LiteralPath $WorkBuddyPath -PathType Leaf)) { throw "未找到 WorkBuddy，请先安装或指定 WorkBuddyPath：$WorkBuddyPath" }
if (-not (Test-Path -LiteralPath $ProjectRoot -PathType Container)) { throw "WorkBuddy 数据目录不存在，请先运行 WorkBuddy 或指定 ProjectRoot：$ProjectRoot" }
$signature = Get-AuthenticodeSignature -LiteralPath $WorkBuddyPath
if ($signature.Status -ne 'Valid' -or $null -eq $signature.SignerCertificate) { throw "WorkBuddy 程序签名未通过核验：$($signature.Status)" }
$root = Join-Path $env:LOCALAPPDATA 'AgentReins'
New-Item -ItemType Directory -Path $root -Force | Out-Null
$identityPath = Join-Path $root 'workbuddy-identity.json'
$profilePath = Join-Path $root 'observer-profile.json'
if (Test-Path -LiteralPath $profilePath) { throw '观测配置已存在，拒绝自动覆盖' }
$identity = [ordered]@{
    executable_path = [IO.Path]::GetFullPath($WorkBuddyPath)
    file_version = (Get-Item -LiteralPath $WorkBuddyPath).VersionInfo.FileVersion
    sha256 = (Get-FileHash -LiteralPath $WorkBuddyPath -Algorithm SHA256).Hash.ToLowerInvariant()
    signer_subject = $signature.SignerCertificate.Subject
}
if (Test-Path -LiteralPath $identityPath) { throw '身份文件已存在但缺少配置，请检查后手动恢复；拒绝覆盖' }
[IO.File]::WriteAllText($identityPath, ($identity | ConvertTo-Json), [Text.UTF8Encoding]::new($false))
$profile = [ordered]@{ project_root = [IO.Path]::GetFullPath($ProjectRoot); identity_path = $identityPath }
[IO.File]::WriteAllText($profilePath, ($profile | ConvertTo-Json), [Text.UTF8Encoding]::new($false))
$profile | ConvertTo-Json -Compress
