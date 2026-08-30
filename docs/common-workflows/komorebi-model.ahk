#Requires AutoHotkey v2.0
#SingleInstance Force

; komorebi 窗口 / 容器 / 工作区模型的完整 AutoHotkey v2 快捷键配置。
;
; 这个脚本只负责调用 komorebic，不复制任何模型逻辑：窗口归属、容器生命周期、
; 槽位几何、焦点历史和浮动状态全部由 komorebi 自己维护。
; 本配置不使用 whkd，也不需要 whkdrc。
;
; 使用方法：
;   1. 安装 AutoHotkey v2；
;   2. 把本文件另存为 %KOMOREBI_CONFIG_HOME%\komorebi.ahk（未设置该变量时用 %USERPROFILE%）；
;   3. 双击运行，然后用 Alt+Ctrl+S 启动 komorebi。

; ============================================================================
; 顶部变量：路径、静态配置与默认步长
; ============================================================================

; komorebic.exe 的位置。已经在 PATH 中时保留 "komorebic.exe" 即可，
; 否则写成完整路径，例如 "C:\Program Files\komorebi\bin\komorebic.exe"。
global KomorebicPath := "komorebic.exe"

; komorebi 的配置目录：优先使用 KOMOREBI_CONFIG_HOME，其次是用户目录。
global KomorebiConfigHome := EnvGet("KOMOREBI_CONFIG_HOME")
if (KomorebiConfigHome = "")
    KomorebiConfigHome := EnvGet("USERPROFILE")

; 静态 JSON 配置文件，供 komorebic start --config 和 replace-configuration 使用。
global StaticConfig := KomorebiConfigHome "\komorebi.json"

; 浮动窗口的默认移动步长与缩放步长（逻辑单位，komorebi 会按显示器 DPI 换算）。
; 省略这两个参数时 komorebi 会使用配置文件中的 floating_move_delta /
; floating_resize_delta；在这里显式给出，是为了让两种步长各自可调。
global FloatingMoveDelta := 40
global FloatingResizeDelta := 40

; 等待 komorebi.exe 退出的秒数，供“安全重启”使用。
global StopTimeoutSeconds := 10

; ============================================================================
; 辅助函数
; ============================================================================

; 同步执行一条 komorebic 命令，并返回退出码。
; komorebi 会为每条修改命令回复一个结果，komorebic 把它转成退出码；
; 非 0 的退出码在这里被翻译成一条提示，而不是静默丢弃。
Komorebic(Args) {
    global KomorebicPath

    try {
        exitCode := RunWait(Format('"{1}" {2}', KomorebicPath, Args), , "Hide")
    } catch as err {
        Notify("无法执行 komorebic：" err.Message)
        return -1
    }

    if (exitCode != 0)
        Notify(OutcomeText(exitCode) "  (" Args ")")

    return exitCode
}

; 异步执行一条 komorebic 命令，用于 start / stop 这类耗时较长的操作。
KomorebicAsync(Args) {
    global KomorebicPath

    try {
        Run(Format('"{1}" {2}', KomorebicPath, Args), , "Hide")
    } catch as err {
        Notify("无法执行 komorebic：" err.Message)
    }
}

; 把 komorebic 的退出码翻译成中文说明。
OutcomeText(ExitCode) {
    switch ExitCode {
        case 10: return "无操作"
        case 11: return "该窗口不是浮动窗口"
        case 12: return "该窗口已最小化"
        case 13: return "没有合法目标"
        case 14: return "该窗口被忽略规则排除"
        case 15: return "该窗口处于暂停接管状态"
        case 16: return "命令会破坏模型不变量，已拒绝"
        default: return "komorebic 返回 " ExitCode
    }
}

; 屏幕上的短暂提示，避免弹窗打断操作。
Notify(Text) {
    ToolTip(Text)
    SetTimer(() => ToolTip(), -2000)
}

; 询问一个值（工作区 ID、容器 ID、位置序号等）后再发命令。
; Template 里用 {1} 表示用户输入的位置。
PromptThen(Prompt, Template) {
    answer := InputBox(Prompt, "komorebi", "w420 h140")
    if (answer.Result != "OK")
        return

    value := Trim(answer.Value)
    if (value = "")
        return

    Komorebic(Format(Template, value))
}

; 启动 komorebi：存在静态配置时带上 --config。
StartKomorebi() {
    global StaticConfig

    if FileExist(StaticConfig)
        KomorebicAsync(Format('start --config "{1}"', StaticConfig))
    else
        KomorebicAsync("start")
}

; 停止 komorebi。
StopKomorebi() {
    KomorebicAsync("stop")
}

; 安全重启：先停止并等待进程真正退出，再启动，避免两个实例同时接管窗口。
RestartKomorebi() {
    global StopTimeoutSeconds

    StopKomorebi()

    if !ProcessWaitClose("komorebi.exe", StopTimeoutSeconds) {
        Notify("komorebi.exe 未在 " StopTimeoutSeconds " 秒内退出，已放弃重启")
        return
    }

    Sleep(500)
    StartKomorebi()
}

; 重载静态 JSON 配置：这是当前版本正确的替换命令。
ReloadStaticConfig() {
    global StaticConfig

    if !FileExist(StaticConfig) {
        Notify("找不到静态配置：" StaticConfig)
        return
    }

    Komorebic(Format('replace-configuration "{1}"', StaticConfig))
}

; 浮动窗口移动：步长由脚本顶部的变量决定。
MoveFloating(Direction) {
    global FloatingMoveDelta
    Komorebic(Format("move-floating-window {1} {2}", Direction, FloatingMoveDelta))
}

; 浮动窗口按边缘缩放：另一条边始终不动。
ResizeFloating(Edge, Sizing) {
    global FloatingResizeDelta
    Komorebic(Format("resize-floating-window {1} {2} {3}", Edge, Sizing, FloatingResizeDelta))
}

; ============================================================================
; 窗口管理器：启动、停止、重启、配置、暂停与接管
; ============================================================================

!^s::StartKomorebi()                              ; 启动 komorebi
!^q::StopKomorebi()                               ; 停止 komorebi
!+r::RestartKomorebi()                            ; 安全重启（先停后启）
!r::ReloadStaticConfig()                          ; 重载静态 JSON 配置
!p::Komorebic("toggle-pause")                     ; 全局暂停 / 恢复
!u::Komorebic("suspend-window")                   ; 暂停接管当前窗口
!+u::Komorebic("resume-window")                   ; 恢复接管窗口（按新窗口重新处理）
!/::ToggleShortcutPanel()                         ; 显示 / 隐藏快捷键面板

; ============================================================================
; 布局与焦点
; ============================================================================

!y::Komorebic("cycle-layout next")                ; 循环到下一个布局
!+y::Komorebic("cycle-layout previous")           ; 循环到上一个布局

!h::Komorebic("focus left")                       ; 聚焦左侧容器
!j::Komorebic("focus down")                       ; 聚焦下方容器
!k::Komorebic("focus up")                         ; 聚焦上方容器
!l::Komorebic("focus right")                      ; 聚焦右侧容器

!+h::Komorebic("move left")                       ; 与左侧容器交换
!+j::Komorebic("move down")                       ; 与下方容器交换
!+k::Komorebic("move up")                         ; 与上方容器交换
!+l::Komorebic("move right")                      ; 与右侧容器交换

!Left::Komorebic("stack left")                    ; 把当前窗口并入左侧容器
!Down::Komorebic("stack down")                    ; 把当前窗口并入下方容器
!Up::Komorebic("stack up")                        ; 把当前窗口并入上方容器
!Right::Komorebic("stack right")                  ; 把当前窗口并入右侧容器

!n::Komorebic("raise-next-stack-window")          ; 把堆栈中下一层窗口提到最高并聚焦

; ============================================================================
; 窗口状态：浮动、最大化、全屏、最小化、关闭与恢复
; ============================================================================

!t::Komorebic("toggle-float")                     ; Stored / Floating 切换
!+t::Komorebic("toggle-maximize")                 ; 最大化 / 取消最大化
!^t::Komorebic("toggle-fullscreen")               ; 全屏 / 退出全屏
!q::Komorebic("close")                            ; 关闭当前窗口
!m::Komorebic("minimize")                         ; 最小化当前窗口
!+m::Komorebic("restore-last-minimized-window")   ; 恢复本工作区最后最小化的窗口

; ============================================================================
; 浮动窗口：独立移动
; 只对当前聚焦的浮动窗口生效，不改变任何容器、槽位或相邻窗口。
; ============================================================================

#Left::MoveFloating("left")                       ; 浮动窗口左移
#Down::MoveFloating("down")                       ; 浮动窗口下移
#Up::MoveFloating("up")                           ; 浮动窗口上移
#Right::MoveFloating("right")                     ; 浮动窗口右移

; ============================================================================
; 浮动窗口：按边缘缩放
; Shift 表示该边向外扩，Ctrl 表示该边向内收。
; ============================================================================

#+Left::ResizeFloating("left", "increase")        ; 左边缘向左扩
#^Left::ResizeFloating("left", "decrease")        ; 左边缘向右收
#+Right::ResizeFloating("right", "increase")      ; 右边缘向右扩
#^Right::ResizeFloating("right", "decrease")      ; 右边缘向左收
#+Up::ResizeFloating("up", "increase")            ; 上边缘向上扩
#^Up::ResizeFloating("up", "decrease")            ; 上边缘向下收
#+Down::ResizeFloating("down", "increase")        ; 下边缘向下扩
#^Down::ResizeFloating("down", "decrease")        ; 下边缘向上收

; ============================================================================
; 活动容器尺寸：移动一条逻辑共享边
; 与浮动窗口缩放是两套完全独立的命令。
; ============================================================================

!^Left::Komorebic("resize-edge left increase")    ; 左边界外扩
!^+Left::Komorebic("resize-edge left decrease")   ; 左边界内收
!^Right::Komorebic("resize-edge right increase")  ; 右边界外扩
!^+Right::Komorebic("resize-edge right decrease") ; 右边界内收
!^Up::Komorebic("resize-edge up increase")        ; 上边界外扩
!^+Up::Komorebic("resize-edge up decrease")       ; 上边界内收
!^Down::Komorebic("resize-edge down increase")    ; 下边界外扩
!^+Down::Komorebic("resize-edge down decrease")   ; 下边界内收

; ============================================================================
; 容器：手动创建与销毁
; ============================================================================

!c::Komorebic("create-container")                 ; 自动方向切分出新容器
!+c::Komorebic("create-container left-right")     ; 强制左右切分
!^c::Komorebic("create-container top-bottom")     ; 强制上下切分
!d::Komorebic("destroy-container")                ; 销毁当前容器并分发它的窗口

; ============================================================================
; 工作区：切换、新建、删除、重排
; ============================================================================

!1::Komorebic("focus-workspace 0")                ; 切换到第 1 个工作区
!2::Komorebic("focus-workspace 1")                ; 切换到第 2 个工作区
!3::Komorebic("focus-workspace 2")                ; 切换到第 3 个工作区
!4::Komorebic("focus-workspace 3")                ; 切换到第 4 个工作区
!5::Komorebic("focus-workspace 4")                ; 切换到第 5 个工作区
!6::Komorebic("focus-workspace 5")                ; 切换到第 6 个工作区
!7::Komorebic("focus-workspace 6")                ; 切换到第 7 个工作区
!8::Komorebic("focus-workspace 7")                ; 切换到第 8 个工作区

!w::Komorebic("new-workspace")                    ; 在当前显示器新建工作区
!+w::Komorebic("merge-workspace")                 ; 删除当前工作区并合并到相邻工作区

![::Komorebic("cycle-move-workspace previous")    ; 当前工作区左移一位
!]::Komorebic("cycle-move-workspace next")        ; 当前工作区右移一位
!+[::PromptThen("把当前工作区移动到第几个位置？（从 0 开始）", "move-workspace-to-index {1}")
!+]::PromptThen("与第几个位置的工作区交换？（从 0 开始）", "swap-workspace-with-index {1}")

; ============================================================================
; 把窗口 / 容器送到别处
;
; 索引形式的 move-to-workspace / send-to-workspace 移动的是整个容器；
; 单个窗口按稳定 ID 移动，ID 可以用 komorebic state 查到。
; 带 --follow 的版本焦点跟随，不带的版本保持当前焦点不动。
; ============================================================================

!+1::Komorebic("move-to-workspace 0")             ; 容器移动到第 1 个工作区（焦点跟随）
!+2::Komorebic("move-to-workspace 1")             ; 容器移动到第 2 个工作区（焦点跟随）
!+3::Komorebic("move-to-workspace 2")             ; 容器移动到第 3 个工作区（焦点跟随）
!+4::Komorebic("move-to-workspace 3")             ; 容器移动到第 4 个工作区（焦点跟随）

!^1::Komorebic("send-to-workspace 0")             ; 容器送到第 1 个工作区（不跟随）
!^2::Komorebic("send-to-workspace 1")             ; 容器送到第 2 个工作区（不跟随）
!^3::Komorebic("send-to-workspace 2")             ; 容器送到第 3 个工作区（不跟随）
!^4::Komorebic("send-to-workspace 3")             ; 容器送到第 4 个工作区（不跟随）

!g::PromptThen("把当前窗口送到哪个工作区 ID？", "move-to-workspace-id {1}")
!+g::PromptThen("把当前窗口送到哪个工作区 ID？（焦点跟随）", "move-to-workspace-id {1} --follow")
!b::PromptThen("把当前窗口送到哪个容器 ID？", "move-to-container-id {1}")
!+b::PromptThen("把当前窗口送到哪个容器 ID？（焦点跟随）", "move-to-container-id {1} --follow")

; ============================================================================
; 显示器：窗口、容器、工作区跨显示器
; ============================================================================

!F1::Komorebic("focus-monitor 0")                 ; 聚焦第 1 个显示器
!F2::Komorebic("focus-monitor 1")                 ; 聚焦第 2 个显示器
!F3::Komorebic("focus-monitor 2")                 ; 聚焦第 3 个显示器

!+F1::Komorebic("move-window-to-monitor 0 --follow") ; 单个窗口送到第 1 个显示器（跟随）
!+F2::Komorebic("move-window-to-monitor 1 --follow") ; 单个窗口送到第 2 个显示器（跟随）
!+F3::Komorebic("move-window-to-monitor 2 --follow") ; 单个窗口送到第 3 个显示器（跟随）

!^F1::Komorebic("move-to-monitor 0")              ; 整个容器移动到第 1 个显示器
!^F2::Komorebic("move-to-monitor 1")              ; 整个容器移动到第 2 个显示器
!^F3::Komorebic("move-to-monitor 2")              ; 整个容器移动到第 3 个显示器

!^+F1::Komorebic("move-workspace-to-monitor 0")   ; 整个工作区移动到第 1 个显示器
!^+F2::Komorebic("move-workspace-to-monitor 1")   ; 整个工作区移动到第 2 个显示器
!^+F3::Komorebic("move-workspace-to-monitor 2")   ; 整个工作区移动到第 3 个显示器

; ============================================================================
; 快捷键面板
;
; komorebic toggle-shortcuts 打开的面板读取的是 whkdrc，本配置不使用 whkd，
; 所以这里用一个 AutoHotkey v2 的小面板列出本文件真正绑定的快捷键。
; ============================================================================

global ShortcutPanel := ""

global ShortcutTable := [
    ["Alt+Ctrl+S / Alt+Ctrl+Q", "启动 / 停止 komorebi", "start / stop"],
    ["Alt+Shift+R", "安全重启", "stop 等待退出后 start"],
    ["Alt+R", "重载静态 JSON 配置", "replace-configuration"],
    ["Alt+P", "全局暂停 / 恢复", "toggle-pause"],
    ["Alt+U / Alt+Shift+U", "暂停接管 / 恢复接管窗口", "suspend-window / resume-window"],
    ["Alt+Y / Alt+Shift+Y", "循环布局", "cycle-layout next / previous"],
    ["Alt+H J K L", "上下左右聚焦", "focus"],
    ["Alt+Shift+H J K L", "上下左右交换容器", "move"],
    ["Alt+方向键", "把窗口并入该方向的容器", "stack"],
    ["Alt+N", "把堆栈下一层窗口提到最高", "raise-next-stack-window"],
    ["Alt+T", "Stored / Floating 切换", "toggle-float"],
    ["Alt+Shift+T / Alt+Ctrl+T", "最大化 / 全屏", "toggle-maximize / toggle-fullscreen"],
    ["Alt+Q / Alt+M / Alt+Shift+M", "关闭 / 最小化 / 恢复最小化", "close / minimize / restore-last-minimized-window"],
    ["Win+方向键", "移动浮动窗口", "move-floating-window"],
    ["Win+Shift 或 Ctrl+方向键", "浮动窗口按边缘扩 / 收", "resize-floating-window"],
    ["Alt+Ctrl(+Shift)+方向键", "活动容器边界外扩 / 内收", "resize-edge"],
    ["Alt+C / Alt+Shift+C / Alt+Ctrl+C", "自动 / 左右 / 上下创建容器", "create-container"],
    ["Alt+D", "销毁容器并分发窗口", "destroy-container"],
    ["Alt+1..8", "切换工作区", "focus-workspace"],
    ["Alt+W / Alt+Shift+W", "新建 / 删除并合并工作区", "new-workspace / merge-workspace"],
    ["Alt+[ 与 Alt+]", "工作区左移 / 右移", "cycle-move-workspace"],
    ["Alt+Shift+[ 与 Alt+Shift+]", "工作区移动到 / 交换指定位置", "move-workspace-to-index / swap-workspace-with-index"],
    ["Alt+Shift+1..4 / Alt+Ctrl+1..4", "容器送到工作区，跟随 / 不跟随", "move-to-workspace / send-to-workspace"],
    ["Alt+G / Alt+Shift+G", "窗口送到工作区 ID，不跟随 / 跟随", "move-to-workspace-id"],
    ["Alt+B / Alt+Shift+B", "窗口送到容器 ID，不跟随 / 跟随", "move-to-container-id"],
    ["Alt+F1..F3", "聚焦显示器", "focus-monitor"],
    ["Alt+Shift+F1..F3", "窗口送到显示器", "move-window-to-monitor"],
    ["Alt+Ctrl+F1..F3", "容器送到显示器", "move-to-monitor"],
    ["Alt+Ctrl+Shift+F1..F3", "工作区送到显示器", "move-workspace-to-monitor"],
    ["Alt+/", "显示 / 隐藏本面板", "（由本脚本提供）"]
]

ToggleShortcutPanel() {
    global ShortcutPanel, ShortcutTable

    if IsObject(ShortcutPanel) {
        CloseShortcutPanel()
        return
    }

    ShortcutPanel := Gui("+AlwaysOnTop -MinimizeBox", "komorebi 快捷键")
    ShortcutPanel.SetFont("s10", "Microsoft YaHei UI")

    list := ShortcutPanel.Add("ListView", "w900 r30", ["快捷键", "作用", "komorebic 命令"])
    for entry in ShortcutTable
        list.Add(, entry[1], entry[2], entry[3])

    list.ModifyCol(1, 260)
    list.ModifyCol(2, 300)
    list.ModifyCol(3, 320)

    ShortcutPanel.OnEvent("Close", (*) => CloseShortcutPanel())
    ShortcutPanel.OnEvent("Escape", (*) => CloseShortcutPanel())
    ShortcutPanel.Show()
}

CloseShortcutPanel() {
    global ShortcutPanel

    if IsObject(ShortcutPanel)
        ShortcutPanel.Destroy()

    ShortcutPanel := ""
}
