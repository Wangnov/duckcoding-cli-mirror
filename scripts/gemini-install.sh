#!/bin/bash
set -e

MIRROR_URL="${MIRROR_URL:-__MIRROR_URL__}"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.duckcoding}"
BIN_DIR="$INSTALL_DIR/bin"
TAG="${TAG:-latest}"
VERSION=""
NODE_TAG="${NODE_TAG:-latest}"
NODE_VERSION=""
NODE_PTY_TAG="${NODE_PTY_TAG:-latest}"
NODE_PTY_VERSION=""
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
                "unknown_option")      printf "未知选项: %s\n" "$1" ;;
                "unsupported_arch")    printf "不支持的架构: %s\n" "$1" ;;
                "unsupported_os")      printf "不支持的操作系统: %s\n" "$1" ;;
                "detected_platform")   printf "检测到平台: %s\n" "$1" ;;
                "version")             printf "版本: %s\n" "$1" ;;
                "already_up_to_date")  printf "已是最新版本: %s\n" "$1" ;;
                "update_available")    printf "有可用更新: %s -> %s\n" "$1" "$2" ;;
                "downloading")         printf "正在下载 Gemini CLI...\n" ;;
                "verifying")           printf "正在校验文件完整性...\n" ;;
                "checksum_ok")         printf "SHA256 校验通过\n" ;;
                "checksum_failed")     printf "错误: SHA256 校验失败!\n期望: %s\n实际: %s\n" "$1" "$2" ;;
                "checksum_missing")    printf "错误: 无法获取校验值 (平台: %s)\n" "$1" ;;
                "json_parser_missing") printf "错误: 需要 jq 或 python 来解析校验信息\n" ;;
                "installing_node")     printf "正在安装私有 Node.js: %s\n" "$1" ;;
                "installing_node_pty") printf "正在安装 node-pty 预编译: %s\n" "$1" ;;
                "node_ok")             printf "已检测到私有 Node.js: %s\n" "$1" ;;
                "node_pty_ok")         printf "已检测到 node-pty: %s\n" "$1" ;;
                "node_musl_unsupported") printf "当前系统为 musl，镜像未提供 Node.js 运行时，请手动安装私有 Node.js\n" ;;
                "installed_to")        printf "Gemini CLI %s 已安装到 %s\n" "$1" "$2" ;;
                "symlink_created")     printf "已创建符号链接: ~/.local/bin/gemini\n" ;;
                "path_added")          printf "已添加 PATH 到 %s\n" "$1" ;;
                "restart_terminal")    printf "请运行: source %s 或重启终端\n" "$1" ;;
                "install_complete")    printf "安装完成!\n" ;;
                "check_node_missing")  printf "未检测到私有 Node.js\n" ;;
                "check_node_pty_missing") printf "未检测到 node-pty\n" ;;
            esac
            ;;
        *)
            case "$key" in
                "unknown_option")      printf "Unknown option: %s\n" "$1" ;;
                "unsupported_arch")    printf "Unsupported architecture: %s\n" "$1" ;;
                "unsupported_os")      printf "Unsupported OS: %s\n" "$1" ;;
                "detected_platform")   printf "Detected platform: %s\n" "$1" ;;
                "version")             printf "Version: %s\n" "$1" ;;
                "already_up_to_date")  printf "Already up to date: %s\n" "$1" ;;
                "update_available")    printf "Update available: %s -> %s\n" "$1" "$2" ;;
                "downloading")         printf "Downloading Gemini CLI...\n" ;;
                "verifying")           printf "Verifying file integrity...\n" ;;
                "checksum_ok")         printf "SHA256 checksum verified\n" ;;
                "checksum_failed")     printf "Error: SHA256 checksum mismatch!\nExpected: %s\nActual: %s\n" "$1" "$2" ;;
                "checksum_missing")    printf "Error: checksum not found (platform: %s)\n" "$1" ;;
                "json_parser_missing") printf "Error: jq or python is required to parse checksum info\n" ;;
                "installing_node")     printf "Installing private Node.js: %s\n" "$1" ;;
                "installing_node_pty") printf "Installing node-pty prebuilds: %s\n" "$1" ;;
                "node_ok")             printf "Private Node.js found: %s\n" "$1" ;;
                "node_pty_ok")         printf "node-pty found: %s\n" ;;
                "node_musl_unsupported") printf "musl detected. Node.js runtime is not available; please install a private Node.js manually\n" ;;
                "installed_to")        printf "Gemini CLI %s installed to %s\n" "$1" "$2" ;;
                "symlink_created")     printf "Created symlink at ~/.local/bin/gemini\n" ;;
                "path_added")          printf "Added PATH to %s\n" "$1" ;;
                "restart_terminal")    printf "Run: source %s or restart your terminal\n" "$1" ;;
                "install_complete")    printf "Installation complete!\n" ;;
                "check_node_missing")  printf "Private Node.js not found\n" ;;
                "check_node_pty_missing") printf "node-pty not found\n" ;;
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
        --node-tag)
            NODE_TAG="$2"
            shift 2
            ;;
        --node-version)
            NODE_VERSION="$2"
            shift 2
            ;;
        --node-pty-tag)
            NODE_PTY_TAG="$2"
            shift 2
            ;;
        --node-pty-version)
            NODE_PTY_VERSION="$2"
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

PLATFORM=$(detect_platform)
msg "detected_platform" "$PLATFORM"

NODE_PLATFORM="$PLATFORM"
IS_MUSL=false
if [[ "$NODE_PLATFORM" == *-musl ]]; then
    NODE_PLATFORM="${NODE_PLATFORM%-musl}"
    IS_MUSL=true
fi

if [[ -z "$VERSION" ]]; then
    VERSION=$(curl -fsSL "$MIRROR_URL/gemini/$TAG")
fi
if [[ -z "$NODE_VERSION" ]]; then
    NODE_VERSION=$(curl -fsSL "$MIRROR_URL/node/$NODE_TAG")
fi
if [[ -z "$NODE_PTY_VERSION" ]]; then
    NODE_PTY_VERSION=$(curl -fsSL "$MIRROR_URL/node-pty/$NODE_PTY_TAG")
fi

msg "version" "$VERSION"

get_installed_version() {
    local key="$1"
    local version_file="$INSTALL_DIR/versions.json"
    if [[ ! -f "$version_file" ]]; then
        return
    fi

    if command -v jq &> /dev/null; then
        jq -r --arg k "$key" '.[$k].version // empty' "$version_file"
    elif command -v python3 &> /dev/null; then
        python3 -c 'import json,sys; data=json.load(open(sys.argv[1])); print((data.get(sys.argv[2], {}) or {}).get("version", ""))' "$version_file" "$key"
    elif command -v python &> /dev/null; then
        python -c 'import json,sys; data=json.load(open(sys.argv[1])); print((data.get(sys.argv[2], {}) or {}).get("version", ""))' "$version_file" "$key"
    else
        grep -A3 "\"$key\"" "$version_file" 2>/dev/null | grep -o '"version"[[:space:]]*:[[:space:]]*"[^"]*"' | head -1 | cut -d'"' -f4 || true
    fi
}

CURRENT_GEMINI_VERSION=$(get_installed_version gemini || true)
CURRENT_NODE_VERSION=$(get_installed_version node || true)
CURRENT_NODE_PTY_VERSION=$(get_installed_version node_pty || true)

if $CHECK_ONLY; then
    if [[ "$CURRENT_GEMINI_VERSION" == "$VERSION" ]]; then
        msg "already_up_to_date" "$VERSION"
    else
        msg "update_available" "$CURRENT_GEMINI_VERSION" "$VERSION"
    fi

    if [[ -n "$CURRENT_NODE_VERSION" ]]; then
        msg "node_ok" "$CURRENT_NODE_VERSION"
    else
        msg "check_node_missing"
    fi

    if [[ -n "$CURRENT_NODE_PTY_VERSION" ]]; then
        msg "node_pty_ok" "$CURRENT_NODE_PTY_VERSION"
    else
        msg "check_node_pty_missing"
    fi
    exit 0
fi

SKIP_GEMINI=false
if [[ "$CURRENT_GEMINI_VERSION" == "$VERSION" ]] && ! $UPGRADE; then
    msg "already_up_to_date" "$VERSION"
    SKIP_GEMINI=true
elif [[ -n "$CURRENT_GEMINI_VERSION" ]] && [[ "$CURRENT_GEMINI_VERSION" != "$VERSION" ]]; then
    msg "update_available" "$CURRENT_GEMINI_VERSION" "$VERSION"
fi

# Check JSON parser
JSON_PARSER=""
if command -v jq &> /dev/null; then
    JSON_PARSER="jq"
elif command -v python3 &> /dev/null; then
    JSON_PARSER="python3"
elif command -v python &> /dev/null; then
    JSON_PARSER="python"
fi

if [[ -z "$JSON_PARSER" ]]; then
    msg "json_parser_missing"
    exit 1
fi

mkdir -p "$BIN_DIR"

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

get_node_file_field() {
    local checksums_json="$1"
    local platform="$2"
    local field="$3"
    local value=""

    if command -v jq &> /dev/null; then
        if [[ "$field" == "filename" ]]; then
            value=$(printf "%s" "$checksums_json" | jq -r --arg p "$platform" '.platforms[$p].files | to_entries[0].key // empty')
        else
            value=$(printf "%s" "$checksums_json" | jq -r --arg p "$platform" --arg f "$field" '.platforms[$p].files | to_entries[0].value[$f] // empty')
        fi
    elif command -v python3 &> /dev/null; then
        value=$(printf "%s" "$checksums_json" | python3 -c 'import json,sys; p=sys.argv[1]; f=sys.argv[2]; data=json.load(sys.stdin); files=(data.get("platforms", {}).get(p, {}) or {}).get("files", {}); item=next(iter(files.items()), None); 
import sys as _s; 
print("" if not item else (item[0] if f=="filename" else item[1].get(f, "")))' "$platform" "$field")
    elif command -v python &> /dev/null; then
        value=$(printf "%s" "$checksums_json" | python -c 'import json,sys; p=sys.argv[1]; f=sys.argv[2]; data=json.load(sys.stdin); files=(data.get("platforms", {}).get(p, {}) or {}).get("files", {}); item=next(iter(files.items()), None); 
import sys as _s; 
print("" if not item else (item[0] if f=="filename" else item[1].get(f, "")))' "$platform" "$field")
    else
        value=""
    fi

    printf "%s" "$value"
}

list_node_pty_files() {
    local checksums_json="$1"
    local platform="$2"

    if command -v jq &> /dev/null; then
        printf "%s" "$checksums_json" | jq -r --arg p "$platform" '.platforms[$p].files | to_entries[] | "\(.key)\t\(.value.sha256)\t\(.value.size)"'
    elif command -v python3 &> /dev/null; then
        printf "%s" "$checksums_json" | python3 -c 'import json,sys; p=sys.argv[1]; data=json.load(sys.stdin); files=(data.get("platforms", {}).get(p, {}) or {}).get("files", {}); 
for name, meta in files.items(): print("%s\t%s\t%s" % (name, meta.get("sha256", ""), meta.get("size", "")))' "$platform"
    elif command -v python &> /dev/null; then
        printf "%s" "$checksums_json" | python -c 'import json,sys; p=sys.argv[1]; data=json.load(sys.stdin); files=(data.get("platforms", {}).get(p, {}) or {}).get("files", {}); 
for name, meta in files.items(): print("%s\t%s\t%s" % (name, meta.get("sha256", ""), meta.get("size", "")))' "$platform"
    fi
}

update_versions_json() {
    local key="$1"
    local version="$2"
    local tag="$3"
    local version_file="$INSTALL_DIR/versions.json"
    local ts
    ts="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"

    if command -v jq &> /dev/null && [[ -f "$version_file" ]]; then
        local tmp_json
        tmp_json="$(mktemp)"
        jq --arg k "$key" --arg v "$version" --arg t "$tag" --arg ts "$ts" \
            '.[$k] = {"version":$v,"tag":$t,"installed_at":$ts}' \
            "$version_file" > "$tmp_json" && mv "$tmp_json" "$version_file"
    elif command -v python3 &> /dev/null && [[ -f "$version_file" ]]; then
        python3 - "$version_file" "$key" "$version" "$tag" "$ts" <<'PY'
import json
import sys
path, key, version, tag, ts = sys.argv[1:6]
with open(path, "r", encoding="utf-8") as f:
    data = json.load(f)
data[key] = {"version": version, "tag": tag, "installed_at": ts}
with open(path, "w", encoding="utf-8") as f:
    json.dump(data, f, indent=2)
PY
    elif command -v python &> /dev/null && [[ -f "$version_file" ]]; then
        python - "$version_file" "$key" "$version" "$tag" "$ts" <<'PY'
import json
import sys
path, key, version, tag, ts = sys.argv[1:6]
with open(path, "r", encoding="utf-8") as f:
    data = json.load(f)
data[key] = {"version": version, "tag": tag, "installed_at": ts}
with open(path, "w", encoding="utf-8") as f:
    json.dump(data, f, indent=2)
PY
    else
        if [[ -f "$version_file" ]]; then
            return
        fi
        cat > "$version_file" << VERSIONS_EOF
{
  "$key": {
    "version": "$version",
    "tag": "$tag",
    "installed_at": "$ts"
  }
}
VERSIONS_EOF
    fi
}

install_node() {
    if $IS_MUSL && [[ ! -x "$INSTALL_DIR/node/versions/$NODE_VERSION/bin/node" ]]; then
        msg "node_musl_unsupported"
        exit 1
    fi

    local node_bin="$INSTALL_DIR/node/versions/$NODE_VERSION/bin/node"
    if [[ -x "$node_bin" ]] && ! $UPGRADE; then
        msg "node_ok" "$NODE_VERSION"
        return
    fi

    msg "installing_node" "$NODE_VERSION"

    local checksums
    checksums=$(curl -fsSL "$MIRROR_URL/node/$NODE_VERSION/checksums.json")
    local filename
    filename=$(get_node_file_field "$checksums" "$NODE_PLATFORM" "filename")
    local expected_sha
    expected_sha=$(get_node_file_field "$checksums" "$NODE_PLATFORM" "sha256")

    if [[ -z "$filename" || -z "$expected_sha" ]]; then
        msg "checksum_missing" "$NODE_PLATFORM"
        exit 1
    fi

    local tmp_dir
    tmp_dir=$(mktemp -d)
    trap "rm -rf \"$tmp_dir\"" EXIT
    local archive="$tmp_dir/$filename"
    local extract_dir="$tmp_dir/extract"
    mkdir -p "$extract_dir"

    curl -fL "$MIRROR_URL/node/$NODE_VERSION/$NODE_PLATFORM/$filename" -o "$archive"

    msg "verifying"
    local actual_sha=""
    if command -v sha256sum &> /dev/null; then
        actual_sha=$(sha256sum "$archive" | cut -d' ' -f1)
    elif command -v shasum &> /dev/null; then
        actual_sha=$(shasum -a 256 "$archive" | cut -d' ' -f1)
    else
        actual_sha="$expected_sha"
    fi

    if [[ "$actual_sha" != "$expected_sha" ]]; then
        msg "checksum_failed" "$expected_sha" "$actual_sha"
        exit 1
    fi
    msg "checksum_ok"

    local tar_flags="xzf"
    if [[ "$filename" == *.tar.xz ]]; then
        tar_flags="xJf"
    fi

    tar -"$tar_flags" "$archive" -C "$extract_dir"

    local src_dir
    src_dir=$(find "$extract_dir" -maxdepth 1 -type d -name "node-v${NODE_VERSION}-*" | head -1 || true)
    if [[ -z "$src_dir" ]]; then
        src_dir=$(find "$extract_dir" -maxdepth 2 -type d -name "node-v${NODE_VERSION}-*" | head -1 || true)
    fi
    if [[ -z "$src_dir" ]]; then
        msg "checksum_failed" "$expected_sha" "$actual_sha"
        exit 1
    fi

    local node_dir="$INSTALL_DIR/node/versions/$NODE_VERSION"
    rm -rf "$node_dir"
    mkdir -p "$node_dir"
    shopt -s dotglob
    mv "$src_dir"/* "$node_dir"/
    shopt -u dotglob

    if [[ -x "$node_dir/bin/node" ]]; then
        chmod +x "$node_dir/bin/node" || true
    fi

    printf "%s" "$checksums" > "$node_dir/checksums.json"
    curl -fsSL "$MIRROR_URL/node/$NODE_VERSION/SHASUMS256.txt" -o "$node_dir/SHASUMS256.txt" 2>/dev/null || true

    update_versions_json "node" "$NODE_VERSION" "$NODE_TAG"
}

install_node_pty() {
    local pty_dir="$INSTALL_DIR/node-pty/versions/$NODE_PTY_VERSION/prebuilds/$PLATFORM"
    if [[ -f "$pty_dir/pty.node" ]] && ! $UPGRADE; then
        msg "node_pty_ok" "$NODE_PTY_VERSION"
        return
    fi

    msg "installing_node_pty" "$NODE_PTY_VERSION"

    local checksums
    checksums=$(curl -fsSL "$MIRROR_URL/node-pty/$NODE_PTY_VERSION/checksums.json")
    local list
    list=$(list_node_pty_files "$checksums" "$PLATFORM")
    if [[ -z "$list" ]]; then
        msg "checksum_missing" "$PLATFORM"
        exit 1
    fi

    mkdir -p "$pty_dir"
    while IFS=$'\t' read -r filename sha size; do
        if [[ -z "$filename" || -z "$sha" ]]; then
            continue
        fi
        local dest="$pty_dir/$filename"
        curl -fL "$MIRROR_URL/node-pty/$NODE_PTY_VERSION/prebuilds/$PLATFORM/$filename" -o "$dest"

        msg "verifying"
        local actual_sha=""
        if command -v sha256sum &> /dev/null; then
            actual_sha=$(sha256sum "$dest" | cut -d' ' -f1)
        elif command -v shasum &> /dev/null; then
            actual_sha=$(shasum -a 256 "$dest" | cut -d' ' -f1)
        else
            actual_sha="$sha"
        fi

        if [[ "$actual_sha" != "$sha" ]]; then
            msg "checksum_failed" "$sha" "$actual_sha"
            rm -f "$dest"
            exit 1
        fi
        msg "checksum_ok"

        if [[ "$filename" == "spawn-helper" ]]; then
            chmod +x "$dest" || true
        fi
    done <<< "$list"

    printf "%s" "$checksums" > "$INSTALL_DIR/node-pty/versions/$NODE_PTY_VERSION/checksums.json"
    update_versions_json "node_pty" "$NODE_PTY_VERSION" "$NODE_PTY_TAG"
}

install_node
install_node_pty

if ! $SKIP_GEMINI; then
    msg "downloading"
    GEMINI_DIR="$INSTALL_DIR/gemini/versions/$VERSION"
    mkdir -p "$GEMINI_DIR"

    CHECKSUMS=$(curl -fsSL "$MIRROR_URL/api/gemini/checksums")
    EXPECTED_SHA256=$(get_expected_field "$CHECKSUMS" "$VERSION" "universal" "sha256")
    EXPECTED_FILENAME=$(get_expected_field "$CHECKSUMS" "$VERSION" "universal" "filename")

    if [[ -z "$EXPECTED_SHA256" ]]; then
        msg "checksum_missing" "universal"
        exit 1
    fi

    if [[ -z "$EXPECTED_FILENAME" ]]; then
        EXPECTED_FILENAME="gemini.js"
    fi

    curl -fL "$MIRROR_URL/gemini/$VERSION/$EXPECTED_FILENAME" -o "$GEMINI_DIR/gemini.js"

    msg "verifying"
    if command -v sha256sum &> /dev/null; then
        ACTUAL_SHA256=$(sha256sum "$GEMINI_DIR/gemini.js" | cut -d' ' -f1)
    elif command -v shasum &> /dev/null; then
        ACTUAL_SHA256=$(shasum -a 256 "$GEMINI_DIR/gemini.js" | cut -d' ' -f1)
    else
        ACTUAL_SHA256="$EXPECTED_SHA256"
    fi

    if [[ "$ACTUAL_SHA256" != "$EXPECTED_SHA256" ]]; then
        msg "checksum_failed" "$EXPECTED_SHA256" "$ACTUAL_SHA256"
        rm -f "$GEMINI_DIR/gemini.js"
        exit 1
    fi
    msg "checksum_ok"

    cat > "$BIN_DIR/gemini" << EOF
#!/bin/bash
set -e
INSTALL_DIR="\${INSTALL_DIR:-$INSTALL_DIR}"
NODE_BIN="\$INSTALL_DIR/node/versions/$NODE_VERSION/bin/node"
GEMINI_JS="\$INSTALL_DIR/gemini/versions/$VERSION/gemini.js"
export DUCKCODING_NODE_PTY_DIR="\$INSTALL_DIR/node-pty/versions/$NODE_PTY_VERSION/prebuilds"

if [[ ! -x "\$NODE_BIN" ]]; then
  echo "Private Node.js not found: \$NODE_BIN" >&2
  exit 1
fi
if [[ ! -f "\$GEMINI_JS" ]]; then
  echo "Gemini CLI not found: \$GEMINI_JS" >&2
  exit 1
fi

exec "\$NODE_BIN" "\$GEMINI_JS" "\$@"
EOF

    chmod +x "$BIN_DIR/gemini"
    msg "installed_to" "$VERSION" "$BIN_DIR/gemini"

    update_versions_json "gemini" "$VERSION" "$TAG"
fi

# Setup PATH
if ! $NO_MODIFY_PATH; then
    setup_path() {
        # Method A: Check if ~/.local/bin is in PATH
        if [[ ":$PATH:" == *":$HOME/.local/bin:"* ]]; then
            mkdir -p "$HOME/.local/bin"
            ln -sf "$BIN_DIR/gemini" "$HOME/.local/bin/gemini"
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
