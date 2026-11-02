#!/bin/sh
set -eu

case "$(uname -s)-$(uname -m)" in
    Linux-x86_64) target=x86_64-unknown-linux-gnu ;;
    Darwin-arm64) target=aarch64-apple-darwin ;;
    *)
        echo "No prebuilt claude-tabs for $(uname -s) $(uname -m)." >&2
        echo "Build it from source: cargo install --git https://github.com/donnybeelo/claude-tabs" >&2
        exit 1
        ;;
esac

dir="$HOME/.local/bin"
url="https://github.com/donnybeelo/claude-tabs/releases/latest/download/claude-tabs-$target"
mkdir -p "$dir"
tmp="$dir/.claude-tabs.download"

echo "Downloading claude-tabs..."
if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$url" -o "$tmp"
else
    wget -qO "$tmp" "$url"
fi
chmod +x "$tmp"
mv "$tmp" "$dir/claude-tabs"

case ":$PATH:" in
    *":$dir:"*) echo "Installed, run claude-tabs to begin!" ;;
    *)
        echo "Installed to $dir/claude-tabs, but that folder isn't on your PATH."
        echo "Add this line to your shell's startup file (~/.bashrc, ~/.zshrc or ~/.profile):"
        echo "    export PATH=\"\$HOME/.local/bin:\$PATH\""
        ;;
esac
