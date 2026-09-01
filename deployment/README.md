# komorebi 迁移包

把一台电脑上跑着的整套 komorebi（程序 + 配置 + 脚本层 + 键位表 + 字体）搬到另一台
电脑，并在目标机器上按它实际连接的显示器完成登记。双显示器开箱即用。

## 包里有什么

| 路径 | 内容 |
| --- | --- |
| `bin/` | `komorebi.exe`、`komorebic.exe`、`komorebi-bar.exe`、`komorebi-gui.exe`、`komorebi-shortcuts.exe`、`komorebic-no-console.exe` |
| `home/komorebi.json` | 静态配置：忽略/接管规则、布局、间距、边框、主题、每显示器工作区、浮动步长 |
| `home/applications.json` | 应用识别规则（komorebi 官方 applications 配置） |
| `home/komorebi.bar.json`、`.1.json`、`.2.json` | 三块屏幕各自的 bar 配置（`monitor.index` 分别是 0 / 1 / 2） |
| `home/komorebi-hotkeys.ahk` | AutoHotkey v2 热键入口，`#Include` 同目录的键位表 |
| `home/komorebi-model.ahk` | 完整键位表，全部直连 `komorebic`，不使用 whkd |
| `home/komorebi-*.ps1` | 启动、停止、看门狗、重载、显示器登记、快捷键面板等脚本层 |
| `fonts/` | bar 使用的 Maple Mono NF CN（打包时可用 `-NoFonts` 去掉） |
| `manifest.json` | 版本、提交号、逐文件 SHA256 |
| `Install-Komorebi.ps1` | 目标机器上的安装脚本 |

## 前置条件（目标机器）

1. **PowerShell 7**：`winget install --id Microsoft.PowerShell`
   脚本层的启动、看门狗、面板都通过 `pwsh.exe` 运行。
2. **AutoHotkey v2**：`winget install --id AutoHotkey.AutoHotkey`
   所有快捷键都由它直接调用 `komorebic`。安装到 `%LOCALAPPDATA%\Programs\AutoHotkey\v2`
   或 `%ProgramFiles%\AutoHotkey\v2` 都可以，脚本会自己找。
3. 一个**本机管理员账号**。komorebi 必须提权运行，否则 Windows 的 UIPI 会让它收不到
   以管理员身份运行的窗口（Everything 之类）的事件，快捷键在这些窗口获得焦点时也会失灵。

安装脚本会自己请求提权；请用**当前登录用户自己的管理员权限**确认，不要换成另一个
管理员账号运行，否则配置会装到那个账号的 `%USERPROFILE%` 下。

## 安装

解压 zip 到任意目录，然后：

```powershell
pwsh -NoProfile -ExecutionPolicy Bypass -File .\Install-Komorebi.ps1
```

先看看它准备做什么（不改任何东西）：

```powershell
pwsh -NoProfile -ExecutionPolicy Bypass -File .\Install-Komorebi.ps1 -DryRun
```

脚本按顺序做十一件事：

1. 检查 PowerShell 7 与 AutoHotkey v2；缺失就停下并给出安装命令（`-Force` 可强行继续）
2. 自我提权
3. 按 `manifest.json` 校验包内每个文件的 SHA256
4. 停掉正在运行的 komorebi / bar / 本套热键进程（不碰其它 AutoHotkey 脚本）
5. 把目标机器上的同名文件备份到 `%USERPROFILE%\.komorebi-backups\transfer-<时间戳>\`
6. 安装二进制到 `%ProgramFiles%\komorebi\bin`，并把该目录加入机器 PATH
7. 安装配置与脚本层到 `%USERPROFILE%`
8. 安装字体（用户级，不需要重启，也不写系统字体目录）
9. 注册登录自启动任务（以最高权限运行 `komorebi-start.ps1`，登录后 20 秒触发）
10. 冷启动 komorebi → 登记本机显示器 → 对齐 bar 映射 → 再冷启动一次
11. 写安装报告 `%USERPROFILE%\.komorebi-transfer-report.json`

可用开关：`-SkipFonts`、`-SkipAutostart`、`-SkipStart`、`-SkipMonitorSetup`、`-Force`、`-DryRun`。

## 双显示器是怎么适配的

`komorebi.json` 里预留了三块屏幕的位置：

- 序号 0：来源机器（笔记本）自带的屏幕
- 序号 1、2：公司电脑的两块屏幕

`display_index_preferences` 用**显示器硬件 ID** 把物理屏幕钉到这些序号上，因此同一份
配置在两台机器上都成立：每台机器只使用它当前真正连着的屏幕，离线屏幕的映射原样保留。

安装脚本第 10 步做的就是这件事：

1. `komorebi-pin-displays.ps1` 读取 `komorebic monitor-information`，把本机两块屏幕
   登记到还空着的序号（在公司电脑上通常是 1 和 2），只追加、不覆盖已有映射；
2. 安装脚本再把 `bar_configurations` 对齐到登记后的序号 —— 这一步必须做：
   序号 0 那份 bar 配置在本机没有对应屏幕时，会退回到「第一块物理屏幕」，
   在同一块屏上叠出第二条 bar；
3. 最后再冷启动一次，让每块屏的 bar 按新序号各起一条。

每块屏幕都有自己独立的 4 个工作区（`Alt+1..4` 切换的是**当前聚焦那块屏**的工作区）。
跨屏操作：`Alt+F1/F2` 聚焦第 1/2 块屏，`Alt+Shift+F1/F2` 送窗口过去，
`Alt+Ctrl+F1/F2` 送整个容器，`Alt+Ctrl+Shift+F1/F2` 送整个工作区。
这几个键用的是**物理显示器顺序**，与上面的登记序号无关，接两块屏就用 F1/F2。

换了屏幕、改了接线或换了分辨率之后，重跑一次登记即可：

```powershell
pwsh -NoProfile -File "$env:USERPROFILE\komorebi-pin-displays.ps1"
```

## 装完之后

| 快捷键 | 作用 |
| --- | --- |
| `Alt+Ctrl+S` / `Alt+Ctrl+Q` / `Alt+Shift+R` | 启动 / 停止 / 安全重启 |
| `Alt+I` | 中文快捷键面板（列出全部键位） |
| `Alt+H/J/K/L` | 聚焦左/下/上/右容器 |
| `Alt+Shift+H/J/K/L` | 与该方向容器交换 |
| `Alt+X` / `Alt+Z` | 增加 / 减少一个容器 |
| `Alt+F` | Stored / Floating 切换 |
| `Alt+Ctrl+H/J/K/L` | 移动浮动窗口 |
| `Alt+Ctrl+Shift+H/J/K/L` | 缩放浮动窗口 |
| `Alt+1..8` | 切换工作区 |

完整键位以 `Alt+I` 面板与 `home/komorebi-model.ahk` 为准。

## 出问题时

- 启动日志：`%LOCALAPPDATA%\komorebi\start.log`
- 运行日志：`%LOCALAPPDATA%\komorebi\komorebi.log`
- 安装报告：`%USERPROFILE%\.komorebi-transfer-report.json`
- 手动冷启动：`pwsh -NoProfile -File "$env:USERPROFILE\komorebi-start.ps1"`
- 检查状态：`komorebic state`、`komorebic monitor-information`

**回滚**：安装前的同名文件都在 `%USERPROFILE%\.komorebi-backups\transfer-<时间戳>\`，
`bin\` 与 `home\` 两个子目录分别对应 `%ProgramFiles%\komorebi\bin` 和 `%USERPROFILE%`，
复制回去即可。取消自启动：以管理员运行
`Unregister-ScheduledTask -TaskName komorebi -Confirm:$false`。

## 重新打包（在来源机器上）

配置改动之后，重新生成一份新的包：

```powershell
pwsh -NoProfile -File deployment\New-KomorebiTransferPackage.ps1
```

产物在 `%USERPROFILE%\komorebi-transfer\`：一个 `komorebi-package\` 目录和一个带时间戳
的 zip。`-NoFonts` 去掉字体，`-NoZip` 只生成目录。
