#!/bin/sh
# Build ~/Applications/UR44 Mixer.app:
#   Contents/MacOS/ur44-open                                   launcher ("show the window")
#   Contents/Helpers/UR44 Mixer Agent.app/Contents/MacOS/ur44-mixer   background agent (login item)
set -e
cd "$(dirname "$0")/.."
cargo build --release --bin ur44-mixer --bin ur44-open
APP="$HOME/Applications/UR44 Mixer.app"
AGENT="$APP/Contents/Helpers/UR44 Mixer Agent.app"

plist() { # $1 path, $2 name, $3 id, $4 executable
cat > "$1" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>$2</string>
  <key>CFBundleDisplayName</key><string>$2</string>
  <key>CFBundleIdentifier</key><string>$3</string>
  <key>CFBundleVersion</key><string>0.3.0</string>
  <key>CFBundleShortVersionString</key><string>0.3.0</string>
  <key>CFBundleExecutable</key><string>$4</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>LSUIElement</key><true/>
  <key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST
}

# Stop a running agent so its binary can be replaced (it is restarted below).
pkill -f 'UR44 Mixer Agent.app/Contents/MacOS/ur44-mixer' 2>/dev/null || true
pkill -f 'target/release/ur44-mixer' 2>/dev/null || true
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$AGENT/Contents/MacOS"
cp target/release/ur44-open "$APP/Contents/MacOS/ur44-open"
cp target/release/ur44-mixer "$AGENT/Contents/MacOS/ur44-mixer"
plist "$APP/Contents/Info.plist" "UR44 Mixer" "local.ur44.mixer" "ur44-open"
plist "$AGENT/Contents/Info.plist" "UR44 Mixer Agent" "local.ur44.mixer.agent" "ur44-mixer"
codesign --force --sign - "$AGENT"
codesign --force --sign - "$APP"
echo "Built: $APP"
