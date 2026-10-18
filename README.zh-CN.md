# monctl（中文说明）

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
```

安全设计：

- **最后一块活动屏永远关不掉**。只剩一块屏时 `off` 直接拒绝（退出码 1），
  不可能把自己弄瞎。
- 关闭状态记在一个小 JSON 文件里（macOS
  `~/Library/Application Support/monctl/state.json`，Linux
  `~/.config/monctl/state.json`，Windows `%APPDATA%\monctl\state.json`）。
  因为屏幕被关后会从系统枚举里彻底消失，只有记住 id 才能找回来。
- macOS 上是会话级状态：注销/重启自动清空，不会重启后莫名黑一块屏。

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
  逐屏开关，带「只留主屏」和同样的最后一块屏保护。编译：
  `swiftc -O macos-tray/tray.swift -o ~/bin/monctl-tray`。与 CLI 共享同一份
  状态文件。

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
