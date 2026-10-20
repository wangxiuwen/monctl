# monctl

[English](README.md) · 简体中文

用一条命令把单个显示器「关掉」——效果等同拔线。

支持 macOS / Windows / Linux，单个静态二进制，零依赖。

典型场景：主机接着两台外接屏，你远程连回去办公。远程只需要一块屏，但系统
里两块都活着，于是远程软件里得来回切屏。远程前跑一下 `monctl off`，桌面
缩到一块屏；回到工位 `monctl all` 全部恢复。

```console
$ monctl list
id              state     main  detail                   name
2               active    *     1920x1080 @60Hz (0,0)    DELL P2317H
5               active          1920x1080 @60Hz (1920,0) DELL P2314H

$ monctl off          # 关掉非主屏
disabled  5 "DELL P2314H" (1920x1080 @60Hz (1920,0))
— 1 display active now (1 changed)

$ monctl all          # 全部恢复
enabled  5 "DELL P2314H" (n/a)
— 2 displays active now (1 changed)
```

被关掉的显示器会从系统里彻底消失（窗口和鼠标都过不去，远程采集端只看得到
一块屏）。重新开启时位置、分辨率自动恢复。

## 用法

```
monctl list                  列出所有已连接显示器
monctl off  [ID|main]        关掉某块屏      （默认：非主屏）
monctl on   [ID|main|all]    开启            （默认：所有关着的）
monctl toggle [ID|main]      切换某块屏
monctl solo [ID|main]        只保留一块屏    （默认：主屏）
monctl all                   全部开启
monctl forget [ID|all]       清除记住的关闭状态

monctl awake [时长]          阻止休眠（2h、90m、2h30m、3600；不带时长 = 直到解除）
monctl awake off|status      解除 / 查看保持状态（--display 连屏幕一起常亮）
monctl sleep                 立即休眠
```

显示器安全设计：

- **最后一块活动屏永远关不掉**。只剩一块屏时 `off` 直接拒绝（退出码 1），
  不可能把自己弄瞎。
- 关闭状态记在一个小 JSON 文件里（macOS
  `~/Library/Application Support/monctl/state.json`，Linux
  `~/.config/monctl/state.json`，Windows `%APPDATA%\monctl\state.json`）。
  因为屏幕被关后会从系统枚举里彻底消失，只有记住 id 才能找回来。
- macOS 上是会话级状态：注销/重启自动清空，不会重启后莫名黑一块屏。

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

Windows 的 id（`\\.\DISPLAY2`）和 Linux 的 id（`DP-1`）用 `monctl list` 查；
macOS 的 id 是数字、重启后会变（反正重启后关闭状态也清空了）。

## 附加

- **macOS 菜单栏小应用**：[`macos-tray/`](macos-tray) —— 托盘图标点开即可
  逐屏开关，带「只留主屏」、一键「屏幕常亮」（阻止屏幕休眠）和同样的
  最后一块屏保护。编译：
  `swiftc -O macos-tray/tray.swift -o ~/bin/monctl-tray`。与 CLI 共享同一份
  状态文件。
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

或到 [Releases](../../releases) 下载对应平台二进制（macOS arm64/x86_64、
Windows x86_64、Linux x86_64），由 GitHub Actions 在打 tag 时自动构建。

## 为什么写这个

现成工具要么年久失修（DisableMonitor，2018 年后停更），要么核心功能收费
（BetterDisplay），要么只有图形界面。这里把需求的 200 行内核做成纯 CLI，
顺便补上 Windows 和 Linux。

## 许可

MIT —— 见 [LICENSE](LICENSE)。
