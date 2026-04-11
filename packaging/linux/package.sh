#!/bin/bash
set -euo pipefail

TARGET="$1"
NAME="$2"
BINARY="target/${TARGET}/release/tonn"

# Create tarball with binary + desktop file + icons
mkdir -p "${NAME}"
cp "${BINARY}" "${NAME}/tonn"
chmod +x "${NAME}/tonn"

# Copy desktop entry
cp "packaging/linux/tonn.desktop" "${NAME}/"

# Copy hicolor icons in the standard layout
for size in 16 32 48 64 128 256 512; do
    mkdir -p "${NAME}/icons/hicolor/${size}x${size}/apps"
    cp "assets/linux/${size}x${size}/tonn.png" "${NAME}/icons/hicolor/${size}x${size}/apps/tonn.png"
done

# Install script that copies everything into ~/.local (run by user)
cat > "${NAME}/install.sh" << 'INSTALL_EOF'
#!/bin/bash
set -euo pipefail
PREFIX="${PREFIX:-$HOME/.local}"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

install -Dm755 "$SCRIPT_DIR/tonn" "$PREFIX/bin/tonn"
install -Dm644 "$SCRIPT_DIR/tonn.desktop" "$PREFIX/share/applications/tonn.desktop"
for size in 16 32 48 64 128 256 512; do
    install -Dm644 \
        "$SCRIPT_DIR/icons/hicolor/${size}x${size}/apps/tonn.png" \
        "$PREFIX/share/icons/hicolor/${size}x${size}/apps/tonn.png"
done

# Refresh icon cache if available
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -f -t "$PREFIX/share/icons/hicolor" 2>/dev/null || true
fi

echo "Tonn installed to $PREFIX/bin/tonn"
echo "Make sure $PREFIX/bin is in your PATH."
INSTALL_EOF
chmod +x "${NAME}/install.sh"

tar czf "${NAME}.tar.gz" "${NAME}"
echo "Created ${NAME}.tar.gz"
