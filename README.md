# DuckCoding CLI Mirror

一个 Rust 实现的 CLI 工具镜像服务，为 [DuckCoding APP](https://github.com/DuckCoding-dev/DuckCoding) 提供后端支持，同时提供独立的安装脚本供用户手动安装。

## 支持的工具

| 工具 | 来源 | 说明 |
|------|------|------|
| **Claude Code** | Google Cloud Storage | Anthropic 官方 CLI |
| **Codex** | GitHub Releases | OpenAI 官方 CLI |
| **Gemini CLI** | GitHub Releases | Google 官方 CLI |
| **Node.js** | 从官方拉取后缓存 | Gemini CLI 私有运行时 |
| **node-pty** | CI预构建后缓存 | 终端模拟预编译库 |

## 快速开始

### 通过安装脚本安装 CLI 工具

**Claude Code**

```bash
# Linux / macOS
curl -fsSL https://mirror.duckcoding.com/claude-code/install.sh | bash

# Windows (PowerShell)
irm https://mirror.duckcoding.com/claude-code/install.ps1 | iex
```

**Codex**

```bash
# Linux / macOS
curl -fsSL https://mirror.duckcoding.com/codex/install.sh | bash

# Windows (PowerShell)
irm https://mirror.duckcoding.com/codex/install.ps1 | iex
```

**Gemini CLI**

```bash
# Linux / macOS
curl -fsSL https://mirror.duckcoding.com/gemini/install.sh | bash

# Windows (PowerShell)
irm https://mirror.duckcoding.com/gemini/install.ps1 | iex
```

### 安装脚本选项

```bash
# 安装指定版本
curl -fsSL .../install.sh | bash -s -- --version 2.0.67

# 安装指定标签 (stable/latest)
curl -fsSL .../install.sh | bash -s -- --tag stable

# 强制升级（即使已是最新版本）
curl -fsSL .../install.sh | bash -s -- --upgrade

# 仅检查更新
curl -fsSL .../install.sh | bash -s -- --check

# 不修改 PATH
curl -fsSL .../install.sh | bash -s -- --no-modify-path
```

### 卸载

```bash
# Linux / macOS
curl -fsSL https://mirror.duckcoding.com/claude-code/uninstall.sh | bash

# Windows (PowerShell)
irm https://mirror.duckcoding.com/claude-code/uninstall.ps1 | iex
```

## 部署镜像服务

### 前置要求

- Rust 1.85+
- 可选：`GITHUB_TOKEN` 环境变量（提高 GitHub API 配额）

### 编译运行

```bash
# 克隆项目
git clone https://github.com/wangnov/duckcoding-cli-mirror.git
cd duckcoding-cli-mirror

# 编译
cargo build --release

# 运行
./target/release/duckcoding-cli-mirror --config config.toml
```

### 服务端部署与缓存准备（不主动拉取 GitHub）

为避免服务端向 GitHub 发起下载请求，以下三类缓存需要在部署时由运维**从本地/CI 拉取**并同步到服务器：

- **installer**：由 release 构建产生（cargo-dist）
- **node**：由 `node-runtime.yml` 构建产生
- **node-pty**：由 `node-pty-prebuilds.yml` 构建产生

建议流程（在本地执行，服务器仅接收 rsync）：

1) 触发 CI 并下载产物（示例）：

```bash
# node runtime
gh run list -w node-runtime.yml -L 1
gh run download <RUN_ID> -D /tmp/node-runtime

# node-pty
gh run list -w node-pty-prebuilds.yml -L 1
gh run download <RUN_ID> -D /tmp/node-pty

# installer：从 GitHub Release 下载对应版本资产
gh release download <TAG> -D /tmp/installer
```

2) 同步到服务器 cache（示例）：

```bash
# node
rsync -av /tmp/node-runtime/node-<VERSION>-runtime/ \
  <USER>@<SERVER>:/home/<USER>/duckcoding-cli-mirror/cache/node/versions/<VERSION>/

# node-pty
rsync -av /tmp/node-pty/node-pty-<VERSION>-prebuilds/ \
  <USER>@<SERVER>:/home/<USER>/duckcoding-cli-mirror/cache/node-pty/versions/<VERSION>/

# installer
rsync -av /tmp/installer/ \
  <USER>@<SERVER>:/home/<USER>/duckcoding-cli-mirror/cache/installer/versions/<VERSION>/
```

3) 写入 tag 并刷新缓存：

```bash
ssh <USER>@<SERVER> <<'EOF'
set -e
echo "<NODE_VERSION>" > /home/<USER>/duckcoding-cli-mirror/cache/node/tags/latest
echo "<NODE_PTY_VERSION>" > /home/<USER>/duckcoding-cli-mirror/cache/node-pty/tags/latest
echo "<INSTALLER_VERSION>" > /home/<USER>/duckcoding-cli-mirror/cache/installer/tags/latest
curl -fsS -X POST http://127.0.0.1:1357/api/node/refresh
curl -fsS -X POST http://127.0.0.1:1357/api/node-pty/refresh
curl -fsS -X POST http://127.0.0.1:1357/api/installer/refresh
EOF
```

> 说明：服务端不会主动从 GitHub 拉取 installer/node/node-pty，部署时需保证这些缓存已被同步到 `cache/`。

### 配置文件

复制示例配置并修改：

```bash
cp config.toml.example config.toml
```

关键配置项：

```toml
[server]
port = 1357
host = "0.0.0.0"
# 必须配置：安装脚本中使用的公开地址
public_url = "https://mirror.duckcoding.com"

[cache]
dir = "./cache"
max_versions = 10      # 保留的历史版本数

[update]
interval_minutes = 10  # 自动更新检查间隔
enabled = true
```

### 环境变量

| 变量 | 说明 | 默认值 |
|------|------|--------|
| `MIRROR_PORT` | 服务端口 | 1357 |
| `MIRROR_HOST` | 监听地址 | 0.0.0.0 |
| `MIRROR_PUBLIC_URL` | 公开访问地址 | - |
| `MIRROR_CACHE_DIR` | 缓存目录 | ./cache |
| `MIRROR_UPDATE_INTERVAL` | 更新间隔（分钟） | 10 |
| `GITHUB_TOKEN` | GitHub API Token | - |

## API 文档

### 版本信息

```
GET /{tool}/{tag}          # 获取版本号 (stable/latest)
GET /{tool}/{version}/...  # 下载指定版本
```

### JSON API

```
GET /api/{tool}/info       # 版本信息、平台、SHA256、文件大小
GET /api/{tool}/versions   # 所有缓存版本列表
GET /api/{tool}/checksums  # 所有 SHA256 校验值
POST /api/{tool}/refresh   # 手动触发更新
```

其中 `{tool}` 可以是：`claude-code`、`codex`、`gemini`、`node`、`node-pty`

### 示例响应

```json
// GET /api/claude-code/info
{
  "tags": {
    "latest": "2.0.76",
    "stable": "2.0.67"
  },
  "platforms": {
    "darwin-arm64": {
      "version": "2.0.76",
      "url": "/claude-code/2.0.76/darwin-arm64/claude",
      "sha256": "abc123...",
      "size": 52428800
    }
  },
  "updated_at": "2026-01-07T12:00:00Z"
}
```

## 平台支持

| 平台 | Claude Code | Codex | Gemini | Node.js | node-pty |
|------|:-----------:|:-----:|:------:|:-------:|:--------:|
| darwin-x64 | ✅ | ✅ | ✅ | ✅ | ✅ |
| darwin-arm64 | ✅ | ✅ | ✅ | ✅ | ✅ |
| linux-x64 | ✅ | ✅ | ✅ | ✅ | ✅ |
| linux-arm64 | ✅ | ✅ | ✅ | ✅ | ✅ |
| linux-x64-musl | ✅ | ✅ | - | - | ✅ |
| linux-arm64-musl | ✅ | ✅ | - | - | ✅ |
| win32-x64 | ✅ | ✅ | ✅ | ✅ | ✅ |
| win32-arm64 | - | ✅ | ✅ | ✅ | ✅ |

## 安装脚本特性

- **中英文国际化**：自动检测系统语言
- **智能版本检测**：已是最新版本时跳过下载
- **SHA256 校验**：下载后自动验证文件完整性
- **进度显示**：显示下载进度和速度
- **PATH 配置**：自动配置环境变量
- **代理支持**：支持 `HTTP_PROXY` / `HTTPS_PROXY`

## 缓存结构

```
cache/
├── claude-code/
│   ├── tags/
│   │   ├── stable          # 版本号文本
│   │   └── latest
│   ├── versions/
│   │   └── 2.0.76/
│   │       ├── manifest.json
│   │       ├── darwin-arm64/
│   │       │   └── claude
│   │       └── ...
│   └── metadata.json
├── codex/
├── gemini/
├── node/
└── node-pty/
```

## 开发

```bash
# 运行测试
cargo test

# 代码检查
cargo clippy -- -D warnings

# 格式化
cargo fmt
```

## CI/CD

项目包含以下 GitHub Actions 工作流：

### 安装脚本端到端测试 (`install-tests.yml`)

在多平台上测试安装脚本的完整流程：

| 平台 | 运行环境 |
|------|----------|
| macOS x64 | `macos-15-intel` |
| macOS ARM64 | `macos-latest` |
| Windows x64 | `windows-latest` |
| Linux x64 (glibc) | Ubuntu 24.04 容器 |
| Linux ARM64 (glibc) | Ubuntu 24.04 容器 (QEMU) |
| Linux x64 (musl) | Alpine 3.19 容器 |
| Linux ARM64 (musl) | Alpine 3.19 容器 (QEMU) |

触发条件：推送或 PR 修改 `scripts/**` 或工作流文件

### node-pty 预构建 (`node-pty-prebuilds.yml`)

手动触发，构建 node-pty 的跨平台预编译库：
- 从 npm 提取 darwin/win32 预构建
- 在 Docker 容器中交叉编译 Linux glibc/musl 版本

### Node.js 运行时下载 (`node-runtime.yml`)

手动触发，从 nodejs.org 下载指定版本的 Node.js 运行时并生成校验文件。

## 架构

```
┌─────────────────┐     ┌──────────────────┐
│  DuckCoding APP │────>│                  │
│    (Tauri)      │     │  CLI Mirror      │
└─────────────────┘     │    Service       │
                        │                  │
┌─────────────────┐     │  ┌────────────┐  │     ┌─────────────┐
│  Install Script │────>│  │  Providers │──│────>│ Upstream    │
│  (curl | bash)  │     │  └────────────┘  │     │ (GCS/GitHub)│
└─────────────────┘     │        │         │     └─────────────┘
                        │        v         │
                        │  ┌────────────┐  │
                        │  │   Cache    │  │
                        │  └────────────┘  │
                        └──────────────────┘
```

## License

AGPL-3.0
