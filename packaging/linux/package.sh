#!/bin/bash
set -euo pipefail

TARGET="$1"
NAME="$2"
BINARY="target/${TARGET}/release/tonn"

# Create tarball with binary + desktop file
mkdir -p "${NAME}"
cp "${BINARY}" "${NAME}/tonn"
chmod +x "${NAME}/tonn"

# Copy desktop entry if exists
if [ -f "packaging/linux/tonn.desktop" ]; then
    cp "packaging/linux/tonn.desktop" "${NAME}/"
fi

tar czf "${NAME}.tar.gz" "${NAME}"
echo "Created ${NAME}.tar.gz"
