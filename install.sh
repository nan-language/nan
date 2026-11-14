#!/bin/sh
# Installs `nan`, the NaN toolchain of the latest release or of the tag in NAN_VERSION, into
# NAN_INSTALL_DIR (by default ~/.local/bin), after checking the SHA-256 the release publishes.
set -eu

repository=https://github.com/nan-language/nan
dir=${NAN_INSTALL_DIR:-$HOME/.local/bin}

case "$(uname -s) $(uname -m)" in
    "Linux x86_64") target=x86_64-linux ;;
    "Linux aarch64") target=aarch64-linux ;;
    "Darwin x86_64") target=x86_64-macos ;;
    "Darwin arm64") target=aarch64-macos ;;
    *)
        echo "nan: releases are built for Linux and macOS on x86_64 and AArch64 only" >&2
        exit 1
        ;;
esac

version=${NAN_VERSION:-$(curl -fsSLI -o /dev/null -w '%{url_effective}' "$repository/releases/latest" | sed 's|.*/tag/||')}
case "$version" in
    v[0-9]*) ;;
    *)
        echo "nan: no release found" >&2
        exit 1
        ;;
esac

asset="nan-$version-$target"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
curl -fsSL -o "$work/$asset" "$repository/releases/download/$version/$asset"
curl -fsSL -o "$work/$asset.sha256" "$repository/releases/download/$version/$asset.sha256"
if command -v sha256sum > /dev/null; then
    (cd "$work" && sha256sum --check --quiet "$asset.sha256")
else
    (cd "$work" && shasum -a 256 --check --status "$asset.sha256")
fi

mkdir -p "$dir"
install -m 755 "$work/$asset" "$dir/nan"
echo "installed nan $version as $dir/nan"
case ":$PATH:" in
    *":$dir:"*) ;;
    *) echo "add $dir to your PATH to run nan" ;;
esac
