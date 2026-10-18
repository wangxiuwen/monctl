# monctl-tray (macOS menu bar app)

A tiny status-bar app that exposes the same toggles as the `monctl` CLI:

- one menu entry per display — click to turn it off, click again to turn it on
- "keep only the main display" / "turn everything on"
- **开机启动 (launch at login)** toggle — registers/removes a per-user
  LaunchAgent from the menu itself; the checkbox reflects the current state
- the **last active screen can never be turned off** (its entry is disabled)
- shares `~/Library/Application Support/monctl/state.json` with the CLI, so
  both stay in sync

No Dock icon, no window — just the double-display icon in the menu bar.

## Build & run

```sh
macos-tray/build.sh                     # → ~/.local/bin/monctl-tray and start it
macos-tray/build.sh --install-agent     # also register a LaunchAgent (start at login, auto-restart on crash)
```

## Notes

- Uses the same private `CGSConfigureDisplayEnabled` API as the CLI; the
  disable is session-scoped (logout/reboot clears it).
- Quit from the menu stops the app; if installed as a LaunchAgent it will not
  come back until next login (`KeepAlive` only restarts on abnormal exit).
