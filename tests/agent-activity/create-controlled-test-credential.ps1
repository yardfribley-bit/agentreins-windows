[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$TokenFile
)

$ErrorActionPreference = 'Stop'
$tokenPath = [IO.Path]::GetFullPath($TokenFile)
if (Test-Path -LiteralPath $tokenPath) {
    throw "测试凭据已经存在，拒绝覆盖 path=$tokenPath"
}

New-Item -ItemType Directory -Path (Split-Path -Parent $tokenPath) -Force | Out-Null
$randomBytes = New-Object byte[] 32
$random = [Security.Cryptography.RandomNumberGenerator]::Create()
try {
    $random.GetBytes($randomBytes)
}
finally {
    $random.Dispose()
}

$token = [Convert]::ToBase64String($randomBytes)
[IO.File]::WriteAllText($tokenPath, $token, [Text.UTF8Encoding]::new($false))

[pscustomobject]@{
    token_file = $tokenPath
    token_sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $tokenPath).Hash
    token_length = $token.Length
    created_at = [DateTimeOffset]::UtcNow.ToString('o')
} | ConvertTo-Json -Compress
