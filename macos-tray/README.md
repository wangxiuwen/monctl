# monctl-tray (macOS menu bar app)

A tiny status-bar app that exposes the same toggles as the `monctl` CLI:

- one menu entry per display — click to turn it off, click again to turn it on
- "keep only the main display" / "turn everything on"
- **分辨率** — every mode per display, including the HiDPI ones
- **HiDPI 清晰模式** — per-monitor HiDPI (`monctl hidpi`)
- **虚拟显示器** — add 1080p / 1440p / 4K virtual screens, remove them
- **投屏到平板** — one click starts `monctl cast`, shows the QR code, counts
  connected tablets; stopping it removes every tablet screen
- keep awake / keep screen on (`monctl awake`)
- **开机启动 (launch at login)** toggle — registers/removes a per-user
  LaunchAgent from the menu itself; the checkbox reflects the current state
- the **last active screen can never be turned off** (its entry is disabled)
- shares `~/Library/Application Support/monctl/state.json` with the CLI, so
  both stay in sync

No Dock icon, no window — just the double-display icon in the menu bar.

## Build & run

The release dmg ships it as **monctl.app** with the CLI inside
(`packaging/macos/build-app.sh` builds that). Everything beyond on/off and
resolutions is delegated to the `monctl` binary next to the tray executable.
For development:

```sh
macos-tray/build.sh                     # → ~/.local/bin/monctl-tray (put monctl next to it)
macos-tray/build.sh --install-agent     # also register a LaunchAgent (start at login, auto-restart on crash)
```

## Notes

- Uses the same private `CGSConfigureDisplayEnabled` API as the CLI; the
  disable is session-scoped (logout/reboot clears it).
- Quit from the menu stops the app; if installed as a LaunchAgent it will not
  come back until next login (`KeepAlive` only restarts on abnormal exit).
