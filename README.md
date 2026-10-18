# monctl

Turn individual displays on/off from the command line — as if the cable were pulled.

macOS / Windows / Linux. Single static binary, zero dependencies.

Use case: your Mac/PC sits on a desk with two external screens, and you remote into
it. Screen sharing only needs one screen, but the OS keeps both active — so you
end up switching monitors in the remote session. `monctl off` collapses the
desktop to a single screen before you connect; `monctl all` brings everything
back when you're at the desk.

```console
$ monctl list
id              state     main  detail                   name
2               active    *     1920x1080 @60Hz (0,0)    DELL P2317H
5               active          1920x1080 @60Hz (1920,0) DELL P2314H

$ monctl off          # turn off the non-main screen
disabled  5 "DELL P2314H" (1920x1080 @60Hz (1920,0))
— 1 display active now (1 changed)

$ monctl all          # bring everything back
enabled  5 "DELL P2314H" (n/a)
— 2 displays active now (1 changed)
```

A disabled display disappears from the OS (windows and the mouse can't reach it,
screen sharing sees one screen). Turning it back on restores its position and
resolution.

## Usage

```
monctl list                  show all connected displays
monctl off  [ID|main]        disable a display        (default: the non-main one)
monctl on   [ID|main|all]    enable display(s)        (default: all that are off)
monctl toggle [ID|main]      flip one display
monctl solo [ID|main]        keep only one display on (default: main)
monctl all                   enable every connected display
monctl forget [ID|all]       drop remembered off-state entries
```

Safety rails:

- **The last active screen can never be turned off.** `off` refuses (exit 1)
  when only one display is active — you can't blind yourself.
- Off-state is remembered in a small JSON file
  (`~/Library/Application Support/monctl/state.json` on macOS,
  `~/.config/monctl/state.json` on Linux, `%APPDATA%\monctl\state.json`
  on Windows), because a disabled display usually vanishes from the OS
  enumeration entirely — remembering its id is the only way back.
- On macOS the change is session-scoped: logout/reboot clears it, so a reboot
  never leaves you with a mysteriously dead screen.

## Platform notes

| Platform | Mechanism | Notes |
| --- | --- | --- |
| macOS | private `CGSConfigureDisplayEnabled` (dlsym at runtime) | same mechanism as the classic DisableMonitor; verified on Apple Silicon |
| Windows | `ChangeDisplaySettingsExW` detach (zero-sized mode) | same approach as NirSoft MultiMonitorTool; behavior depends on the GPU driver |
| Linux (X11) | `xrandr --output … --off/--auto` | any WM/DE |
| Linux (KDE Wayland) | `kscreen-doctor output.<name>.disable/enable` | |
| Linux (GNOME Wayland) | not supported yet | no stable public interface; use an X11 session |

Windows ids (`\\.\DISPLAY2`) and Linux ids (`DP-1`) are shown by `monctl list`;
macOS ids are numeric and change across reboots (the remembered off-state is
cleared on reboot anyway).

## Extras

- **macOS menu bar app**: [`macos-tray/`](macos-tray) — a tiny tray app with the
  same toggle per display, "keep only main", and the same last-screen guard.
  Build with `swiftc -O macos-tray/tray.swift -o ~/bin/monctl-tray` (see
  [macos-tray/README.md](macos-tray/README.md)). It shares the same state file
  as the CLI.

## Build

Any of the three platforms, with a Rust toolchain:

```sh
cargo build --release
# → target/release/monctl
```

Or grab a prebuilt binary from [Releases](../../releases) (macOS arm64/x86_64,
Windows x86_64, Linux x86_64) — built by GitHub Actions on each tag.

## Why

The existing tools for this were either unmaintained (DisableMonitor, last
touched in 2018), paywalled per-feature (BetterDisplay), or GUI-only. This is
the 200-line core of the idea as a plain CLI that also runs on Windows and
Linux.

## License

MIT — see [LICENSE](LICENSE).
