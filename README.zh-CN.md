# monctl

[English](README.md) · 简体中文

屏幕助手：一个零依赖的小二进制（外加菜单栏 / 托盘应用），支持 macOS / Windows / Linux。

- **单独关掉某块显示器**——效果等同拔线。
- **分辨率**（包括系统设置里藏起来的 HiDPI 模式）、**设主屏**、**镜像**，一条命令搞定。
- **任意显示器开 HiDPI**（macOS）——非 Retina 外接屏也能显示锐利的文字。
- **虚拟显示器**（macOS）——任意尺寸，支持 HiDPI，适合无头 Mac 和屏幕共享。
- **平板当副屏**（macOS → iPad / 安卓 / Windows 平板）——无线、有线都行，平板上什么都不用装。
- **防休眠** / 立即休眠。

```console
$ monctl list
id              state     main  detail                         name
2               active    *     1920x1080 @60Hz (0,0)          DELL P2317H
5               active          1920x1080 @60Hz (1920,0)       DELL P2314H

$ monctl off          # 关掉非主屏
disabled  5 "DELL P2314H" (1920x1080 @60Hz (1920,0))
- 1 display active now (1 changed)

$ monctl all          # 全部恢复
enabled  5 "DELL P2314H" (n/a)
- 2 displays active now (1 changed)
```

被关掉的显示器会从系统里彻底消失（窗口和鼠标都过不去，远程采集端只看得到
一块屏）。重新开启时位置、分辨率自动恢复。典型场景：远程连回接着两块屏的工位
主机前 `monctl off`，回到工位 `monctl all`。

## 安装

到 [Releases](../../releases/latest) 下载：

| | |
| --- | --- |
| macOS | `monctl-macos.dmg`——把 **monctl.app** 拖进「应用程序」，它常驻菜单栏；命令行工具在 app 里面：`ln -s /Applications/monctl.app/Contents/MacOS/monctl /usr/local/bin/monctl`。未公证，第一次打开要右键 → 打开（或 系统设置 → 隐私与安全性 → 仍要打开）。 |
| Windows | `monctl-setup.exe`——装到当前用户目录，不需要管理员；可选开机自启、加入 PATH。也有便携版 `monctl-windows-x64.zip`。 |
| Linux | `monctl-linux-x64.tar.gz` |

或者自己 `cargo build --release`（见[构建](#构建)）。

## 用法

```
monctl list                  列出所有已连接显示器
monctl off  [ID|main]        关掉某块屏      （默认：非主屏）
monctl on   [ID|main|all]    开启            （默认：所有关着的）
monctl toggle [ID|main]      切换某块屏
monctl solo [ID|main]        只保留一块屏    （默认：主屏）
monctl all                   全部开启
monctl forget [ID|all]       清除记住的关闭状态

monctl mode [ID] [WxH[@Hz]|N]  列出 / 切换分辨率（含 HiDPI；--lodpi 强制 1x）
monctl main ID               设为主屏（菜单栏、Dock / 任务栏跟过去）
monctl mirror ID [源]        让 ID 显示「源」的画面（默认主屏）；mirror off 取消
monctl hidpi [ID] | off [ID] macOS：任意显示器开 HiDPI
monctl virtual [add [WxH[@Hz]] [--name 名字] [--no-hidpi] | rm ID|all]   macOS 虚拟显示器
monctl cast [--port 端口] [--display ID|main] [--bitrate Mbps]           macOS 平板当副屏

monctl awake [时长]          阻止休眠（2h、90m、2h30m、3600；不带时长 = 直到解除）
monctl awake off|status      解除 / 查看保持状态（--display 连屏幕一起常亮）
monctl sleep                 立即休眠
```

ID 来自 `monctl list`，可以简写（`\\.\DISPLAY2` 写成 `DISPLAY2` 或 `2` 都行）。

显示器安全设计：

- **最后一块活动屏永远关不掉**。只剩一块屏时 `off` 直接拒绝（退出码 1），
  不可能把自己弄瞎。
- 关闭状态记在一个小 JSON 文件里（macOS
  `~/Library/Application Support/monctl/state.json`，Linux
  `~/.config/monctl/state.json`，Windows `%APPDATA%\monctl\state.json`）。
  因为屏幕被关后会从系统枚举里彻底消失，只有记住 id 才能找回来。
- macOS 上是会话级状态：注销/重启自动清空，不会重启后莫名黑一块屏。

## 分辨率、主屏、镜像

```console
$ monctl mode 2              # 列出；* 是当前模式
*   14  2560x1440 @60Hz  HiDPI (5120x2880)
    15  2560x1440 @60Hz
    22  1920x1080 @60Hz  HiDPI (3840x2160)
    ...
$ monctl mode 2 1920x1080    # 同尺寸有 HiDPI 时优先 HiDPI；--lodpi 选 1x
$ monctl mode 2 22           # 也可以按列表序号
$ monctl main 5              # 菜单栏 / Dock（任务栏）挪到 5 号屏
$ monctl mirror 5            # 5 号屏显示主屏画面
$ monctl mirror off
```

macOS 上列出的模式包括系统设置里要点「显示所有分辨率」才看得到的 HiDPI 模式。
Windows：分辨率和主屏走 `ChangeDisplaySettingsEx`（镜像请用 Win+P）；Linux：xrandr / kscreen-doctor。

## 任意显示器开 HiDPI（macOS）

macOS 只对它认定为 Retina 的屏幕做清晰渲染。`monctl hidpi` 让任何显示器都享受
这个待遇：建一块同尺寸、按 2 倍渲染的虚拟 HiDPI 画布，让真实屏幕镜像它（缩放后显示）
——也就是 BetterDisplay 最出名的那一招。

```console
$ monctl hidpi 5
hidpi on  5 "DELL P2314H" (1920x1080 @60Hz (1920,0)) - now rendered at 3840x2160 (undo: monctl hidpi off 5)
$ monctl hidpi off 5
```

结构上就是安全的：画布挂在一个小 holder 进程上，关闭、进程崩溃、注销、重启都会让屏幕恢复原样。

## 虚拟显示器（macOS）

```console
$ monctl virtual add 2560x1440          # 也支持 1080p、4k、3840x2160@30、--no-hidpi、--name
added   8 "monctl Virtual 1" (2560x1440 @60Hz, HiDPI)
$ monctl virtual                        # 列出
$ monctl virtual rm 8                   # 或 rm all
```

对 macOS 来说虚拟显示器就是一块真屏幕：能拖窗口、能共享、能录制，也能用
`monctl mode` 改分辨率。适合无头 Mac（任意分辨率远程桌面），或者屏幕共享时
放一块「私人屏」。每块虚拟屏挂在一个小 holder 进程上，进程没了屏也就没了。

## 平板当副屏（macOS）

```console
$ monctl cast
monctl cast - open on the tablet (same Wi-Fi):
  http://192.168.1.20:8484/
  ▀▀▀（二维码）▀▀▀
USB  Android R58N...: wired via adb, opened http://localhost:8484/ on the tablet
```

用 iPad、安卓或 Windows 平板扫码（或浏览器打开网址）：Mac 上立刻多出一块和平板
屏幕一模一样尺寸的屏，平板上显示它。平板上什么都不用装——就是一个网页。

- **能触控操作**：点按 = 单击，拖动 = 拖拽，长按 = 右键，双指 = 滚动，⌨ 按钮呼出软键盘。
  （需要给终端 / monctl.app 开「辅助功能」权限；显示画面需要「录屏」权限。）
- **有线**：安卓平板打开 USB 调试插上线，`monctl cast` 自动配置 `adb reverse`
  并在平板上打开页面。有线时页面跑在 `localhost` 上，能用延迟最低的 WebCodecs 解码；
  无线时走 Media Source Extensions。
- **多台平板**同时连，每台一块屏；平板转屏，屏幕跟着转。
- `--display main` 改为投一块现有的屏（演示、在平板上看桌面）。
- 原理：CGDisplayStream（画面有变化才出帧）→ VideoToolbox 低延迟 H.264 →
  WebSocket。每台平板一个 worker 进程，平板一走，它的屏跟着消失。

菜单栏应用里一键搞定（**投屏到平板**）：弹出二维码窗口，菜单上显示已连接几台平板。

## 防休眠

`awake` 的设计对标经典的 `caffeinate`，但是即发即忘、跨平台。`monctl awake 2h`
立刻返回：一个极小的后台 holder 进程持有系统级电源断言，到点自动退出——之后
休眠自然恢复。不驻留常驻服务、不改系统电源设置；holder 进程意外死掉时保持
也会自动失效，不会出现「永远醒着」。

```console
$ monctl awake 2h            # 保持 2 小时，之后允许休眠
awake: holding for 2h00m, sleep allowed again after that
$ monctl awake 8h --display  # 连屏幕一起常亮
$ monctl awake               # 一直保持，直到解除
$ monctl awake status
awake: holding, 1h59m left [holder pid 87571]
$ monctl awake off
awake: released
```

重复执行 `awake` 会顺延/替换到期时间。两个托盘（macOS 菜单栏 / Windows
任务栏）还有一键「**屏幕常亮**」开关：等价于不限时 + `--display`，点一下
屏幕不再休眠，再点一下恢复。各平台机制：IOKit 电源断言（macOS，
`pmset -g assertions` 可见）、`SetThreadExecutionState`（Windows）、
`systemd-inhibit`（Linux）。

macOS 上的保持同时包含 `PreventSystemSleep`（即 `caffeinate -s` 的效果）：
合盖状态下只要接着电源就不会睡——专为合盖外接的使用方式设计。和所有主流
工具（包括 Amphetamine）一样，苹果菜单里手动点「睡眠」仍然会睡；被挡住的是
闲置休眠和合盖休眠。

## 平台支持

| 平台 | 机制 | 说明 |
| --- | --- | --- |
| macOS | 私有接口 `CGSConfigureDisplayEnabled`（运行时 dlsym） | 与老牌 DisableMonitor 同原理，Apple Silicon 实测 |
| Windows | `ChangeDisplaySettingsExW` 分离（零尺寸模式） | 与 NirSoft MultiMonitorTool 同方案；具体行为取决于显卡驱动 |
| Linux (X11) | `xrandr --output … --off/--auto` | 任意桌面环境 |
| Linux (KDE Wayland) | `kscreen-doctor output.<名字>.disable/enable` | |
| Linux (GNOME Wayland) | 暂不支持 | 没有稳定的公开接口；请用 X11 会话 |

功能支持：

| | macOS | Windows | Linux |
| --- | --- | --- | --- |
| 开关 / solo | ✓ | ✓（主屏除外） | ✓ X11、KDE Wayland |
| 分辨率 / 主屏 | ✓（含 HiDPI 模式） | ✓ | ✓ |
| 镜像 | ✓ | Win+P | ✓ X11 |
| hidpi、虚拟显示器、平板投屏 | ✓ | –（需要虚拟显示驱动） | – |
| 防休眠 / 休眠 | ✓ | ✓ | ✓ |
| 托盘应用 | ✓ | ✓ | – |

Windows 的 id（`\\.\DISPLAY2`）和 Linux 的 id（`DP-1`）用 `monctl list` 查；
macOS 的 id 是数字、重启后会变（反正重启后关闭状态也清空了）。

## 附加

- **macOS 菜单栏应用**（dmg 里的 monctl.app，源码在 [`macos-tray/`](macos-tray)）：
  逐屏开关、只留主屏、分辨率（含 HiDPI 模式）、逐屏 HiDPI、虚拟显示器、
  一键投屏到平板（弹二维码）、保持唤醒 / 屏幕常亮、开机启动，同样带最后一块屏保护。
  它调用内置的 CLI，与 CLI 共享状态文件。`packaging/macos/build-app.sh` 打 app 和 dmg。
- **Windows 托盘**：`cargo build --release` 时会一起产出
  `target/release/monctl-tray.exe`（win32 原生，零依赖）。双击运行：
  单击图标即弹出菜单，逐屏开关、只留主屏、保持唤醒（防休眠，
  当前时长带勾选）和开机自启；悬停图标显示当前状态，每次操作都有气泡反馈；
  全局热键 **Ctrl+Alt+Shift+D** 一键恢复全部显示器（屏幕全关时的盲操作
  保命符）。注意在登录用户的会话里运行（远程场景用
  [`extras/windows-session-run.cmd`](extras/windows-session-run.cmd) 跳进
  控制台会话）。

### Windows 故障排查

- **托盘图标没出现**：Win11 任务栏偶尔不渲染新注册的托盘图标（图标其实已
  注册，右下角 `^` 溢出区里找找蓝色 M）。注销重登或重启「Windows 资源管理
  器」（任务管理器 → 右键 Windows 资源管理器 → 重新启动）即可显示。
- **远程/服务上下文里 `monctl list` 为空**：显示 API 是会话级的，SYSTEM
  任务计划里跑看不到屏幕；用上面的 `windows-session-run.cmd` 跳进登录用户
  的会话再执行。
- **主屏无法关闭**：Windows 显卡驱动普遍允许分离主屏，但**拒绝重新挂载**
  （NVIDIA 实测返回 DISP_CHANGE_FAILED），关掉后无法程序化找回，只能重启。
  monctl 已在 CLI 与托盘里同时禁止关闭主屏，只关副屏即可。
- **id 带 `\\.\` 前缀**：`monctl off DISPLAY2` 和 `monctl off \\.\DISPLAY2`
  都可以，`monctl list` 的输出可直接抄。

## 构建

任一平台，装好 Rust 工具链即可：

```sh
cargo build --release
# → target/release/monctl
```

安装包：`packaging/macos/build-app.sh`（monctl.app + dmg，两个 Rust target 都装了
就是通用二进制）和 `packaging/windows/monctl.iss`（Inno Setup）。每打一个 `v*`
tag，GitHub Actions 自动构建全部安装包。

## 为什么写这个

现成工具要么年久失修（DisableMonitor，2018 年后停更），要么按功能收费
（BetterDisplay；平板副屏类的 Duet、Sidecar 同类应用），要么只有图形界面。
monctl 把它们最有用的内核做成一个零依赖、可脚本化的小二进制，上面再加一个
菜单栏应用，并且同样支持 Windows 和 Linux。

## 许可

MIT —— 见 [LICENSE](LICENSE)。
