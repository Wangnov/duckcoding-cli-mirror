#!/bin/bash
set -e

MIRROR_URL="${MIRROR_URL:-__MIRROR_URL__}"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.duckcoding}"
BIN_DIR="$INSTALL_DIR/bin"
TAG="${TAG:-latest}"
VERSION=""
NO_MODIFY_PATH=false
UPGRADE=false
CHECK_ONLY=false

# Detect language (zh = Chinese, otherwise English)
detect_lang() {
    local lang="${LANG:-${LC_ALL:-en}}"
    if [[ "$lang" == zh* ]]; then
        echo "zh"
    else
        echo "en"
    fi
}

LANG_CODE=$(detect_lang)

# Internationalization messages
msg() {
    local key="$1"
    shift
    case "$LANG_CODE" in
        zh)
            case "$key" in
                "unknown_option")     printf "未知选项: %s\n" "$1" ;;
                "unsupported_arch")   printf "不支持的架构: %s\n" "$1" ;;
                "unsupported_os")     printf "不支持的操作系统: %s\n" "$1" ;;
                "detected_platform")  printf "检测到平台: %s\n" "$1" ;;
                "version")            printf "版本: %s\n" "$1" ;;
                "already_up_to_date") printf "已是最新版本: %s\n" "$1" ;;
                "update_available")   printf "有可用更新: %s -> %s\n" "$1" "$2" ;;
                "downloading")        printf "正在下载 Codex...\n" ;;
                "verifying")          printf "正在校验文件完整性...\n" ;;
                "checksum_ok")        printf "SHA256 校验通过\n" ;;
                "checksum_failed")    printf "错误: SHA256 校验失败!\n期望: %s\n实际: %s\n" "$1" "$2" ;;
                "checksum_missing")   printf "错误: 无法获取校验值 (平台: %s)\n" "$1" ;;
                "extract_failed")     printf "错误: 解压失败\n" ;;
                "installed_to")       printf "Codex %s 已安装到 %s\n" "$1" "$2" ;;
                "symlink_created")    printf "已创建符号链接: ~/.local/bin/codex\n" ;;
                "path_added")         printf "已添加 PATH 到 %s\n" "$1" ;;
                "restart_terminal")   printf "请运行: source %s 或重启终端\n" "$1" ;;
                "install_complete")   printf "安装完成!\n" ;;
            esac
            ;;
        *)
            case "$key" in
                "unknown_option")     printf "Unknown option: %s\n" "$1" ;;
                "unsupported_arch")   printf "Unsupported architecture: %s\n" "$1" ;;
                "unsupported_os")     printf "Unsupported OS: %s\n" "$1" ;;
                "detected_platform")  printf "Detected platform: %s\n" "$1" ;;
                "version")            printf "Version: %s\n" "$1" ;;
                "already_up_to_date") printf "Already up to date: %s\n" "$1" ;;
                "update_available")   printf "Update available: %s -> %s\n" "$1" "$2" ;;
                "downloading")        printf "Downloading Codex...\n" ;;
                "verifying")          printf "Verifying file integrity...\n" ;;
                "checksum_ok")        printf "SHA256 checksum verified\n" ;;
                "checksum_failed")    printf "Error: SHA256 checksum mismatch!\nExpected: %s\nActual: %s\n" "$1" "$2" ;;
                "checksum_missing")   printf "Error: checksum not found (platform: %s)\n" "$1" ;;
                "extract_failed")     printf "Error: extract failed\n" ;;
                "installed_to")       printf "Codex %s installed to %s\n" "$1" "$2" ;;
                "symlink_created")    printf "Created symlink at ~/.local/bin/codex\n" ;;
                "path_added")         printf "Added PATH to %s\n" "$1" ;;
                "restart_terminal")   printf "Run: source %s or restart your terminal\n" "$1" ;;
                "install_complete")   printf "Installation complete!\n" ;;
            esac
            ;;
    esac
}

# Parse arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        --tag)
            TAG="$2"
            shift 2
            ;;
        --version)
            VERSION="$2"
            shift 2
            ;;
        --upgrade)
            UPGRADE=true
            shift
            ;;
        --no-modify-path)
            NO_MODIFY_PATH=true
            shift
            ;;
        --check)
            CHECK_ONLY=true
            shift
            ;;
        *)
            msg "unknown_option" "$1"
            exit 1
            ;;
    esac
done

# Detect platform
detect_platform() {
    local os=$(uname -s | tr '[:upper:]' '[:lower:]')
    local arch=$(uname -m)

    case "$os" in
        darwin)
            case "$arch" in
                x86_64) echo "darwin-x64" ;;
                arm64)  echo "darwin-arm64" ;;
                *)      msg "unsupported_arch" "$arch" >&2; exit 1 ;;
            esac
            ;;
        linux)
            # Check for musl
            local libc=""
            if ldd --version 2>&1 | grep -q musl; then
                libc="-musl"
            fi
            case "$arch" in
                x86_64)  echo "linux-x64${libc}" ;;
                aarch64) echo "linux-arm64${libc}" ;;
                *)       msg "unsupported_arch" "$arch" >&2; exit 1 ;;
            esac
            ;;
        *)
            msg "unsupported_os" "$os" >&2
            exit 1
            ;;
    esac
}

platform_target() {
    case "$1" in
        darwin-x64)        echo "x86_64-apple-darwin" ;;
        darwin-arm64)      echo "aarch64-apple-darwin" ;;
        linux-x64)         echo "x86_64-unknown-linux-gnu" ;;
        linux-arm64)       echo "aarch64-unknown-linux-gnu" ;;
        linux-x64-musl)    echo "x86_64-unknown-linux-musl" ;;
        linux-arm64-musl)  echo "aarch64-unknown-linux-musl" ;;
        *)                 echo "" ;;
    esac
}

PLATFORM=$(detect_platform)
msg "detected_platform" "$PLATFORM"

TARGET=$(platform_target "$PLATFORM")
if [[ -z "$TARGET" ]]; then
    msg "unsupported_arch" "$PLATFORM" >&2
    exit 1
fi

ASSET="codex-$TARGET.tar.gz"

# Get version
if [[ -z "$VERSION" ]]; then
    VERSION=$(curl -fsSL "$MIRROR_URL/codex/$TAG")
fi
msg "version" "$VERSION"

get_installed_version() {
    local version_file="$INSTALL_DIR/versions.json"
    if [[ ! -f "$version_file" ]]; then
        return
    fi

    if command -v jq &> /dev/null; then
        jq -r '.codex.version // empty' "$version_file"
    elif command -v python3 &> /dev/null; then
        python3 -c 'import json,sys; data=json.load(open(sys.argv[1])); print(data.get("codex", {}).get("version", ""))' "$version_file"
    elif command -v python &> /dev/null; then
        python -c 'import json,sys; data=json.load(open(sys.argv[1])); print(data.get("codex", {}).get("version", ""))' "$version_file"
    else
        grep -A3 '"codex"' "$version_file" 2>/dev/null | grep -o '"version"[[:space:]]*:[[:space:]]*"[^"]*"' | head -1 | cut -d'"' -f4 || true
    fi
}

CURRENT_VERSION=$(get_installed_version || true)

# Check only mode
if $CHECK_ONLY; then
    if [[ "$CURRENT_VERSION" == "$VERSION" ]]; then
        msg "already_up_to_date" "$VERSION"
    else
        msg "update_available" "$CURRENT_VERSION" "$VERSION"
    fi
    exit 0
fi

# Skip download if already up to date (unless --upgrade is specified)
if [[ "$CURRENT_VERSION" == "$VERSION" ]] && ! $UPGRADE; then
    msg "already_up_to_date" "$VERSION"
    exit 0
fi

# Show update info if upgrading
if [[ -n "$CURRENT_VERSION" ]] && [[ "$CURRENT_VERSION" != "$VERSION" ]]; then
    msg "update_available" "$CURRENT_VERSION" "$VERSION"
fi

# Create directories
mkdir -p "$BIN_DIR"

# Get expected checksum from checksums API
CHECKSUMS=$(curl -fsSL "$MIRROR_URL/api/codex/checksums")

get_expected_field() {
    local checksums_json="$1"
    local version="$2"
    local platform="$3"
    local field="$4"
    local value=""

    if command -v jq &> /dev/null; then
        value=$(printf "%s" "$checksums_json" | jq -r --arg v "$version" --arg p "$platform" --arg f "$field" '.[$v][$p][$f] // empty')
    elif command -v python3 &> /dev/null; then
        value=$(printf "%s" "$checksums_json" | python3 -c 'import json,sys; v=sys.argv[1]; p=sys.argv[2]; f=sys.argv[3]; data=json.load(sys.stdin); print(((data.get(v, {}) or {}).get(p, {}) or {}).get(f, ""))' "$version" "$platform" "$field")
    elif command -v python &> /dev/null; then
        value=$(printf "%s" "$checksums_json" | python -c 'import json,sys; v=sys.argv[1]; p=sys.argv[2]; f=sys.argv[3]; data=json.load(sys.stdin); print(((data.get(v, {}) or {}).get(p, {}) or {}).get(f, ""))' "$version" "$platform" "$field")
    else
        value=""
    fi

    printf "%s" "$value"
}

EXPECTED_SHA256=$(get_expected_field "$CHECKSUMS" "$VERSION" "$PLATFORM" "sha256")
EXPECTED_FILENAME=$(get_expected_field "$CHECKSUMS" "$VERSION" "$PLATFORM" "filename")

if [[ -z "$EXPECTED_SHA256" ]]; then
    msg "checksum_missing" "$PLATFORM"
    exit 1
fi

if [[ -n "$EXPECTED_FILENAME" ]]; then
    ASSET="$EXPECTED_FILENAME"
fi

# Download asset
msg "downloading"
TMP_DIR=$(mktemp -d)
trap 'rm -rf "$TMP_DIR"' EXIT
TMP_ARCHIVE="$TMP_DIR/$ASSET"
EXTRACT_DIR="$TMP_DIR/extract"

curl -fL "$MIRROR_URL/codex/$VERSION/$PLATFORM/$ASSET" -o "$TMP_ARCHIVE"

# Verify SHA256 checksum
msg "verifying"
if command -v sha256sum &> /dev/null; then
    ACTUAL_SHA256=$(sha256sum "$TMP_ARCHIVE" | cut -d' ' -f1)
elif command -v shasum &> /dev/null; then
    ACTUAL_SHA256=$(shasum -a 256 "$TMP_ARCHIVE" | cut -d' ' -f1)
else
    ACTUAL_SHA256="$EXPECTED_SHA256"
fi

if [[ "$ACTUAL_SHA256" != "$EXPECTED_SHA256" ]]; then
    msg "checksum_failed" "$EXPECTED_SHA256" "$ACTUAL_SHA256"
    rm -f "$TMP_ARCHIVE"
    exit 1
fi
msg "checksum_ok"

if ! command -v tar &> /dev/null; then
    msg "extract_failed" >&2
    exit 1
fi

mkdir -p "$EXTRACT_DIR"
tar -xzf "$TMP_ARCHIVE" -C "$EXTRACT_DIR" || { msg "extract_failed"; exit 1; }

BIN_PATH=$(find "$EXTRACT_DIR" -maxdepth 4 -type f \( -name "codex" -o -name "codex-*" \) | head -1 || true)
if [[ -z "$BIN_PATH" ]]; then
    # Fallback: pick the first file if archive only contains a single binary
    BIN_PATH=$(find "$EXTRACT_DIR" -maxdepth 4 -type f | head -1 || true)
fi
if [[ -z "$BIN_PATH" ]]; then
    msg "extract_failed" >&2
    exit 1
fi

cp -f "$BIN_PATH" "$BIN_DIR/codex"
chmod +x "$BIN_DIR/codex"

# Save version info
VERSION_FILE="$INSTALL_DIR/versions.json"
if command -v jq &> /dev/null && [[ -f "$VERSION_FILE" ]]; then
    tmp_json="$TMP_DIR/versions.json"
    jq --arg v "$VERSION" --arg t "$TAG" --arg ts "$(date -u +"%Y-%m-%dT%H:%M:%SZ")" \
        '.codex = {"version":$v,"tag":$t,"installed_at":$ts}' \
        "$VERSION_FILE" > "$tmp_json" && mv "$tmp_json" "$VERSION_FILE"
elif command -v python3 &> /dev/null && [[ -f "$VERSION_FILE" ]]; then
    python3 - "$VERSION_FILE" "$VERSION" "$TAG" <<'PY'
import json
import sys
path, version, tag = sys.argv[1:4]
with open(path, "r", encoding="utf-8") as f:
    data = json.load(f)
data["codex"] = {
    "version": version,
    "tag": tag,
    "installed_at": __import__("datetime").datetime.utcnow().strftime("%Y-%m-%dT%H:%M:%SZ"),
}
with open(path, "w", encoding="utf-8") as f:
    json.dump(data, f, indent=2)
PY
else
    cat > "$VERSION_FILE" << VERSIONS_EOF
{
  "codex": {
    "version": "$VERSION",
    "tag": "$TAG",
    "installed_at": "$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
  }
}
VERSIONS_EOF
fi

msg "installed_to" "$VERSION" "$BIN_DIR/codex"

# Setup PATH
if ! $NO_MODIFY_PATH; then
    setup_path() {
        # Method A: Check if ~/.local/bin is in PATH
        if [[ ":$PATH:" == *":$HOME/.local/bin:"* ]]; then
            mkdir -p "$HOME/.local/bin"
            ln -sf "$BIN_DIR/codex" "$HOME/.local/bin/codex"
            msg "symlink_created"
            return
        fi

        # Method B: Modify shell config
        local shell_name
        shell_name=$(basename "$SHELL")
        local rc_file=""
        case "$shell_name" in
            bash) rc_file="$HOME/.bashrc" ;;
            zsh)  rc_file="$HOME/.zshrc" ;;
            fish) rc_file="$HOME/.config/fish/config.fish" ;;
            *)    rc_file="$HOME/.profile" ;;
        esac

        local path_line='export PATH="$HOME/.duckcoding/bin:$PATH"'
        if ! grep -q ".duckcoding/bin" "$rc_file" 2>/dev/null; then
            {
                echo ""
                echo "# DuckCoding CLI Mirror"
                echo "$path_line"
            } >> "$rc_file"
            msg "path_added" "$rc_file"
            msg "restart_terminal" "$rc_file"
        fi
    }
    setup_path
fi

msg "install_complete"
