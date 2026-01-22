# DuckCoding CLI Mirror

一个 Rust 实现的 CLI 工具镜像服务，为 [DuckCoding APP](https://github.com/DuckCoding-dev/DuckCoding) 提供后端支持，同时提供独立的安装脚本供用户手动安装。

## 支持的工具

| 工具 | 来源 | 说明 |
|------|------|------|
| **Claude Code** | Google Cloud Storage | Anthropic 官方 CLI |
| **Codex** | GitHub Releases | OpenAI 官方 CLI |
| **Gemini CLI** | GitHub Releases | Google 官方 CLI |
| **Installer** | GitHub Releases | 安装器二进制 |
| **Node.js** | GitHub Releases（本仓库） | Gemini CLI 私有运行时 |
| **node-pty** | GitHub Releases（本仓库） | 终端模拟预编译库 |

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

# 安装指定标签 (stable/latest/custom)
curl -fsSL .../install.sh | bash -s -- --tag stable

# 安装自定义标签（需在服务端配置 tags）
curl -fsSL .../install.sh | bash -s -- --tag my-tag

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

### 服务端部署与缓存准备（Release 上游驱动）

installer/node/node-pty 现已统一改为 **GitHub Release 上游驱动**，服务端会主动从 Release 拉取并同步到 R2/OSS，
无需再手动 rsync 到 `cache/`。

推荐流程（发布到 Release 后由服务端拉取）：

1) **node runtime**：触发 `node-runtime.yml`（workflow_dispatch）
   - 自动创建/更新 Release：`node-v<version>`
   - Release 资产包含：`checksums.json`、`SHASUMS256.txt`、各平台 `node-v*.tar.xz/zip`

2) **node-pty**：触发 `node-pty-prebuilds.yml`（workflow_dispatch）
   - 自动创建/更新 Release：`node-pty-v<version>`
   - Release 资产包含：`checksums.json`、`<platform>--pty.node`、`<platform>--spawn-helper`

3) **installer**：正常走 cargo-dist 的 Release 流程（tag 触发，随后由 `installer-release-assets` 生成并补充 `checksums.json`）

4) 让服务端更新缓存（可选）：

```bash
# 注意：refresh 接口需要配置 server.refresh_token（或环境变量 MIRROR_REFRESH_TOKEN）
# 然后带上 Authorization: Bearer
curl -fsS -X POST -H "Authorization: Bearer <token>" http://127.0.0.1:1357/api/node/refresh
curl -fsS -X POST -H "Authorization: Bearer <token>" http://127.0.0.1:1357/api/node-pty/refresh
curl -fsS -X POST -H "Authorization: Bearer <token>" http://127.0.0.1:1357/api/installer/refresh
```

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
# 可选：refresh 接口鉴权/节流
# refresh_token = "replace-with-a-strong-random-token"
# refresh_min_interval_seconds = 10  # 0 表示禁用节流

[http]
connect_timeout_seconds = 10   # 连接超时
request_timeout_seconds = 3600 # 请求总超时（0 表示不限制）

[cache]
dir = "./cache"
max_versions = 10      # 保留的历史版本数

[update]
interval_minutes = 10  # 自动更新检查间隔
enabled = true

[codex]
tags = ["stable", "latest", "my-tag"]  # 自定义 tag 支持
```

### 环境变量

| 变量 | 说明 | 默认值 |
|------|------|--------|
| `MIRROR_PORT` | 服务端口 | 1357 |
| `MIRROR_HOST` | 监听地址 | 0.0.0.0 |
| `MIRROR_PUBLIC_URL` | 公开访问地址 | - |
| `MIRROR_CACHE_DIR` | 缓存目录 | ./cache |
| `MIRROR_UPDATE_INTERVAL` | 更新间隔（分钟） | 10 |
| `MIRROR_HTTP_CONNECT_TIMEOUT` | 连接超时（秒） | 10 |
| `MIRROR_HTTP_TIMEOUT` | 请求总超时（秒） | 3600 |
| `MIRROR_REFRESH_TOKEN` | 刷新接口 Token（POST /api/*/refresh，需要 Authorization: Bearer） | - |
| `MIRROR_REFRESH_MIN_INTERVAL` | refresh 接口节流间隔（秒，0 表示禁用） | 10 |
| `GITHUB_TOKEN` | GitHub API Token | - |

## API 文档

### 版本信息

```
GET /{tool}/{tag}          # 获取版本号（支持自定义 tag；需在配置中启用）
GET /{tool}/{version}/...  # 下载指定版本
```

### JSON API

```
GET /api/{tool}/info       # 版本信息、平台、SHA256、文件大小
GET /api/{tool}/versions   # 所有缓存版本列表
GET /api/{tool}/checksums  # 所有 SHA256 校验值
POST /api/{tool}/refresh   # 手动触发更新
```

其中 `{tool}` 可以是：`claude-code`、`codex`、`gemini`、`installer`、`node`、`node-pty`

说明：
- `stable` 未配置时会回退到 `latest`
- `/api/*/info` 会额外返回 `sync` 字段（最近一次同步成功/失败时间、耗时、错误摘要）

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
