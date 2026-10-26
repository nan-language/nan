#!/bin/sh
# Installs `nan`, the NaN toolchain of the latest release or of the tag in NAN_VERSION, into
# NAN_INSTALL_DIR (by default ~/.local/bin), after checking the SHA-256 the release publishes.
set -eu

repository=https://github.com/nan-language/nan
dir=${NAN_INSTALL_DIR:-$HOME/.local/bin}

case "$(uname -s) $(uname -m)" in
    "Linux x86_64") ;;
    *)
        echo "nan: releases are built for Linux on x86_64 only" >&2
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

asset="nan-$version-x86_64-linux"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
curl -fsSL -o "$work/$asset" "$repository/releases/download/$version/$asset"
curl -fsSL -o "$work/$asset.sha256" "$repository/releases/download/$version/$asset.sha256"
(cd "$work" && sha256sum --check --quiet "$asset.sha256")

mkdir -p "$dir"
install -m 755 "$work/$asset" "$dir/nan"
echo "installed nan $version as $dir/nan"
case ":$PATH:" in
    *":$dir:"*) ;;
    *) echo "add $dir to your PATH to run nan" ;;
esac
