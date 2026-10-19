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

monctl awake [DURATION]      prevent sleep (2h, 90m, 2h30m, 3600; default: until off)
monctl awake off|status      release / inspect the hold (--display also keeps the screen on)
monctl sleep                 suspend the machine now
```

Display safety rails:

- **The last active screen can never be turned off.** `off` refuses (exit 1)
  when only one display is active — you can't blind yourself.
- **Global recovery hotkey** (both tray apps): `Ctrl+Alt+Shift+D`
  (`⌃⌥⇧D` on macOS) re-enables every display, blind-safe — even if the
  remaining screen is dark, one keypress brings the others back.
- Off-state is remembered in a small JSON file
  (`~/Library/Application Support/monctl/state.json` on macOS,
  `~/.config/monctl/state.json` on Linux, `%APPDATA%\monctl\state.json`
  on Windows), because a disabled display usually vanishes from the OS
  enumeration entirely — remembering its id is the only way back.
- On macOS the change is session-scoped: logout/reboot clears it, so a reboot
  never leaves you with a mysteriously dead screen. Physical fallbacks always
  work too: replug the cable, power-cycle the monitor, or reboot.
  (Moving the mouse only wakes *sleeping* displays — a monctl-disabled display
  is "unplugged" by definition, which is why the hotkey exists.)

## Keeping the machine awake

`awake` works like the classic `caffeinate`, but fire-and-forget and
cross-platform. `monctl awake 2h` returns immediately; a tiny detached holder
process keeps an OS-level power assertion alive for the duration, then exits
by itself — sleep simply becomes possible again. No lingering daemon, no
settings mutated, survives-holder-death cleanly (the hold auto-expires if the
holder process disappears).

```console
$ monctl awake 2h            # 2 hours, then allow sleep again
awake: holding for 2h00m, sleep allowed again after that
$ monctl awake 8h --display  # also keep the screen on
$ monctl awake               # until released
$ monctl awake status
awake: holding, 1h59m left [holder pid 87571]
$ monctl awake off
awake: released
```

Re-running `awake` extends/replaces the deadline. Mechanisms per platform:
IOKit power assertions (macOS, visible in `pmset -g assertions`),
`SetThreadExecutionState` (Windows), `systemd-inhibit` (Linux).

On macOS the hold includes `PreventSystemSleep` (the `caffeinate -s`
behaviour): with the lid closed the machine stays awake as long as it's on AC
power — made for lid-closed, external-monitor setups. Like every mainstream
tool (Amphetamine included), an explicit "Sleep" from the Apple menu still
wins; idle and lid-close sleep are what get blocked.

## Platform notes

| Platform | Mechanism | Notes |
| --- | --- | --- |
| macOS | private `CGSConfigureDisplayEnabled` (dlsym at runtime) | same mechanism as the classic DisableMonitor; verified on Apple Silicon |
| Windows | `ChangeDisplaySettingsExW` detach (zero-sized mode) | same approach as NirSoft MultiMonitorTool; behavior depends on the GPU driver |
| Linux (X11) | `xrandr --output … --off/--auto` | any WM/DE |
| Linux (KDE Wayland) | `kscreen-doctor output.<name>.disable/enable` | |
| Linux (GNOME Wayland) | not supported yet | no stable public interface; use an X11 session |

Windows specifics (verified on real hardware):

- **Run it inside your desktop session.** Windows display APIs are
  session-scoped: from a service / SSH context (session 0)
  `EnumDisplayDevicesW` sees nothing, so remote-execution wrappers must land
  the process in the interactive session (e.g. a scheduled task with an
  interactive token).
- **Some outputs refuse detaching.** GPU phantom outputs (an adapter with no
  real panel) reject the zero-sized mode with `DISP_CHANGE_BADMODE`; monctl
  reports it and leaves everything untouched. Real panels detach fine — this
  is the same limitation MultiMonitorTool has.
- Re-attach always falls back to a real registry mode, never 0x0, so a
  restored screen can't come back dead.

Windows ids (`\\.\DISPLAY2`) and Linux ids (`DP-1`) are shown by `monctl list`;
macOS ids are numeric and change across reboots (the remembered off-state is
cleared on reboot anyway).

## Extras

- **macOS menu bar app**: [`macos-tray/`](macos-tray) — a tiny tray app with the
  same toggle per display, "keep only main", "keep awake" durations, an
  auto-start-at-login toggle, and the same last-screen guard. Build with
  `swiftc -O macos-tray/tray.swift -o ~/bin/monctl-tray` (see
  [macos-tray/README.md](macos-tray/README.md)). It shares the same state files
  as the CLI.
- **Windows tray**: built from the same repo — `cargo build --release` produces
  `monctl-tray.exe` next to `monctl.exe` (pure win32, no dependencies). Same
  menus, plus an autostart toggle backed by the `HKCU …\Run` registry key.
  Like all Windows display APIs it must run in the logged-on user's session.
- **Running monctl on a headless Windows box over remote exec** (winforge,
  SSH-as-SYSTEM, …): the service context cannot see displays, so hop into the
  console session with a one-shot scheduled task — template in
  [`extras/windows-session-run.cmd`](extras/windows-session-run.cmd).

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
