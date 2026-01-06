#!/bin/bash
set -e

INSTALL_DIR="${INSTALL_DIR:-$HOME/.duckcoding}"

echo "Uninstalling DuckCoding CLI tools..."

# Remove installation directory
if [[ -d "$INSTALL_DIR" ]]; then
    rm -rf "$INSTALL_DIR"
    echo "Removed $INSTALL_DIR"
fi

# Remove symlinks from ~/.local/bin
for cmd in claude codex gemini; do
    if [[ -L "$HOME/.local/bin/$cmd" ]]; then
        rm -f "$HOME/.local/bin/$cmd"
        echo "Removed symlink ~/.local/bin/$cmd"
    fi
done

# Clean up shell config
for rc_file in ~/.bashrc ~/.zshrc ~/.profile; do
    if [[ -f "$rc_file" ]]; then
        if grep -q ".duckcoding" "$rc_file" 2>/dev/null; then
            # Create backup and remove lines
            sed -i.bak '/.duckcoding/d' "$rc_file"
            rm -f "$rc_file.bak"
            echo "Cleaned up $rc_file"
        fi
    fi
done

echo "Uninstallation complete!"
echo "Please restart your terminal or run: source ~/.bashrc (or ~/.zshrc)"
