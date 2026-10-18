#!/bin/sh
# Build the macOS menu bar app and (optionally) register it as a login LaunchAgent.
set -e
BIN="${1:-$HOME/.local/bin/monctl-tray}"

swiftc -O "$(dirname "$0")/tray.swift" -o "$BIN"
echo "built $BIN"

if [ "${1:-}" = "--install-agent" ]; then
    PLIST="$HOME/Library/LaunchAgents/cn.williamwong.monctl-tray.plist"
    cat > "$PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>cn.williamwong.monctl-tray</string>
    <key>ProgramArguments</key>
    <array>
        <string>$BIN</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
</dict>
</plist>
EOF
    launchctl bootstrap "gui/$(id -u)" "$PLIST" 2>/dev/null ||
        launchctl kickstart -k "gui/$(id -u)/cn.williamwong.monctl-tray"
    echo "LaunchAgent installed and started"
fi
