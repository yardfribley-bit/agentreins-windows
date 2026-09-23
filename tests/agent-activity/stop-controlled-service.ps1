[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$PidFile
)

$ErrorActionPreference = 'Stop'
$pidPath = [IO.Path]::GetFullPath($PidFile)
if (-not (Test-Path -LiteralPath $pidPath -PathType Leaf)) {
    throw "找不到受控服务 PID 文件 path=$pidPath"
}

$processId = [int](Get-Content -LiteralPath $pidPath -Raw)
$process = Get-CimInstance Win32_Process -Filter "ProcessId = $processId"
if ($null -eq $process) {
    throw "PID 文件对应进程不存在 pid=$processId path=$pidPath"
}
if ($process.Name -ne 'node.exe' -or $process.CommandLine -notlike '*controlled-service.mjs*') {
    throw "PID 对应进程不是受控服务 pid=$processId name=$($process.Name) command_line=$($process.CommandLine)"
}

Stop-Process -Id $processId -Force
Remove-Item -LiteralPath $pidPath -Force
[pscustomobject]@{
    stopped_pid = $processId
    pid_file_removed = -not (Test-Path -LiteralPath $pidPath)
} | ConvertTo-Json -Compress
