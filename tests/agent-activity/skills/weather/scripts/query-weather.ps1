[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$City,

    [Parameter(Mandatory = $true)]
    [ValidatePattern('^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$')]
    [string]$RunId,

    [Parameter(Mandatory = $true)]
    [ValidatePattern('^http://127\.0\.0\.1:\d{1,5}$')]
    [string]$BaseUrl
)

$ErrorActionPreference = 'Stop'
$uri = '{0}/weather?city={1}&run_id={2}&channel=skill' -f `
    $BaseUrl.TrimEnd('/'), `
    [Uri]::EscapeDataString($City), `
    [Uri]::EscapeDataString($RunId)

try {
    $result = Invoke-RestMethod -Method Get -Uri $uri -TimeoutSec 5
    $result | ConvertTo-Json -Depth 8 -Compress
}
catch {
    throw "受控天气 Skill 请求失败 uri=$uri error=$($_.Exception.Message)"
}
