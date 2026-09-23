[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$SourceRoot,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$ReleaseRoot,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$InstallRoot,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$CargoPath,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$NodePath,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$TauriCliPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Assert-SafeOutputRoot {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path,
        [Parameter(Mandatory = $true)]
        [string]$ExpectedLeaf
    )

    $fullPath = [IO.Path]::GetFullPath($Path).TrimEnd('\')
    if ([IO.Path]::GetFileName($fullPath) -ne $ExpectedLeaf) {
        throw "发布输出目录名称不符合约束 expected=$ExpectedLeaf actual=$([IO.Path]::GetFileName($fullPath)) path=$fullPath"
    }
    $fullPath
}

function Write-ReleaseManifest {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path,
        [Parameter(Mandatory = $true)]
        [string]$Version,
        [Parameter(Mandatory = $true)]
        [string[]]$Files
    )

    $manifest = [ordered]@{
        schema_version = '0.1.0'
        product = 'AgentReins Windows Observer'
        version = $Version
        built_at = [DateTimeOffset]::UtcNow.ToString('o')
        platform = 'windows-x86_64'
        observation_mode = 'observe_only'
        runtime_enforcement_applied = $false
        files = $Files
    }
    [IO.File]::WriteAllText($Path, ($manifest | ConvertTo-Json -Depth 4), [Text.UTF8Encoding]::new($false))
}

function Write-Checksums {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Root,
        [Parameter(Mandatory = $true)]
        [string]$OutputPath
    )

    $lines = @(Get-ChildItem -LiteralPath $Root -Recurse -File |
        Where-Object { $_.FullName -ne $OutputPath } |
        Sort-Object FullName |
        ForEach-Object {
            $relative = $_.FullName.Substring($Root.TrimEnd('\').Length + 1).Replace('\', '/')
            $hash = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
            "$hash  $relative"
        })
    [IO.File]::WriteAllLines($OutputPath, $lines, [Text.UTF8Encoding]::new($false))
}

$source = [IO.Path]::GetFullPath($SourceRoot).TrimEnd('\')
$release = Assert-SafeOutputRoot -Path $ReleaseRoot -ExpectedLeaf 'Release'
$install = Assert-SafeOutputRoot -Path $InstallRoot -ExpectedLeaf 'Install'
$cargo = [IO.Path]::GetFullPath($CargoPath)
$node = [IO.Path]::GetFullPath($NodePath)
$tauriCli = [IO.Path]::GetFullPath($TauriCliPath)
if (-not (Test-Path -LiteralPath (Join-Path $source 'Cargo.toml') -PathType Leaf)) {
    throw "源码根目录无效 path=$source"
}
foreach ($toolPath in @($cargo, $node, $tauriCli)) {
    if (-not (Test-Path -LiteralPath $toolPath -PathType Leaf)) {
        throw "发布构建工具不存在 path=$toolPath"
    }
}
foreach ($outputRoot in @($release, $install)) {
    if ($outputRoot.StartsWith("$source\", [StringComparison]::OrdinalIgnoreCase)) {
        throw "发布输出目录不得位于源码目录内 source=$source output=$outputRoot"
    }
}

$cargoManifest = Get-Content -LiteralPath (Join-Path $source 'Cargo.toml') -Raw -Encoding UTF8
$versionMatch = [regex]::Match($cargoManifest, '(?m)^version = "(?<version>\d+\.\d+\.\d+)"$')
if (-not $versionMatch.Success) {
    throw "无法从 Cargo.toml 读取平台版本 path=$(Join-Path $source 'Cargo.toml')"
}
$version = $versionMatch.Groups['version'].Value
$stageRoot = Join-Path ([IO.Path]::GetTempPath()) "agentreins-release-$([Guid]::NewGuid().ToString('N'))"
$releaseStage = Join-Path $stageRoot "AgentReins-$version"
$installStage = Join-Path $stageRoot 'Install'
$binaryStage = Join-Path $source 'crates\desktop-shell\binaries'
New-Item -ItemType Directory -Path $releaseStage, $installStage, $binaryStage | Out-Null

try {
    Push-Location $source
    try {
        & $cargo build --release -p agentreins-observer-service -p etw-collector -p workbuddy-adapter
        if ($LASTEXITCODE -ne 0) {
            throw "Observer 二进制 Release 构建失败 exit_code=$LASTEXITCODE"
        }
        foreach ($binaryName in @('agentreins-observer-service', 'etw-collector', 'workbuddy-semantic-collector')) {
            Copy-Item -LiteralPath (Join-Path $source "target\release\$binaryName.exe") -Destination (Join-Path $binaryStage "$binaryName-x86_64-pc-windows-msvc.exe") -Force
        }
        Push-Location (Join-Path $source 'apps\desktop-ui')
        try {
            & $node (Join-Path $source 'apps\desktop-ui\node_modules\typescript\bin\tsc') --noEmit
            if ($LASTEXITCODE -ne 0) {
                throw "桌面 GUI TypeScript 检查失败 exit_code=$LASTEXITCODE"
            }
            & $node (Join-Path $source 'apps\desktop-ui\node_modules\vite\bin\vite.js') build --base ./
            if ($LASTEXITCODE -ne 0) {
                throw "桌面 GUI 构建失败 exit_code=$LASTEXITCODE"
            }
        }
        finally {
            Pop-Location
        }
        Push-Location (Join-Path $source 'crates\desktop-shell')
        try {
            & $node $tauriCli build --config (Join-Path $source 'crates\desktop-shell\tauri.release.conf.json') --bundles msi
            if ($LASTEXITCODE -ne 0) {
                throw "AgentReins MSI 构建失败 exit_code=$LASTEXITCODE"
            }
        }
        finally {
            Pop-Location
        }
    }
    finally {
        Pop-Location
    }

    $releaseFiles = @(
        'agentreins-desktop.exe',
        'agentreins-observer-service.exe',
        'etw-collector.exe',
        'workbuddy-semantic-collector.exe'
    )
    foreach ($binaryName in $releaseFiles) {
        Copy-Item -LiteralPath (Join-Path $source "target\release\$binaryName") -Destination (Join-Path $releaseStage $binaryName) -Force
    }
    Copy-Item -LiteralPath (Join-Path $source 'apps\desktop-ui\dist') -Destination (Join-Path $releaseStage 'ui') -Recurse
    New-Item -ItemType Directory -Path (Join-Path $releaseStage 'scripts') | Out-Null
    Copy-Item -LiteralPath (Join-Path $source 'scripts\get-workbuddy-semantic-observer-status.ps1') -Destination (Join-Path $releaseStage 'scripts\get-workbuddy-semantic-observer-status.ps1') -Force
    Copy-Item -LiteralPath (Join-Path $source 'scripts\run-workbuddy-semantic-observer-host.ps1') -Destination (Join-Path $releaseStage 'scripts\run-workbuddy-semantic-observer-host.ps1') -Force
    Copy-Item -LiteralPath (Join-Path $source 'scripts\start-workbuddy-semantic-observer.ps1') -Destination (Join-Path $releaseStage 'scripts\start-workbuddy-semantic-observer.ps1') -Force
    Copy-Item -LiteralPath (Join-Path $source 'scripts\stop-workbuddy-semantic-observer.ps1') -Destination (Join-Path $releaseStage 'scripts\stop-workbuddy-semantic-observer.ps1') -Force
    Copy-Item -LiteralPath (Join-Path $source 'scripts\update-workbuddy-semantic-evidence.ps1') -Destination (Join-Path $releaseStage 'scripts\update-workbuddy-semantic-evidence.ps1') -Force
    Copy-Item -LiteralPath (Join-Path $source 'scripts\install-agentreins-observer-service.ps1') -Destination (Join-Path $releaseStage 'scripts\install-agentreins-observer-service.ps1') -Force

    $msi = Get-ChildItem -LiteralPath (Join-Path $source 'target\release\bundle\msi') -Filter '*.msi' -File |
        Sort-Object LastWriteTimeUtc |
        Select-Object -Last 1
    if ($null -eq $msi) {
        throw "Tauri 构建成功但未找到 MSI path=$(Join-Path $source 'target\release\bundle\msi')"
    }
    $installerName = "AgentReins_${version}_x64_en-US.msi"
    Copy-Item -LiteralPath $msi.FullName -Destination (Join-Path $installStage $installerName) -Force

    Write-ReleaseManifest -Path (Join-Path $releaseStage 'release-manifest.json') -Version $version -Files @($releaseFiles + 'ui/' + 'scripts/')
    Write-Checksums -Root $releaseStage -OutputPath (Join-Path $releaseStage 'SHA256SUMS.txt')
    Write-ReleaseManifest -Path (Join-Path $installStage 'release-manifest.json') -Version $version -Files @($installerName)
    Write-Checksums -Root $installStage -OutputPath (Join-Path $installStage 'SHA256SUMS.txt')

    foreach ($outputRoot in @($release, $install)) {
        if (Test-Path -LiteralPath $outputRoot) {
            Get-ChildItem -LiteralPath $outputRoot -Force | Remove-Item -Recurse -Force
        }
        else {
            New-Item -ItemType Directory -Path $outputRoot | Out-Null
        }
    }
    Move-Item -LiteralPath $releaseStage -Destination (Join-Path $release "AgentReins-$version")
    Get-ChildItem -LiteralPath $installStage -Force | Move-Item -Destination $install

    [pscustomobject]@{
        schema_version = '0.1.0'
        version = $version
        release_path = Join-Path $release "AgentReins-$version"
        installer_path = Join-Path $install $installerName
        historical_observer_evidence_deleted = $false
    } | ConvertTo-Json -Compress
}
finally {
    if (Test-Path -LiteralPath $binaryStage) {
        Remove-Item -LiteralPath $binaryStage -Recurse -Force
    }
    if (Test-Path -LiteralPath $stageRoot) {
        Remove-Item -LiteralPath $stageRoot -Recurse -Force
    }
}
