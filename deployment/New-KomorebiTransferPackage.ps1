<#
    komorebi 迁移包打包脚本（在「当前这台电脑」上运行）

    把这台机器上跑着的整套 komorebi 收进一个自包含目录，并压缩成一个 zip：

      - %ProgramFiles%\komorebi\bin 下的全部可执行文件（komorebi / komorebic /
        komorebi-bar / komorebi-gui / komorebi-shortcuts / komorebic-no-console）
      - %USERPROFILE% 下的全部活动配置与 PowerShell 脚本层（不含 *.bak*）
      - 本仓库 docs/common-workflows/komorebi-model.ahk 完整键位表
      - bar 使用的 Maple Mono NF CN 字体（可用 -NoFonts 关掉）
      - 目标机器上运行的安装脚本 Install-Komorebi.ps1 与中文说明 README.md
      - manifest.json：版本、提交号、每个文件的 SHA256

    用法：
        pwsh -NoProfile -File deployment\New-KomorebiTransferPackage.ps1
        pwsh -NoProfile -File deployment\New-KomorebiTransferPackage.ps1 -NoFonts

    产物：
        %USERPROFILE%\komorebi-transfer\komorebi-package\        打包目录
        %USERPROFILE%\komorebi-transfer\komorebi-package-<时间戳>.zip

    把那个 zip 发到公司电脑，解压后在里面运行 Install-Komorebi.ps1 即可。
#>

[CmdletBinding()]
param(
    # 打包输出的根目录。
    [string] $OutputRoot = (Join-Path $env:USERPROFILE 'komorebi-transfer'),
    # 本仓库根目录（默认取本脚本所在目录的上一级）。
    [string] $RepoRoot = (Split-Path -Parent $PSScriptRoot),
    # 已安装的二进制目录。
    [string] $BinDir = (Join-Path $env:ProgramFiles 'komorebi\bin'),
    # 用户配置所在目录。
    [string] $ConfigHome = $env:USERPROFILE,
    # 不打包字体（包体积从约 120 MB 降到约 80 MB）。
    [switch] $NoFonts,
    # 只生成目录，不压缩。
    [switch] $NoZip
)

$ErrorActionPreference = 'Stop'
$Utf8NoBom = New-Object System.Text.UTF8Encoding($false)
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)

function Write-Step { param([string] $Message) Write-Host "==> $Message" -ForegroundColor Cyan }
function Write-Note { param([string] $Message) Write-Host "    $Message" }
function Write-Caution { param([string] $Message) Write-Host "  ! $Message" -ForegroundColor Yellow }

# ---------------------------------------------------------------------------
# 1. 校验来源
# ---------------------------------------------------------------------------

Write-Step '校验来源目录'

if (-not (Test-Path -LiteralPath $BinDir)) { throw "找不到已安装的二进制目录：$BinDir" }
if (-not (Test-Path -LiteralPath $RepoRoot)) { throw "找不到仓库目录：$RepoRoot" }

$ModelAhkSource = Join-Path $RepoRoot 'docs\common-workflows\komorebi-model.ahk'
if (-not (Test-Path -LiteralPath $ModelAhkSource)) { throw "找不到键位表：$ModelAhkSource" }

$InstallerSource = Join-Path $PSScriptRoot 'Install-KomorebiTransferPackage.ps1'
$ReadmeSource = Join-Path $PSScriptRoot 'README.md'
foreach ($required in @($InstallerSource, $ReadmeSource)) {
    if (-not (Test-Path -LiteralPath $required)) { throw "找不到打包所需文件：$required" }
}

# 必须成组替换的四个程序，加上热键面板与无控制台客户端。
$BinaryNames = @(
    'komorebi.exe',
    'komorebic.exe',
    'komorebi-bar.exe',
    'komorebi-gui.exe',
    'komorebi-shortcuts.exe',
    'komorebic-no-console.exe'
)

foreach ($binary in $BinaryNames) {
    $path = Join-Path $BinDir $binary
    if (-not (Test-Path -LiteralPath $path)) { throw "找不到二进制：$path" }
}

# %USERPROFILE% 下必须随包走的配置文件。
$HomeConfigNames = @(
    'komorebi.json',
    'applications.json',
    'komorebi.bar.json',
    'komorebi.bar.1.json',
    'komorebi.bar.2.json',
    'komorebi-hotkeys.ahk'
)

foreach ($name in $HomeConfigNames) {
    $path = Join-Path $ConfigHome $name
    if (-not (Test-Path -LiteralPath $path)) { throw "找不到配置文件：$path" }
}

# PowerShell 脚本层：取 komorebi-*.ps1 的活动版本，排除全部备份。
$HomeScripts = @(
    Get-ChildItem -LiteralPath $ConfigHome -Filter 'komorebi-*.ps1' -File |
        Where-Object { $_.Name -notmatch '\.bak' } |
        Sort-Object Name
)
if ($HomeScripts.Count -lt 1) { throw "在 $ConfigHome 下没有找到任何 komorebi-*.ps1" }

# ---------------------------------------------------------------------------
# 2. 建立干净的打包目录
# ---------------------------------------------------------------------------

$PackageName = 'komorebi-package'
$PackageDir = Join-Path $OutputRoot $PackageName

Write-Step "准备打包目录 $PackageDir"

if (Test-Path -LiteralPath $PackageDir) {
    # 只允许删除自己上一次生成的目录：必须带有本脚本写下的标记文件。
    $marker = Join-Path $PackageDir '.komorebi-package'
    if (-not (Test-Path -LiteralPath $marker)) {
        throw "$PackageDir 已存在且不是本脚本生成的包目录；请先手动移走"
    }
    Remove-Item -LiteralPath $PackageDir -Recurse -Force
}

$null = New-Item -ItemType Directory -Path $PackageDir -Force
$null = New-Item -ItemType File -Path (Join-Path $PackageDir '.komorebi-package') -Force

$PackageBin = Join-Path $PackageDir 'bin'
$PackageHome = Join-Path $PackageDir 'home'
$PackageFont = Join-Path $PackageDir 'fonts'
foreach ($dir in @($PackageBin, $PackageHome)) { $null = New-Item -ItemType Directory -Path $dir -Force }

# ---------------------------------------------------------------------------
# 3. 复制二进制
# ---------------------------------------------------------------------------

Write-Step '复制已安装的二进制'
foreach ($binary in $BinaryNames) {
    Copy-Item -LiteralPath (Join-Path $BinDir $binary) -Destination (Join-Path $PackageBin $binary) -Force
    Write-Note $binary
}

# ---------------------------------------------------------------------------
# 4. 复制用户配置与脚本层
# ---------------------------------------------------------------------------

Write-Step '复制 %USERPROFILE% 下的配置与脚本'
foreach ($name in $HomeConfigNames) {
    Copy-Item -LiteralPath (Join-Path $ConfigHome $name) -Destination (Join-Path $PackageHome $name) -Force
    Write-Note $name
}
foreach ($script in $HomeScripts) {
    Copy-Item -LiteralPath $script.FullName -Destination (Join-Path $PackageHome $script.Name) -Force
    Write-Note $script.Name
}

# 键位表来自仓库，安装后与 komorebi-hotkeys.ahk 并排放在 %USERPROFILE%。
Copy-Item -LiteralPath $ModelAhkSource -Destination (Join-Path $PackageHome 'komorebi-model.ahk') -Force
Write-Note 'komorebi-model.ahk（来自仓库 docs/common-workflows）'

# 本机的 komorebi-hotkeys.ahk 用绝对路径 #Include 仓库里的键位表。公司电脑上
# 不会有这个仓库，所以包内副本改成相对脚本自身的位置；安装后两个文件同在
# %USERPROFILE%，不依赖任何仓库检出。
$HotkeyPath = Join-Path $PackageHome 'komorebi-hotkeys.ahk'
$hotkeyText = [IO.File]::ReadAllText($HotkeyPath)
$patchedText = [Text.RegularExpressions.Regex]::Replace(
    $hotkeyText,
    '(?m)^#Include\s+.*komorebi-model\.ahk[ \t]*$',
    '#Include %A_ScriptDir%\komorebi-model.ahk'
)
if ($patchedText -eq $hotkeyText) {
    throw '没有在 komorebi-hotkeys.ahk 里找到 komorebi-model.ahk 的 #Include 行；请检查该文件'
}
[IO.File]::WriteAllText($HotkeyPath, $patchedText, $Utf8NoBom)
Write-Note '已把 komorebi-hotkeys.ahk 的 #Include 改为 %A_ScriptDir%'

# ---------------------------------------------------------------------------
# 5. 字体
# ---------------------------------------------------------------------------

$FontFiles = @()
if (-not $NoFonts) {
    Write-Step '复制 bar 使用的字体'
    $userFontDir = Join-Path $env:LOCALAPPDATA 'Microsoft\Windows\Fonts'
    $wanted = @('MapleMono-NF-CN-Regular.ttf', 'MapleMono-NF-CN-Bold.ttf')
    $found = @()
    foreach ($font in $wanted) {
        $source = Join-Path $userFontDir $font
        if (-not (Test-Path -LiteralPath $source)) {
            $source = Join-Path $env:WINDIR "Fonts\$font"
        }
        if (Test-Path -LiteralPath $source) { $found += $source }
        else { Write-Caution "找不到字体 $font，将不打包" }
    }
    if ($found.Count -gt 0) {
        $null = New-Item -ItemType Directory -Path $PackageFont -Force
        foreach ($source in $found) {
            Copy-Item -LiteralPath $source -Destination (Join-Path $PackageFont (Split-Path -Leaf $source)) -Force
            Write-Note (Split-Path -Leaf $source)
        }
        $FontFiles = @($found | ForEach-Object { Split-Path -Leaf $_ })
    }
}
else {
    Write-Step '按 -NoFonts 跳过字体'
}

# ---------------------------------------------------------------------------
# 6. 安装脚本与说明
# ---------------------------------------------------------------------------

Write-Step '复制安装脚本与说明'
Copy-Item -LiteralPath $InstallerSource -Destination (Join-Path $PackageDir 'Install-Komorebi.ps1') -Force
Copy-Item -LiteralPath $ReadmeSource -Destination (Join-Path $PackageDir 'README.md') -Force
Write-Note 'Install-Komorebi.ps1'
Write-Note 'README.md'

# ---------------------------------------------------------------------------
# 7. manifest：版本、提交号、逐文件 SHA256
# ---------------------------------------------------------------------------

Write-Step '生成 manifest.json'

function Get-ToolVersion {
    param([string] $Exe)
    try {
        $raw = & $Exe --version 2>&1 | Out-String
        return (($raw -split '[\r\n]+' | Where-Object { $_.Trim() }) -join ' | ')
    }
    catch { return '' }
}

$commit = ''
$branch = ''
Push-Location $RepoRoot
try {
    $commit = (& git rev-parse HEAD 2>$null | Out-String).Trim()
    $branch = (& git rev-parse --abbrev-ref HEAD 2>$null | Out-String).Trim()
}
catch { }
finally { Pop-Location }

$files = @(
    Get-ChildItem -LiteralPath $PackageDir -Recurse -File |
        Where-Object { $_.Name -ne 'manifest.json' -and $_.Name -ne '.komorebi-package' } |
        ForEach-Object {
            [pscustomobject]@{
                path = $_.FullName.Substring($PackageDir.Length + 1).Replace('\', '/')
                bytes = $_.Length
                sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash
            }
        }
)

$manifest = [ordered]@{
    created_at = (Get-Date).ToString('o')
    source_machine = $env:COMPUTERNAME
    source_user = $env:USERNAME
    repo_root = $RepoRoot
    repo_branch = $branch
    repo_commit = $commit
    komorebi_version = (Get-ToolVersion (Join-Path $PackageBin 'komorebi.exe'))
    komorebic_version = (Get-ToolVersion (Join-Path $PackageBin 'komorebic.exe'))
    binaries = $BinaryNames
    home_files = @(@($HomeConfigNames) + @($HomeScripts | ForEach-Object { $_.Name }) + @('komorebi-model.ahk'))
    fonts = $FontFiles
    file_count = $files.Count
    total_bytes = ($files | Measure-Object -Property bytes -Sum).Sum
    files = $files
}

$manifestPath = Join-Path $PackageDir 'manifest.json'
[IO.File]::WriteAllText($manifestPath, ($manifest | ConvertTo-Json -Depth 8), $Utf8NoBom)
Write-Note "$($files.Count) 个文件，共 $([Math]::Round($manifest.total_bytes / 1MB, 1)) MB"

# ---------------------------------------------------------------------------
# 8. 压缩
# ---------------------------------------------------------------------------

$zipPath = $null
if (-not $NoZip) {
    $zipPath = Join-Path $OutputRoot ('komorebi-package-{0}.zip' -f (Get-Date -Format 'yyyyMMdd-HHmmss'))
    Write-Step "压缩到 $zipPath"
    if (Test-Path -LiteralPath $zipPath) { Remove-Item -LiteralPath $zipPath -Force }
    Compress-Archive -Path (Join-Path $PackageDir '*') -DestinationPath $zipPath -CompressionLevel Optimal
    $zipSize = [Math]::Round((Get-Item -LiteralPath $zipPath).Length / 1MB, 1)
    Write-Note "压缩包大小 $zipSize MB"
}

Write-Host ''
Write-Host '打包完成。' -ForegroundColor Green
Write-Host "  目录：$PackageDir"
if ($zipPath) { Write-Host "  压缩包：$zipPath" }
Write-Host ''
Write-Host '在公司电脑上：'
Write-Host '  1. 解压 zip 到任意目录（例如 D:\komorebi-package）'
Write-Host '  2. 先读一遍 README.md 的前置条件（PowerShell 7 + AutoHotkey v2）'
Write-Host '  3. 在解压目录里运行：pwsh -NoProfile -ExecutionPolicy Bypass -File .\Install-Komorebi.ps1'
