#!/bin/bash
set -euo pipefail

TARGET="$1"     # e.g., aarch64-apple-darwin
NAME="$2"       # e.g., tonn-macos-arm64
VERSION="${GITHUB_REF_NAME:-dev}"

APP_NAME="Tonn"
BINARY="target/${TARGET}/release/tonn"
APP_DIR="${APP_NAME}.app"

# Create .app bundle structure
mkdir -p "${APP_DIR}/Contents/MacOS"
mkdir -p "${APP_DIR}/Contents/Resources"

# Copy binary
cp "${BINARY}" "${APP_DIR}/Contents/MacOS/tonn"
chmod +x "${APP_DIR}/Contents/MacOS/tonn"

# Copy app icon
cp "assets/tonn.icns" "${APP_DIR}/Contents/Resources/AppIcon.icns"

# Create Info.plist
cat > "${APP_DIR}/Contents/Info.plist" << EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>${APP_NAME}</string>
    <key>CFBundleDisplayName</key>
    <string>${APP_NAME}</string>
    <key>CFBundleIdentifier</key>
    <string>sh.tonn.app</string>
    <key>CFBundleVersion</key>
    <string>${VERSION}</string>
    <key>CFBundleShortVersionString</key>
    <string>${VERSION}</string>
    <key>CFBundleExecutable</key>
    <string>tonn</string>
    <key>CFBundleIconFile</key>
    <string>AppIcon</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>LSMinimumSystemVersion</key>
    <string>11.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
</dict>
</plist>
EOF

# Ad-hoc code sign (no Apple Developer ID needed)
codesign --deep --force --sign - "${APP_DIR}"

# Create DMG
DMG_NAME="${NAME}.dmg"
hdiutil create -volname "${APP_NAME}" -srcfolder "${APP_DIR}" -ov -format UDBZ "${DMG_NAME}"

echo "Created ${DMG_NAME}"
