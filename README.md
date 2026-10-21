# monctl

English · [简体中文](README.zh-CN.md)

The screen assistant, as one small dependency-free binary (plus a menu bar /
tray app). macOS / Windows / Linux.

- **Turn individual displays on/off** — as if the cable were pulled.
- **Resolutions**, including the HiDPI modes System Settings hides, **main
  display** and **mirroring** from one command.
- **HiDPI on any monitor** (macOS) — crisp text on screens macOS won't treat
  as Retina.
- **Virtual displays** (macOS) — any size, HiDPI, for headless Macs and
  screen sharing.
- **Any tablet as an extra screen** (macOS, Windows or Linux/X11 → iPad /
  Android / Windows tablets) — over Wi-Fi or USB, nothing to install on the
  tablet; it gets its screen back after a drop.
- **Keep awake** / sleep now.

```console
$ monctl list
id              state     main  detail                         name
2               active    *     1920x1080 @60Hz (0,0)          DELL P2317H
5               active          1920x1080 @60Hz (1920,0)       DELL P2314H

$ monctl off          # turn off the non-main screen
disabled  5 "DELL P2314H" (1920x1080 @60Hz (1920,0))
- 1 display active now (1 changed)

$ monctl all          # bring everything back
enabled  5 "DELL P2314H" (n/a)
- 2 displays active now (1 changed)
```

A disabled display disappears from the OS (windows and the mouse can't reach it,
screen sharing sees one screen). Turning it back on restores its position and
resolution. Typical use: you remote into a desk machine with two screens —
`monctl off` before connecting, `monctl all` when you're back.

## Install

From [Releases](../../releases/latest):

| | |
| --- | --- |
| macOS | `monctl-macos.dmg` — drag **monctl.app** to Applications. It lives in the menu bar; the CLI is inside: `ln -s /Applications/monctl.app/Contents/MacOS/monctl /usr/local/bin/monctl`. Not notarized: the first launch needs right-click → Open (or System Settings → Privacy & Security → Open Anyway). |
| Windows | `monctl-setup.exe` — per-user install; options for sign-in autostart, adding `monctl` to PATH, and the screen driver for tablets (one admin prompt). Or `monctl-windows-x64.zip` (portable; it installs the driver itself on first use). |
| Linux | `monctl-linux-x64.tar.gz` |

Or `cargo build --release` (see [Build](#build)).

## Usage

```
monctl list                  show all connected displays
monctl off  [ID|main]        disable a display        (default: the non-main one)
monctl on   [ID|main|all]    enable display(s)        (default: all that are off)
monctl toggle [ID|main]      flip one display
monctl solo [ID|main]        keep only one display on (default: main)
monctl all                   enable every connected display
monctl forget [ID|all]       drop remembered off-state entries

monctl mode [ID] [WxH[@Hz]|N]  list / switch resolutions (HiDPI included; --lodpi for 1x)
monctl main ID               make a display the main one (menu bar, Dock / taskbar)
monctl mirror ID [SOURCE]    show SOURCE (default: main) on display ID; `mirror off` undoes
monctl hidpi [ID] | off [ID] macOS: crisp HiDPI rendering on any monitor
monctl virtual [add [WxH[@Hz]] [--name N] [--no-hidpi] | rm ID|all]   macOS
monctl cast [--port N] [--display ID|main] [--bitrate MBPS]          macOS, Windows, Linux (X11)
monctl driver [status|install|uninstall|restart]                     Windows: cast's screen driver
monctl sidecar [NAME|off]                                            macOS: iPad via Apple's Sidecar

monctl awake [DURATION]      prevent sleep (2h, 90m, 2h30m, 3600; default: until off)
monctl awake off|status      release / inspect the hold (--display also keeps the screen on)
monctl awake display on|off  keep the screen on (or not) without changing the duration
monctl sleep                 suspend the machine now
```

IDs come from `monctl list`; short forms work (`DISPLAY2` or `2` for
`\\.\DISPLAY2`).

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

## Resolutions, main display, mirroring

```console
$ monctl mode 2              # list; * marks the current one
*   14  2560x1440 @60Hz  HiDPI (5120x2880)
    15  2560x1440 @60Hz
    22  1920x1080 @60Hz  HiDPI (3840x2160)
    ...
$ monctl mode 2 1920x1080    # HiDPI preferred when both exist; --lodpi for 1x
$ monctl mode 2 22           # or by number from the list
$ monctl main 5              # menu bar / Dock (taskbar) move to display 5
$ monctl mirror 5            # display 5 shows the main display
$ monctl mirror off
```

On macOS the list includes the HiDPI ("looks like") modes System Settings only
shows behind "Show all resolutions". Windows: modes and primary display via
`ChangeDisplaySettingsEx` (mirroring: use Win+P). Linux: xrandr / kscreen-doctor.

## HiDPI on any monitor (macOS)

macOS renders text crisply only on screens it considers Retina. `monctl hidpi`
fixes that for any monitor: it creates a virtual HiDPI canvas the size of the
screen (rendered at 2x) and lets the real screen mirror it, downsampled — the
trick BetterDisplay is known for.

```console
$ monctl hidpi 5
hidpi on  5 "DELL P2314H" (1920x1080 @60Hz (1920,0)) - now rendered at 3840x2160 (undo: monctl hidpi off 5)
$ monctl hidpi off 5
```

Safe by construction: the canvas lives in a small holder process; undoing it,
a crash, logout or reboot all return the screen to normal.

## Virtual displays (macOS)

```console
$ monctl virtual add 2560x1440          # also 1080p, 4k, 3840x2160@30, --no-hidpi, --name
added   8 "monctl Virtual 1" (2560x1440 @60Hz, HiDPI)
$ monctl virtual                        # list
$ monctl virtual rm 8                   # or: rm all
```

A virtual display is a real screen to macOS: drag windows onto it, share it,
record it, set its resolution with `monctl mode`. Useful on headless Macs
(remote desktop at any resolution) and as a private screen during screen
sharing. Each one is owned by a tiny holder process; it disappears with it.

## Tablets as extra screens (macOS, Windows, Linux)

```console
$ monctl cast
monctl cast - open on the tablet (same Wi-Fi):
  http://192.168.1.20:8484/
  ▀▀▀ (QR code) ▀▀▀
USB  Android R58N...: wired via adb, opened http://localhost:8484/ on the tablet
```

Scan the QR code (or open the URL) on an iPad, Android or Windows tablet: a
screen shaped exactly like the tablet appears on the Mac, and the tablet shows
it. Nothing to install on the tablet — it's a web page.

- **Touch works**: tap = click, drag = drag, long press = right click, two
  fingers = scroll, ⌨ button = on-screen keyboard. (Needs Accessibility
  permission for the terminal / monctl.app; showing the screen needs Screen
  Recording permission.)
- **Wired**: plug an Android tablet in with USB debugging on — `monctl cast`
  sets up `adb reverse` and opens the page by itself. Over USB the page runs
  on `localhost`, which unlocks the lowest-latency decoder (WebCodecs); over
  Wi-Fi it uses Media Source Extensions.
- **Several tablets** at once, each with its own screen. Rotate the tablet and
  its screen follows.
- `--display main` shows an existing screen instead (presenting, a second
  view of the desk).
- Under the hood: CGDisplayStream (frames only when something changes) →
  VideoToolbox H.264 in low-latency mode → WebSocket. One worker process per
  tablet: when the tablet leaves, its screen goes with it.

In both tray apps it's one click (**投屏到平板**): the QR code pops up
(a window on macOS, a page on Windows) and the menu shows how many tablets
are connected.

**On Windows** a new screen needs an indirect display driver. monctl
embeds the open-source [Virtual Display Driver](https://github.com/VirtualDrivers/Virtual-Display-Driver)
(MIT, signed by SignPath Foundation through a GlobalSign chain Windows
already trusts) and installs it itself - from the installer, or on the
first cast with a single admin prompt. Nothing is added to the root
certificate store; the driver's publisher goes into *Trusted Publishers*
only, so Windows doesn't ask a second time. The driver keeps a small pool
of screens parked off the desktop; a tablet claims one at its exact size
and scale (an iPad at 2x gets 200%), and hands it back when it leaves.
`monctl driver status|uninstall` shows / removes it.

- Capture: DXGI Desktop Duplication; encode: Media Foundation H.264 on the
  GPU (NVENC / Quick Sync / AMF), software fallback.
- Up to 4 tablets at a time.

**On Linux** (an X11 session) the tablet's screen is a RandR monitor added
next to the desktop; `ffmpeg` (x11grab + libx264, zero-latency) captures
and encodes it, XTest replays touch. Needs `xrandr`, `ffmpeg`, `libxtst6`
(and `xdotool` for typing): `sudo apt install x11-xserver-utils ffmpeg
libxtst6 xdotool`. X11 has no per-monitor scaling, so the screen is the
tablet's CSS size (normal UI size). Wayland can't add screens from outside
the compositor - use an X11 session.

**Drop-outs**: a tablet that sleeps, reloads or hops Wi-Fi gets *its*
screen back - with its windows still on it - for 60 s after it left; the
newest connection of a tablet always wins, so a half-open old one never
blocks it.

## iPad over USB: Sidecar (macOS)

An iPad's browser can't reach the Mac through a USB cable, so for a wired
iPad monctl drives Apple's own Sidecar - USB or wireless, the best picture
and latency an iPad gets:

```console
$ monctl sidecar             # list iPads (* = connected)
$ monctl sidecar "William's iPad"
$ monctl sidecar off
```

The menu bar app lists them under **随航到 iPad**. The iPad must be signed in
to the same Apple ID. On Windows, iPads (and everything else) use `monctl cast`.

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

Re-running `awake` extends/replaces the deadline. Both tray apps also have a
one-click **keep screen on** toggle (屏幕常亮): an unlimited hold with
`--display`, so the screen cannot sleep until you click it again. Mechanisms
per platform: IOKit power assertions (macOS, visible in `pmset -g assertions`),
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

Feature support:

| | macOS | Windows | Linux |
| --- | --- | --- | --- |
| on / off / solo | ✓ | ✓ (not the primary) | ✓ X11, KDE Wayland |
| mode / main | ✓ (incl. HiDPI modes) | ✓ | ✓ |
| mirror | ✓ | Win+P | ✓ X11 |
| hidpi, virtual | ✓ | – | – |
| cast (tablets) | ✓ | ✓ (bundled driver) | ✓ X11 (ffmpeg) |
| iPad over USB | ✓ Sidecar | – | – |
| awake / sleep | ✓ | ✓ | ✓ |
| tray app | ✓ | ✓ | – |

Windows ids (`\\.\DISPLAY2`) and Linux ids (`DP-1`) are shown by `monctl list`;
macOS ids are numeric and change across reboots (the remembered off-state is
cleared on reboot anyway).

## Extras

- **macOS menu bar app** (monctl.app in the dmg; source in
  [`macos-tray/`](macos-tray)): per-display on/off, keep only main,
  resolutions (with HiDPI modes), HiDPI per monitor, virtual displays,
  one-click casting to tablets with a QR code, keep awake / keep screen on,
  start at login, and the same last-screen guard. It drives the bundled CLI
  and shares its state files. `packaging/macos/build-app.sh` builds the app
  and dmg.
- **Windows tray**: built from the same repo — `cargo build --release` produces
  `monctl-tray.exe` next to `monctl.exe` (pure win32, no dependencies). Any
  click opens the menu (per-display toggles, keep-only-main, keep-awake with a
  tick on the active duration, the one-click "keep screen on" toggle,
  autostart). The
  tooltip shows live state and every action reports via a balloon. Global
  recovery hotkey **Ctrl+Alt+Shift+D**
  (re-enables every display — for when you turned off the wrong one from a
  blind remote session). Like all Windows display APIs it must run in the
  logged-on user's session.

### Windows troubleshooting

- **Tray icon doesn't show up**: Win11 sometimes skips rendering newly
  registered tray icons (the icon IS registered — check the `^` overflow area
  for the blue M). Signing out/in or restarting "Windows Explorer" in Task
  Manager makes it appear.
- **Empty `monctl list` over SSH / from a service**: display APIs are
  session-scoped on Windows. Hop into the console session with
  [`extras/windows-session-run.cmd`](extras/windows-session-run.cmd).
- **The primary display cannot be turned off**: Windows drivers generally
  accept detaching the primary display but refuse the re-attach
  (DISP_CHANGE_FAILED on NVIDIA), leaving it dark until reboot. monctl refuses
  it in both the CLI and the tray; close a secondary display instead.
- **Display ids**: both `monctl off DISPLAY2` and `monctl off \\.\DISPLAY2`
  work; copy straight from `monctl list` output.
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

Installers: `packaging/macos/build-app.sh` (monctl.app + dmg, universal when
both Rust targets are installed) and `packaging/windows/monctl.iss` (Inno
Setup). GitHub Actions builds all of them for every `v*` tag.

## Why

The existing tools were unmaintained (DisableMonitor, last touched in 2018),
paywalled per feature (BetterDisplay; Duet/Sidecar-style apps for tablets), or
GUI-only. monctl does the useful core of all of them as one small,
dependency-free, scriptable binary — with a menu bar app on top — that also
runs on Windows and Linux.

## License

MIT — see [LICENSE](LICENSE). Bundled: the Virtual Display Driver binaries in
[`vendor/vdd`](vendor/vdd) (MIT, [their license](vendor/vdd/LICENSE)), unmodified as signed.
