$ErrorActionPreference = 'Stop'
$adapter = Get-NetIPConfiguration |
    Where-Object { $_.IPv4DefaultGateway -ne $null } |
    Select-Object -First 1 InterfaceAlias, IPv4Address, IPv4DefaultGateway

[pscustomobject]@{
    marker = 'AGENTREINS_POWERSHELL_STDIN_V1'
    run_id = $env:AGENTREINS_RUN_ID
    adapter = $adapter
} | ConvertTo-Json -Depth 5 -Compress
