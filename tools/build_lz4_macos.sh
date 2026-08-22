#!/usr/bin/env bash
# Build a static liblz4 at the wheel's macOS deployment target.
#
# WHY, rather than `brew install lz4`: Homebrew builds for the runner's own OS,
# so its liblz4 carries a minimum target of macOS 14. Bundling that into a wheel
# targeting macOS 11 makes `delocate` refuse the wheel outright —
#
#   DelocationError: Library dependencies do not satisfy target MacOS version
#   11.0: liblz4.1.10.0.dylib has a minimum target of 14.0
#
# — and the only way to accept it would be to raise the wheel's floor to macOS
# 14, silently dropping every Apple Silicon user on macOS 11-13. Building lz4
# ourselves at the wheel's own target keeps that floor where it is, and building
# it STATIC means there is no dylib for delocate to bundle at all.
set -euo pipefail

LZ4_VERSION="${LZ4_VERSION:-1.10.0}"
# $HOME, not /usr/local: the macOS runner's /usr/local is root-owned
# (Homebrew lives in /opt/homebrew on Apple Silicon), so a plain mkdir there
# fails with "Permission denied" and there is no reason to need sudo.
PREFIX="${LZ4_PREFIX:-$HOME/.secantus-lz4}"
TARGET="${MACOSX_DEPLOYMENT_TARGET:-11.0}"

if [ -f "$PREFIX/lib/liblz4.a" ]; then
    echo "lz4: already built at $PREFIX (target $TARGET)"
    exit 0
fi

work="$(mktemp -d)"
curl -fsSL "https://github.com/lz4/lz4/archive/refs/tags/v${LZ4_VERSION}.tar.gz" \
    -o "$work/lz4.tar.gz"
mkdir -p "$work/src"
tar xzf "$work/lz4.tar.gz" -C "$work/src" --strip-components=1

# `liblz4.a` only — no dylib, nothing for delocate to find or rewrite.
make -C "$work/src/lib" liblz4.a \
    CFLAGS="-O2 -fPIC -mmacosx-version-min=${TARGET}" \
    MOREFLAGS="-mmacosx-version-min=${TARGET}"

mkdir -p "$PREFIX/lib" "$PREFIX/include"
cp "$work/src/lib/liblz4.a" "$PREFIX/lib/"
cp "$work/src/lib/lz4.h" "$work/src/lib/lz4hc.h" "$work/src/lib/lz4frame.h" "$PREFIX/include/"
rm -rf "$work"
echo "lz4: built static liblz4.a for macOS ${TARGET} at $PREFIX"
