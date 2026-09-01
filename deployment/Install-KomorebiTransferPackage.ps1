<#
    komorebi 迁移包安装脚本（在「公司电脑」上运行）

    把随包携带的二进制、配置、脚本层、键位表和字体，还原成与来源机器一致的
    komorebi 环境，并按公司电脑实际连接的显示器（双屏）完成显示器登记与 bar
    映射。

    做的事情，按顺序：
      1. 检查前置条件：PowerShell 7、AutoHotkey v2
      2. 自我提权（写 %ProgramFiles% 和注册登录任务需要管理员）
      3. 校验包内每个文件的 SHA256（manifest.json）
      4. 停掉正在运行的 komorebi / bar / 热键进程
      5. 备份目标机器上的同名文件到 %USERPROFILE%\.komorebi-backups\transfer-<时间戳>
      6. 安装二进制到 %ProgramFiles%\komorebi\bin，并把该目录加入机器 PATH
      7. 安装配置与脚本层到 %USERPROFILE%
      8. 安装字体（用户级，不需要重启）
      9. 注册登录自启动任务（以最高权限运行 komorebi-start.ps1）
     10. 冷启动 komorebi，登记本机显示器，把 bar 配置对齐到登记后的显示器序号，
         再冷启动一次让每块屏的 bar 按新序号启动
     11. 写安装报告 %USERPROFILE%\.komorebi-transfer-report.json

    用法（在解压出来的包目录里）：
        pwsh -NoProfile -ExecutionPolicy Bypass -File .\Install-Komorebi.ps1

    可用开关：
        -SkipFonts          不安装字体
        -SkipAutostart      不注册登录自启动任务
        -SkipStart          只安装文件，不启动 komorebi（同时跳过显示器登记）
        -SkipMonitorSetup   启动但不改显示器登记与 bar 映射
        -Force              前置条件缺失时仍然继续安装文件
        -DryRun             只打印将要做什么，不改动任何东西
#>

[CmdletBinding()]
param(
    [string] $PackageRoot = $PSScriptRoot,
    [string] $BinDir = (Join-Path $env:ProgramFiles 'komorebi\bin'),
    [string] $ConfigHome = $env:USERPROFILE,
    [switch] $SkipFonts,
    [switch] $SkipAutostart,
    [switch] $SkipStart,
    [switch] $SkipMonitorSetup,
    [switch] $Force,
    [switch] $DryRun
)

$ErrorActionPreference = 'Stop'
$Utf8NoBom = New-Object System.Text.UTF8Encoding($false)
try { [Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false) } catch { }
$OutputEncoding = $Utf8NoBom

$Script:Timestamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$Script:BackupDir = Join-Path $ConfigHome ".komorebi-backups\transfer-$Script:Timestamp"
$Script:LogLines = New-Object System.Collections.Generic.List[string]
$Script:Warnings = New-Object System.Collections.Generic.List[string]

function Write-Step {
    param([string] $Message)
    Write-Host "==> $Message" -ForegroundColor Cyan
    $Script:LogLines.Add("STEP $Message")
}

function Write-Note {
    param([string] $Message)
    Write-Host "    $Message"
    $Script:LogLines.Add("     $Message")
}

function Write-Caution {
    param([string] $Message)
    Write-Host "  ! $Message" -ForegroundColor Yellow
    $Script:LogLines.Add("WARN $Message")
    $Script:Warnings.Add($Message)
}

# 提示，不是告警：不计入结尾的告警清单。
function Write-Hint {
    param([string] $Message)
    Write-Host "  · $Message" -ForegroundColor Yellow
    $Script:LogLines.Add("HINT $Message")
}

function Test-Elevated {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    (New-Object Security.Principal.WindowsPrincipal($identity)).IsInRole(
        [Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Get-Pwsh7 {
    $candidate = Join-Path $env:ProgramFiles 'PowerShell\7\pwsh.exe'
    if (Test-Path -LiteralPath $candidate) { return $candidate }
    $command = Get-Command pwsh.exe -ErrorAction SilentlyContinue
    if ($command) { return $command.Source }
    return $null
}

function Get-AutoHotkey {
    $candidates = @(Join-Path $env:LOCALAPPDATA 'Programs\AutoHotkey\v2\AutoHotkey64.exe')
    if ($env:ProgramFiles) { $candidates += Join-Path $env:ProgramFiles 'AutoHotkey\v2\AutoHotkey64.exe' }
    if (${env:ProgramFiles(x86)}) {
        $candidates += Join-Path ${env:ProgramFiles(x86)} 'AutoHotkey\v2\AutoHotkey64.exe'
    }
    foreach ($candidate in $candidates) {
        if (Test-Path -LiteralPath $candidate) { return $candidate }
    }
    $command = Get-Command AutoHotkey64.exe -ErrorAction SilentlyContinue
    if ($command) { return $command.Source }
    return $null
}

# ---------------------------------------------------------------------------
# 0. 包结构
# ---------------------------------------------------------------------------

$PackageBin = Join-Path $PackageRoot 'bin'
$PackageHome = Join-Path $PackageRoot 'home'
$PackageFont = Join-Path $PackageRoot 'fonts'
$ManifestPath = Join-Path $PackageRoot 'manifest.json'

if (-not (Test-Path -LiteralPath $PackageBin) -or -not (Test-Path -LiteralPath $PackageHome)) {
    throw "这里不像是一个 komorebi 迁移包（缺少 bin\ 或 home\）：$PackageRoot"
}

Write-Host ''
Write-Host 'komorebi 迁移包安装' -ForegroundColor Green
Write-Note "包目录：$PackageRoot"
if ($DryRun) { Write-Hint '-DryRun：只打印计划，不做任何改动' }

# ---------------------------------------------------------------------------
# 1. 前置条件
# ---------------------------------------------------------------------------

Write-Step '检查前置条件'

$Pwsh = Get-Pwsh7
$AutoHotkey = Get-AutoHotkey
$blocking = @()

if ($Pwsh) { Write-Note "PowerShell 7：$Pwsh" }
else {
    $blocking += 'PowerShell 7 未安装：winget install --id Microsoft.PowerShell'
    Write-Caution 'PowerShell 7 未安装（脚本层的启动、看门狗、热键包装都需要 pwsh.exe）'
}

if ($AutoHotkey) { Write-Note "AutoHotkey v2：$AutoHotkey" }
else {
    $blocking += 'AutoHotkey v2 未安装：winget install --id AutoHotkey.AutoHotkey'
    Write-Caution 'AutoHotkey v2 未安装（所有快捷键都由它直连 komorebic）'
}

if ($blocking.Count -gt 0 -and -not $Force -and -not $DryRun) {
    Write-Host ''
    Write-Host '缺少前置条件，安装中止。请先执行：' -ForegroundColor Red
    foreach ($item in $blocking) { Write-Host "  $item" }
    Write-Host ''
    Write-Host '装好之后重新运行本脚本；或者加 -Force 先把文件装上，稍后再补依赖。'
    exit 2
}

# ---------------------------------------------------------------------------
# 2. 提权
# ---------------------------------------------------------------------------

if (-not (Test-Elevated)) {
    if ($DryRun) {
        Write-Hint '未提权：真正安装时会请求一次管理员权限（写 Program Files 与注册登录任务）'
    }
    else {
        Write-Step '请求管理员权限'
        $host7 = if ($Pwsh) { $Pwsh } else { (Get-Process -Id $PID).Path }
        # 路径可能带空格：Windows PowerShell 5.1 不会替数组元素补引号，这里显式加。
        $arguments = @('-NoProfile', '-ExecutionPolicy', 'Bypass',
            '-File', ('"{0}"' -f $PSCommandPath),
            '-PackageRoot', ('"{0}"' -f $PackageRoot))
        if ($SkipFonts) { $arguments += '-SkipFonts' }
        if ($SkipAutostart) { $arguments += '-SkipAutostart' }
        if ($SkipStart) { $arguments += '-SkipStart' }
        if ($SkipMonitorSetup) { $arguments += '-SkipMonitorSetup' }
        if ($Force) { $arguments += '-Force' }
        Start-Process $host7 -Verb RunAs -ArgumentList $arguments
        Write-Note '已在提权窗口继续安装；本窗口可以关闭。'
        exit 0
    }
}

# ---------------------------------------------------------------------------
# 3. 校验包完整性
# ---------------------------------------------------------------------------

$Manifest = $null
if (Test-Path -LiteralPath $ManifestPath) {
    Write-Step '校验包内文件的 SHA256'
    $Manifest = Get-Content -LiteralPath $ManifestPath -Raw -Encoding UTF8 | ConvertFrom-Json
    $bad = @()
    foreach ($entry in $Manifest.files) {
        $path = Join-Path $PackageRoot ($entry.path -replace '/', '\')
        if (-not (Test-Path -LiteralPath $path)) { $bad += "缺失 $($entry.path)"; continue }
        $hash = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash
        if ($hash -ne $entry.sha256) { $bad += "哈希不符 $($entry.path)" }
    }
    if ($bad.Count -gt 0) {
        Write-Host '包内容与 manifest 不符：' -ForegroundColor Red
        foreach ($item in $bad) { Write-Host "  $item" }
        throw '压缩包可能在传输中损坏，请重新复制后再安装'
    }
    Write-Note "$($Manifest.files.Count) 个文件校验通过"
    Write-Note "来源：$($Manifest.source_machine) / $($Manifest.repo_branch)@$($Manifest.repo_commit)"
    Write-Note "版本：$($Manifest.komorebi_version)"
}
else {
    Write-Caution '包内没有 manifest.json，跳过完整性校验'
}

# ---------------------------------------------------------------------------
# 4. 停掉正在运行的实例
# ---------------------------------------------------------------------------

Write-Step '停掉正在运行的 komorebi'

if (-not $DryRun) {
    $installedKomorebic = Join-Path $BinDir 'komorebic.exe'
    if (Test-Path -LiteralPath $installedKomorebic) {
        try { & $installedKomorebic stop --bar 2>&1 | Out-Null } catch { }
        Start-Sleep -Milliseconds 500
    }

    foreach ($name in @('komorebi', 'komorebi-bar', 'komorebi-gui', 'komorebi-shortcuts')) {
        Get-Process -Name $name -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    }

    # 只结束跑着本套热键脚本的 AutoHotkey 进程，不碰用户其它 AHK 脚本。
    Get-CimInstance Win32_Process -Filter "Name='AutoHotkey64.exe'" -ErrorAction SilentlyContinue |
        Where-Object { [string]$_.CommandLine -like '*komorebi-hotkeys.ahk*' } |
        ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }

    # 看门狗哨兵：留着会让新的看门狗以为上一轮还在跑。
    $sentinel = Join-Path $env:LOCALAPPDATA 'komorebi\watchdog.on'
    if (Test-Path -LiteralPath $sentinel) { Remove-Item -LiteralPath $sentinel -Force -ErrorAction SilentlyContinue }
    Write-Note '已停止（若本来没运行则无事发生）'
}

# ---------------------------------------------------------------------------
# 5. 备份目标机器上的同名文件
# ---------------------------------------------------------------------------

Write-Step "备份现有文件到 $Script:BackupDir"

function Backup-File {
    param([string] $Path, [string] $Category)
    if (-not (Test-Path -LiteralPath $Path)) { return }
    if ($DryRun) { Write-Note "将备份 $Path"; return }
    $target = Join-Path $Script:BackupDir $Category
    if (-not (Test-Path -LiteralPath $target)) { $null = New-Item -ItemType Directory -Path $target -Force }
    Copy-Item -LiteralPath $Path -Destination (Join-Path $target (Split-Path -Leaf $Path)) -Force
}

$binaryFiles = @(Get-ChildItem -LiteralPath $PackageBin -File)
$homeFiles = @(Get-ChildItem -LiteralPath $PackageHome -File)

foreach ($file in $binaryFiles) { Backup-File -Path (Join-Path $BinDir $file.Name) -Category 'bin' }
foreach ($file in $homeFiles) { Backup-File -Path (Join-Path $ConfigHome $file.Name) -Category 'home' }

if (-not $DryRun) {
    if (Test-Path -LiteralPath $Script:BackupDir) {
        $count = @(Get-ChildItem -LiteralPath $Script:BackupDir -Recurse -File).Count
        Write-Note "$count 个文件已备份"
    }
    else { Write-Note '目标机器上没有需要备份的同名文件' }
}

# ---------------------------------------------------------------------------
# 6. 安装二进制并修好 PATH
# ---------------------------------------------------------------------------

Write-Step "安装二进制到 $BinDir"

if (-not $DryRun -and -not (Test-Path -LiteralPath $BinDir)) {
    $null = New-Item -ItemType Directory -Path $BinDir -Force
}

foreach ($file in $binaryFiles) {
    if ($DryRun) { Write-Note "将复制 $($file.Name)"; continue }
    Copy-Item -LiteralPath $file.FullName -Destination (Join-Path $BinDir $file.Name) -Force
    Write-Note $file.Name
}

Write-Step '把 komorebi\bin 加入机器 PATH'
$machinePath = [Environment]::GetEnvironmentVariable('Path', 'Machine')
if ($machinePath -split ';' -contains $BinDir) {
    Write-Note 'PATH 里已经有了'
}
elseif ($DryRun) { Write-Note "将把 $BinDir 追加到机器 PATH" }
else {
    [Environment]::SetEnvironmentVariable('Path', ($machinePath.TrimEnd(';') + ";$BinDir"), 'Machine')
    Write-Note "已追加（新开的终端才会看到）"
}
$env:Path = "$BinDir;$env:Path"

# ---------------------------------------------------------------------------
# 7. 安装配置与脚本层
# ---------------------------------------------------------------------------

Write-Step "安装配置与脚本层到 $ConfigHome"
foreach ($file in $homeFiles) {
    if ($DryRun) { Write-Note "将复制 $($file.Name)"; continue }
    Copy-Item -LiteralPath $file.FullName -Destination (Join-Path $ConfigHome $file.Name) -Force
    Write-Note $file.Name
}

# ---------------------------------------------------------------------------
# 8. 字体
# ---------------------------------------------------------------------------

if (-not $SkipFonts -and (Test-Path -LiteralPath $PackageFont)) {
    Write-Step '安装 bar 使用的字体（用户级）'
    $userFontDir = Join-Path $env:LOCALAPPDATA 'Microsoft\Windows\Fonts'
    $fontKey = 'HKCU:\Software\Microsoft\Windows NT\CurrentVersion\Fonts'

    if (-not $DryRun) {
        if (-not (Test-Path -LiteralPath $userFontDir)) { $null = New-Item -ItemType Directory -Path $userFontDir -Force }
        if (-not (Test-Path -LiteralPath $fontKey)) { $null = New-Item -Path $fontKey -Force }
    }

    Add-Type -Name FontApi -Namespace Komorebi -MemberDefinition @'
[System.Runtime.InteropServices.DllImport("gdi32.dll", CharSet = System.Runtime.InteropServices.CharSet.Unicode)]
public static extern int AddFontResourceW(string path);
'@ -ErrorAction SilentlyContinue

    foreach ($font in @(Get-ChildItem -LiteralPath $PackageFont -Filter '*.ttf' -File)) {
        $target = Join-Path $userFontDir $font.Name
        $valueName = "$([IO.Path]::GetFileNameWithoutExtension($font.Name)) (TrueType)"
        if ($DryRun) { Write-Note "将安装字体 $($font.Name)"; continue }
        Copy-Item -LiteralPath $font.FullName -Destination $target -Force
        New-ItemProperty -Path $fontKey -Name $valueName -Value $target -PropertyType String -Force | Out-Null
        try { [Komorebi.FontApi]::AddFontResourceW($target) | Out-Null } catch { }
        Write-Note $font.Name
    }
}
elseif ($SkipFonts) { Write-Step '按 -SkipFonts 跳过字体' }

# ---------------------------------------------------------------------------
# 9. 登录自启动任务
# ---------------------------------------------------------------------------

if (-not $SkipAutostart) {
    Write-Step '注册登录自启动任务（以最高权限运行 komorebi-start.ps1）'
    $elevateSetup = Join-Path $ConfigHome 'komorebi-elevate-setup.ps1'
    if (-not (Test-Path -LiteralPath $elevateSetup)) {
        Write-Caution "找不到 $elevateSetup，跳过自启动注册"
    }
    elseif ($DryRun) { Write-Note '将运行 komorebi-elevate-setup.ps1 -NoRestart' }
    elseif (-not $Pwsh) { Write-Caution '没有 PowerShell 7，跳过自启动注册' }
    else {
        & $Pwsh -NoProfile -ExecutionPolicy Bypass -File $elevateSetup -NoRestart
        $task = Get-ScheduledTask -TaskName 'komorebi' -ErrorAction SilentlyContinue
        if ($task) { Write-Note "计划任务 komorebi：$($task.State)" }
        else { Write-Caution '计划任务没有注册成功，请查看 komorebi-elevate-setup.log' }
    }
}
else { Write-Step '按 -SkipAutostart 跳过自启动注册' }

# ---------------------------------------------------------------------------
# 10. 启动，并按本机显示器完成登记与 bar 映射
# ---------------------------------------------------------------------------

$Komorebic = Join-Path $BinDir 'komorebic.exe'
$StartScript = Join-Path $ConfigHome 'komorebi-start.ps1'
$PinScript = Join-Path $ConfigHome 'komorebi-pin-displays.ps1'
$ConfigPath = Join-Path $ConfigHome 'komorebi.json'

function Start-KomorebiCold {
    if (-not $Pwsh) { Write-Caution '没有 PowerShell 7，无法调用 komorebi-start.ps1'; return $false }
    if (-not (Test-Path -LiteralPath $StartScript)) { Write-Caution "找不到 $StartScript"; return $false }
    & $Pwsh -NoProfile -ExecutionPolicy Bypass -File $StartScript | Out-Null
    return (Wait-KomorebiSocket -Seconds 30)
}

function Wait-KomorebiSocket {
    param([int] $Seconds = 30)
    $deadline = (Get-Date).AddSeconds($Seconds)
    while ((Get-Date) -lt $deadline) {
        $raw = & $Komorebic state 2>&1 | Out-String
        if ($LASTEXITCODE -eq 0 -and $raw.Trim()) { return $true }
        Start-Sleep -Milliseconds 500
    }
    return $false
}

function Get-BarMonitorIndex {
    param([string] $Path)
    try {
        $bar = Get-Content -LiteralPath $Path -Raw -Encoding UTF8 | ConvertFrom-Json
    }
    catch { return $null }
    if ($null -eq $bar.monitor) { return $null }
    if ($bar.monitor -is [int]) { return [int]$bar.monitor }
    if ($null -ne $bar.monitor.index) { return [int]$bar.monitor.index }
    return $null
}

function Resolve-HomePath {
    param([string] $Value)
    ([string]$Value).Replace('$Env:USERPROFILE', $ConfigHome).Replace('$env:USERPROFILE', $ConfigHome).Replace('/', '\')
}

<#
    把 bar_configurations 对齐到「本机实际登记到的显示器序号」。

    komorebi 按 display_index_preferences 把物理屏幕钉到用户序号上；没有登记的
    序号会被跳过。本机的两块屏幕如果登记成 1 和 2，那么 bar 配置就必须是
    monitor.index = 1 和 2 的那两个 —— 否则序号 0 的那份 bar 会退回到「第一块
    物理屏幕」，在同一块屏上叠出第二条 bar。
#>
function Sync-BarConfiguration {
    $raw = & $Komorebic monitor-information 2>&1 | Out-String
    if ($LASTEXITCODE -ne 0 -or -not $raw.Trim()) {
        Write-Caution '读不到显示器信息，跳过 bar 映射对齐'
        return
    }
    $monitors = @($raw | ConvertFrom-Json)
    $config = Get-Content -LiteralPath $ConfigPath -Raw -Encoding UTF8 | ConvertFrom-Json

    $preferences = @{}
    if ($config.display_index_preferences) {
        foreach ($property in $config.display_index_preferences.PSObject.Properties) {
            $preferences[[string]$property.Value] = [int]$property.Name
        }
    }

    # 当前连接的每块屏幕落在哪个用户序号上。
    $connected = @()
    foreach ($monitor in $monitors) {
        $index = $null
        foreach ($id in @([string]$monitor.serial_number_id, [string]$monitor.device_id)) {
            if ($id -and $preferences.ContainsKey($id)) { $index = $preferences[$id]; break }
        }
        if ($null -ne $index) { $connected += $index }
    }
    $connected = @($connected | Sort-Object -Unique)

    if ($connected.Count -ne $monitors.Count) {
        Write-Caution "只有 $($connected.Count)/$($monitors.Count) 块屏幕完成登记，跳过 bar 映射对齐"
        return
    }

    # 现有的 bar 配置：按它自己声明的 monitor.index 归位。
    $byIndex = @{}
    $template = $null
    foreach ($entry in @($config.bar_configurations)) {
        $path = Resolve-HomePath $entry
        if (-not (Test-Path -LiteralPath $path)) { continue }
        $index = Get-BarMonitorIndex -Path $path
        if ($null -eq $index) { continue }
        if (-not $byIndex.ContainsKey($index)) { $byIndex[$index] = [string]$entry }
        if ($null -eq $template) { $template = $path }
    }
    if ($null -eq $template) {
        Write-Caution '没有可用的 bar 配置文件，跳过 bar 映射对齐'
        return
    }

    $desired = @()
    foreach ($index in $connected) {
        if ($byIndex.ContainsKey($index)) { $desired += $byIndex[$index]; continue }

        # 该序号还没有对应的 bar 配置：以第一份为模板生成一份，只改 monitor.index。
        $leaf = if ($index -eq 0) { 'komorebi.bar.json' } else { "komorebi.bar.$index.json" }
        $newPath = Join-Path $ConfigHome $leaf
        $bar = Get-Content -LiteralPath $template -Raw -Encoding UTF8 | ConvertFrom-Json
        $bar.monitor = [pscustomobject]@{ index = $index }
        [IO.File]::WriteAllText($newPath, ($bar | ConvertTo-Json -Depth 32), $Utf8NoBom)
        $desired += "`$Env:USERPROFILE/$leaf"
        Write-Note "为显示器序号 $index 生成 $leaf"
    }

    $current = @($config.bar_configurations | ForEach-Object { [string]$_ })
    if (($current -join '|') -eq ($desired -join '|')) {
        Write-Note "bar 映射已经对齐（显示器序号：$($connected -join ', ')）"
        return
    }

    Copy-Item -LiteralPath $ConfigPath -Destination "$ConfigPath.bak-$Script:Timestamp" -Force
    $config.bar_configurations = $desired
    [IO.File]::WriteAllText($ConfigPath, ($config | ConvertTo-Json -Depth 32), $Utf8NoBom)
    Write-Note "bar_configurations -> $($desired -join ', ')"
    & $Komorebic replace-configuration $ConfigPath 2>&1 | Out-Null
}

$started = $false
$monitorCount = 0

if ($SkipStart) { Write-Step '按 -SkipStart 跳过启动' }
elseif ($DryRun) { Write-Step '将冷启动 komorebi，并登记本机显示器' }
else {
    Write-Step '冷启动 komorebi'
    $started = Start-KomorebiCold
    if (-not $started) { Write-Caution 'komorebi 没有在 30 秒内应答；请查看 %LOCALAPPDATA%\komorebi\start.log' }
    else { Write-Note 'komorebi 已就绪' }

    if ($started -and -not $SkipMonitorSetup) {
        Write-Step '登记本机显示器（双屏在这一步获得各自的用户序号）'
        if (-not (Test-Path -LiteralPath $PinScript)) { Write-Caution "找不到 $PinScript" }
        else {
            & $Pwsh -NoProfile -ExecutionPolicy Bypass -File $PinScript
        }

        Write-Step '把 bar 配置对齐到登记后的显示器序号'
        Sync-BarConfiguration

        Write-Step '再冷启动一次，让每块屏的 bar 按新序号启动'
        $started = Start-KomorebiCold
        if (-not $started) { Write-Caution '第二次冷启动没有应答' }
    }
    elseif ($started -and $SkipMonitorSetup) { Write-Step '按 -SkipMonitorSetup 跳过显示器登记' }

    if ($started) {
        $raw = & $Komorebic monitor-information 2>&1 | Out-String
        if ($LASTEXITCODE -eq 0 -and $raw.Trim()) { $monitorCount = @($raw | ConvertFrom-Json).Count }
    }
}

# ---------------------------------------------------------------------------
# 11. 报告
# ---------------------------------------------------------------------------

if (-not $DryRun) {
    $report = [ordered]@{
        installed_at = (Get-Date).ToString('o')
        package_root = $PackageRoot
        source_machine = if ($Manifest) { $Manifest.source_machine } else { '' }
        source_commit = if ($Manifest) { $Manifest.repo_commit } else { '' }
        komorebi_version = if ($Manifest) { $Manifest.komorebi_version } else { '' }
        bin_dir = $BinDir
        config_home = $ConfigHome
        backup_dir = $Script:BackupDir
        autostart_registered = (-not $SkipAutostart)
        fonts_installed = (-not $SkipFonts)
        started = $started
        monitor_count = $monitorCount
        warnings = @($Script:Warnings)
    }
    $reportPath = Join-Path $ConfigHome '.komorebi-transfer-report.json'
    [IO.File]::WriteAllText($reportPath, ($report | ConvertTo-Json -Depth 6), $Utf8NoBom)
}

Write-Host ''
if ($Script:Warnings.Count -gt 0) {
    Write-Host "安装结束，但有 $($Script:Warnings.Count) 条告警：" -ForegroundColor Yellow
    foreach ($warning in $Script:Warnings) { Write-Host "  ! $warning" -ForegroundColor Yellow }
}
else {
    Write-Host '安装完成。' -ForegroundColor Green
}

Write-Host ''
Write-Host '接下来：'
Write-Host '  - Alt+Ctrl+S 启动 / Alt+Ctrl+Q 停止 / Alt+Shift+R 重启'
Write-Host '  - Alt+I 打开中文快捷键面板'
Write-Host '  - 显示器接线或分辨率变化之后，重跑一次：'
Write-Host '      pwsh -NoProfile -File "$env:USERPROFILE\komorebi-pin-displays.ps1"'
Write-Host "  - 旧文件备份在：$Script:BackupDir"
if ($monitorCount -gt 0) { Write-Host "  - 当前识别到 $monitorCount 块显示器" }
Write-Host ''
