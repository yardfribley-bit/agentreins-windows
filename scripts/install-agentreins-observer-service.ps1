[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$InstallRoot,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$EvidenceRoot,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$IdentityPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$principal = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    $scriptPath = [IO.Path]::GetFullPath($PSCommandPath)
    $escapedScript = $scriptPath.Replace("'", "''")
    $escapedInstallRoot = $InstallRoot.Replace("'", "''")
    $escapedEvidenceRoot = $EvidenceRoot.Replace("'", "''")
    $escapedIdentityPath = $IdentityPath.Replace("'", "''")
    $elevatedCommand = "& '$escapedScript' -InstallRoot '$escapedInstallRoot' -EvidenceRoot '$escapedEvidenceRoot' -IdentityPath '$escapedIdentityPath'"
    $encodedCommand = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($elevatedCommand))
    $powerShellPath = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
    $elevatedProcess = Start-Process -FilePath $powerShellPath -ArgumentList @('-NoProfile', '-NonInteractive', '-WindowStyle', 'Hidden', '-ExecutionPolicy', 'Bypass', '-EncodedCommand', $encodedCommand) -Verb RunAs -WindowStyle Hidden -Wait -PassThru
    if ($elevatedProcess.ExitCode -ne 0) {
        throw "提升权限安装 AgentReins Observer 服务失败 exit_code=$($elevatedProcess.ExitCode)"
    }
    [pscustomobject]@{
        schema_version = '0.1.0'
        service_name = 'AgentReinsObserver'
        elevated = $true
    } | ConvertTo-Json -Compress
    return
}

$serviceName = 'AgentReinsObserver'
$install = [IO.Path]::GetFullPath($InstallRoot)
$evidence = [IO.Path]::GetFullPath($EvidenceRoot)
$sourceIdentity = [IO.Path]::GetFullPath($IdentityPath)
$servicePath = Join-Path $install 'agentreins-observer-service.exe'
$collectorPath = Join-Path $install 'etw-collector.exe'
foreach ($requiredPath in @($servicePath, $collectorPath, $sourceIdentity)) {
    if (-not (Test-Path -LiteralPath $requiredPath -PathType Leaf)) {
        throw "Observer 服务安装依赖不存在 path=$requiredPath"
    }
}
if (-not (Test-Path -LiteralPath $evidence -PathType Container)) {
    New-Item -ItemType Directory -Path $evidence | Out-Null
}

$productRoot = Join-Path $env:ProgramData 'AgentReins'
$configRoot = Join-Path $productRoot 'config'
$runtimeRoot = Join-Path $productRoot 'runtime'
$logRoot = Join-Path $productRoot 'logs'
foreach ($directory in @($configRoot, $runtimeRoot, $logRoot)) {
    if (-not (Test-Path -LiteralPath $directory -PathType Container)) {
        New-Item -ItemType Directory -Path $directory | Out-Null
    }
}
$installedIdentity = Join-Path $configRoot 'agent-identity.json'
Copy-Item -LiteralPath $sourceIdentity -Destination $installedIdentity -Force
$configurationPath = Join-Path $configRoot 'observer-service.json'
$previousConfiguration = if (Test-Path -LiteralPath $configurationPath -PathType Leaf) {
    Get-Content -LiteralPath $configurationPath -Raw -Encoding UTF8 | ConvertFrom-Json
}
else {
    $null
}
$configuration = [ordered]@{
    schema_version = '0.1.0'
    evidence_root = $evidence
    identity_path = $installedIdentity
    install_root = $install
}

$existing = Get-CimInstance -ClassName Win32_Service -Filter "Name = '$serviceName'"
if ($null -ne $existing) {
    $configuredPath = ([string]$existing.PathName).Trim('"')
    if (-not [StringComparer]::OrdinalIgnoreCase.Equals([IO.Path]::GetFullPath($configuredPath), $servicePath)) {
        $sameBinaryName = [StringComparer]::OrdinalIgnoreCase.Equals(
            [IO.Path]::GetFileName($configuredPath),
            'agentreins-observer-service.exe'
        )
        $ownedConfiguration = $null -ne $previousConfiguration -and $previousConfiguration.schema_version -eq '0.1.0'
        if (-not $sameBinaryName -or -not $ownedConfiguration) {
            throw "同名 Windows 服务不能证明属于 AgentReins，拒绝替换 service=$serviceName expected=$servicePath actual=$configuredPath"
        }
    }
    if ($existing.State -ne 'Stopped') {
        $stopOutput = & sc.exe stop $serviceName 2>&1
        if ($LASTEXITCODE -ne 0) {
            throw "无法停止旧 Observer 服务 service=$serviceName output=$($stopOutput -join ' ')"
        }
    }
    $deleteOutput = & sc.exe delete $serviceName 2>&1
    if ($LASTEXITCODE -ne 0) {
        throw "无法删除旧 Observer 服务注册 service=$serviceName output=$($deleteOutput -join ' ')"
    }
    Start-Sleep -Milliseconds 500
}

$temporaryConfiguration = "$configurationPath.$([Guid]::NewGuid().ToString('N')).tmp"
[IO.File]::WriteAllText($temporaryConfiguration, ($configuration | ConvertTo-Json), [Text.UTF8Encoding]::new($false))
Move-Item -LiteralPath $temporaryConfiguration -Destination $configurationPath -Force

New-Service -Name $serviceName -BinaryPathName "`"$servicePath`"" -DisplayName 'AgentReins Observer' -Description 'Agent-scoped Windows ETW observation service' -StartupType Manual | Out-Null
$descriptionOutput = & sc.exe description $serviceName 'Agent-scoped Windows ETW observation service' 2>&1
if ($LASTEXITCODE -ne 0) {
    throw "无法设置 Observer 服务说明 service=$serviceName output=$($descriptionOutput -join ' ')"
}
$failureOutput = & sc.exe failure $serviceName reset= 86400 actions= restart/5000/restart/15000/none/0 2>&1
if ($LASTEXITCODE -ne 0) {
    throw "无法设置 Observer 服务恢复策略 service=$serviceName output=$($failureOutput -join ' ')"
}
$serviceSecurity = (& sc.exe sdshow $serviceName 2>&1 | Where-Object { $_ -like 'D:*' }) -join ''
$interactiveReadAce = '(A;;CCLCSWLOCRRC;;;IU)'
$interactiveStartAce = '(A;;CCLCSWRPLOCRRC;;;IU)'
if (-not $serviceSecurity.Contains($interactiveReadAce)) {
    throw "Observer 服务 ACL 不包含预期的交互用户只读 ACE，拒绝重写 service=$serviceName sddl=$serviceSecurity"
}
$updatedServiceSecurity = $serviceSecurity.Replace($interactiveReadAce, $interactiveStartAce)
$securityOutput = & sc.exe sdset $serviceName $updatedServiceSecurity 2>&1
if ($LASTEXITCODE -ne 0) {
    throw "无法授予交互用户 Observer 服务启动权 service=$serviceName output=$($securityOutput -join ' ')"
}

[pscustomobject]@{
    schema_version = '0.1.0'
    service_name = $serviceName
    service_path = $servicePath
    configuration_path = $configurationPath
    evidence_root = $evidence
    identity_path = $installedIdentity
    start_mode = 'demand'
    account = 'LocalSystem'
} | ConvertTo-Json -Compress
