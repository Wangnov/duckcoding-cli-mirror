# DuckCoding CLI Mirror 实现计划

## 项目概述

一个 Rust 实现的二进制镜像服务，提供 Claude Code、Codex、Gemini CLI 和 Node.js 的镜像下载。

## 需求确认

| 项目 | 来源 | 标签/版本 | 格式 |
|------|------|-----------|------|
| Claude Code | GCS | stable, latest | 二进制 |
| Codex | GitHub (openai/codex) | 稳定版 (可配置预发行版) | tar.gz, exe |
| Gemini CLI | GitHub (google-gemini/gemini-cli) | 稳定版 (可配置) | gemini.js |
| Node.js | nodejs.org | 24 LTS | tar.xz/zip |
| node-pty | npm + CI 构建 | 1.1.0 | prebuilds 目录 |

**平台支持**:
- Claude Code: darwin-x64, darwin-arm64, linux-x64, linux-arm64, linux-x64-musl, linux-arm64-musl, win32-x64
- Codex: darwin-x64, darwin-arm64, linux-x64, linux-arm64, linux-x64-musl, linux-arm64-musl, win32-x64, win32-arm64
- Gemini CLI: 单一 gemini.js（跨平台）
- Node.js 运行时: darwin-x64, darwin-arm64, linux-x64, linux-arm64, win32-x64, win32-arm64
- node-pty prebuilds: darwin-x64, darwin-arm64, linux-x64, linux-arm64, linux-x64-musl, linux-arm64-musl, win32-x64, win32-arm64

**配置**:
- 端口: 1357 (默认)
- 更新频率: 10 分钟 (默认，可配置)
- 历史版本: 最多 10 个
- SHA256 校验: 记录并提供查询
- public_url: 必须配置（安装脚本依赖）

---

## 第一阶段: Claude Code 镜像

### 1.1 项目结构

```
duckcoding-cli-mirror/
├── Cargo.toml
├── config.toml.example
├── src/
│   ├── main.rs
│   ├── config.rs          # 配置管理
│   ├── server.rs          # HTTP 服务器
│   ├── cache.rs           # 缓存管理 (历史版本、清理)
│   ├── error.rs           # 错误类型定义
│   └── providers/
│       ├── mod.rs
│       ├── claude_code.rs # Claude Code 提供者
│       ├── codex.rs       # Codex 提供者
│       ├── gemini.rs      # Gemini CLI 提供者
│       ├── node.rs        # Node.js 运行时提供者
│       └── node_pty.rs    # node-pty prebuild 提供者
├── scripts/               # 安装脚本 (通过 include_str! 嵌入)
│   ├── claude-code-install.sh
│   ├── claude-code-install.ps1
│   ├── claude-code-uninstall.sh
│   └── claude-code-uninstall.ps1
├── docs/
│   └── plan.md            # 实现计划
└── cache/                 # 缓存目录 (gitignore)
```

### 1.2 Claude Code 数据源

```
Base URL: https://storage.googleapis.com/claude-code-dist-86c565f3-f756-42ad-8dfa-d59b1c096819/claude-code-releases

获取版本:
  GET /{tag}  → 返回版本号 (如 "1.0.30")
  tag = "stable" | "latest"

获取 Manifest:
  GET /{version}/manifest.json → 包含各平台的 SHA256

下载二进制:
  GET /{version}/{platform}/claude      (Unix)
  GET /{version}/{platform}/claude.exe  (Windows)

平台标识:
  darwin-x64, darwin-arm64
  linux-x64, linux-arm64, linux-x64-musl, linux-arm64-musl
  win32-x64
```

### 1.3 缓存结构

```
cache/
└── claude-code/
    ├── tags/
    │   ├── stable           # 文本文件，内容为版本号
    │   └── latest
    ├── versions/
    │   └── 1.0.30/
    │       ├── manifest.json
    │       ├── darwin-x64/
    │       │   └── claude
    │       ├── linux-x64/
    │       │   └── claude
    │       └── ...
    └── metadata.json        # 记录所有版本、SHA256、下载时间
```

### 1.4 API 设计

```
# 版本信息 (纯文本，适合 shell 脚本)
GET /claude-code/stable        → 版本号文本
GET /claude-code/latest        → 版本号文本

# Manifest
GET /claude-code/{version}/manifest.json

# 二进制下载
GET /claude-code/{version}/{platform}/claude
GET /claude-code/{version}/{platform}/claude.exe

# 安装脚本
GET /claude-code/install.sh
GET /claude-code/install.ps1
GET /claude-code/uninstall.sh
GET /claude-code/uninstall.ps1
# 注意：未配置 public_url 时，install.* 接口返回 503

# Codex 版本信息 (纯文本)
GET /codex/stable
GET /codex/latest

# Codex 二进制/归档下载
GET /codex/{version}/{platform}/{filename}

# Codex 安装脚本
GET /codex/install.sh
GET /codex/install.ps1
GET /codex/uninstall.sh
GET /codex/uninstall.ps1

# JSON API (适合 Tauri APP)
GET  /api/claude-code/info     → JSON: 版本信息、平台、SHA256、文件大小
GET  /api/claude-code/versions → JSON: 所有缓存版本列表
GET  /api/claude-code/checksums → JSON: 所有 SHA256

# 管理接口
POST /api/claude-code/refresh  # 手动触发更新检查
POST /api/codex/refresh

# Codex JSON API
GET  /api/codex/info
GET  /api/codex/versions
GET  /api/codex/checksums

# Gemini CLI
GET /gemini/{tag}
GET /gemini/{version}/gemini.js
GET /gemini/install.sh
GET /gemini/install.ps1
GET /gemini/uninstall.sh
GET /gemini/uninstall.ps1

# Gemini JSON API
GET /api/gemini/info
GET /api/gemini/versions
GET /api/gemini/checksums
POST /api/gemini/refresh

# Node.js runtime
GET /node/{tag}
GET /node/{version}/{platform}/{filename}
GET /node/{version}/checksums.json
GET /node/{version}/SHASUMS256.txt

# Node JSON API
GET /api/node/info
GET /api/node/versions
GET /api/node/checksums
POST /api/node/refresh

# node-pty prebuilds
GET /node-pty/{tag}
GET /node-pty/{version}/prebuilds/{platform}/{filename}
GET /node-pty/{version}/checksums.json

# node-pty JSON API
GET /api/node-pty/info
GET /api/node-pty/versions
GET /api/node-pty/checksums
POST /api/node-pty/refresh
```

#### Tauri APP JSON API 响应示例

```json
// GET /api/claude-code/info
{
  "tags": {
    "latest": "1.0.30",
    "stable": "1.0.28"
  },
  "platforms": {
    "darwin-arm64": {
      "version": "1.0.30",
      "url": "/claude-code/1.0.30/darwin-arm64/claude",
      "sha256": "abc123...",
      "size": 52428800
    },
    "linux-x64": { ... },
    "win32-x64": { ... }
  },
  "updated_at": "2026-01-05T12:00:00Z"
}
```

### 1.5 核心依赖（以 Cargo.toml 为准）

```toml
[dependencies]
# 异步运行时
tokio = { version = "1", features = ["full"] }

# Web 框架与中间件
axum = "0.8"
tower-http = { version = "0.6", features = ["cors", "trace"] }

# HTTP 客户端
reqwest = { version = "0.12", features = ["json", "stream"] }

# 序列化
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
toml = "0.8"

# SHA256 校验
sha2 = "0.10"
hex = "0.4"

# 日志追踪
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }

# 时间处理
chrono = { version = "0.4", features = ["serde"] }

# 错误处理
thiserror = "2"
anyhow = "1"

# 异步流处理
futures = "0.3"
tokio-util = { version = "0.7", features = ["io"] }

# 命令行参数
clap = { version = "4", features = ["derive"] }

[dev-dependencies]
tempfile = "3"
tower = { version = "0.5", features = ["util"] }
hyper = { version = "1.0", features = ["full"] }
http-body-util = "0.1"
```

**Context7 状态**:
- 已确认：tokio / axum / reqwest（2026-01-06）
- 其余依赖需逐项通过 Context7 核对最新版本与用法

### 1.6 配置文件格式

```toml
[server]
port = 1357
host = "0.0.0.0"
# 公开访问地址（用于安装脚本中的 MIRROR_URL）
# 必须设置，否则安装脚本接口返回 503
public_url = "http://your-server-ip:1357"

[cache]
dir = "./cache"
max_versions = 10

[update]
interval_minutes = 10
enabled = true

[claude_code]
enabled = true
tags = ["stable", "latest"]
platforms = [
    "darwin-x64", "darwin-arm64",
    "linux-x64", "linux-arm64",
    "linux-x64-musl", "linux-arm64-musl",
    "win32-x64"
]

[codex]
enabled = true
tags = ["stable", "latest"]
include_prerelease = false
repo = "openai/codex"
platforms = [
    "darwin-x64", "darwin-arm64",
    "linux-x64", "linux-arm64",
    "linux-x64-musl", "linux-arm64-musl",
    "win32-x64", "win32-arm64"
]

[gemini]
enabled = true
tags = ["stable", "latest"]
repo = "google-gemini/gemini-cli"
include_prerelease = false

[node]
enabled = true
tags = ["latest"]
platforms = [
    "darwin-x64", "darwin-arm64",
    "linux-x64", "linux-arm64",
    "win32-x64", "win32-arm64"
]

[node_pty]
enabled = true
tags = ["latest"]
platforms = [
    "darwin-x64", "darwin-arm64",
    "linux-x64", "linux-arm64",
    "linux-x64-musl", "linux-arm64-musl",
    "win32-x64", "win32-arm64"
]
```

### 1.7 安装脚本特性

- **中英文国际化**: 自动检测系统语言 (`$LANG` / `Get-UICulture`)，显示对应语言的提示信息
- **下载进度显示**: 显示下载百分比和速度
- **智能版本检测**: 已安装最新版本时跳过下载，使用 `--upgrade` 强制重新下载
- **PATH 配置**: 混合方案（优先 ~/.local/bin 符号链接，否则修改 shell 配置）
- **Manifest 解析**: 优先使用 `jq` 或 `python`，无依赖时回退 `grep`
- **Codex 安装脚本**: 使用 `/api/codex/checksums` 校验下载资产

### 1.8 实现步骤

1. **初始化项目** - Cargo.toml + 基础结构
2. **配置模块** - config.rs (TOML + 环境变量)
3. **缓存模块** - cache.rs (文件管理、版本清理)
4. **Claude Code Provider** - 下载、校验、存储逻辑
5. **HTTP 服务器** - axum 路由
6. **定时任务** - 后台更新检查
7. **安装脚本生成** - 动态生成 install.sh
8. **测试** - 单元测试 + 集成测试

### 1.9 Build 前质量门槛

- **必须通过**: `cargo fmt`
- **必须通过**: `cargo clippy -- -D warnings`

---

## 第二阶段: Codex 镜像 (GitHub Release API)

### 2.1 数据源

- Release API: `https://api.github.com/repos/openai/codex/releases`
- Tag 解析:
  - `stable`: 选最新非 prerelease 的 release
  - `latest`: 根据 `include_prerelease` 决定是否允许 prerelease
- 版本号使用 `tag_name`
- 资产命名（示例）:
  - `codex-x86_64-apple-darwin.tar.gz`
  - `codex-aarch64-apple-darwin.tar.gz`
  - `codex-x86_64-unknown-linux-gnu.tar.gz`
  - `codex-x86_64-unknown-linux-musl.tar.gz`
  - `codex-x86_64-pc-windows-msvc.exe`

### 2.2 缓存结构

```
cache/
└── codex/
    ├── tags/
    │   ├── stable
    │   └── latest
    ├── versions/
    │   └── rust-v0.78.0-alpha.12/
    │       ├── darwin-arm64/
    │       │   └── codex-aarch64-apple-darwin.tar.gz
    │       ├── win32-x64/
    │       │   └── codex-x86_64-pc-windows-msvc.exe
    │       └── ...
    └── metadata.json
```

### 2.3 SHA256 校验

- 优先使用 Release asset 的 `digest` 字段（若存在 `sha256:` 前缀）
- 若无 digest，使用下载时计算的 SHA256 作为记录值
- 安装脚本通过 `/api/codex/checksums` 取值并校验

### 2.4 GitHub API 频率限制与缓存策略

- 默认同步每个 tag 会请求 Release 列表，版本同步还会请求 `/releases/tags/{tag}`。
- 建议: 在一次同步周期内复用 Release 列表（内存缓存），减少重复请求。
- 建议: 支持 `GITHUB_TOKEN`，避免匿名配额不足。
- 可选优化: 使用 ETag/If-None-Match 缓存 Release 列表，响应 304 时跳过解析。

---

## 后续阶段

- **阶段三**: Gemini CLI 镜像
- **阶段四**: Node.js 运行时镜像 + node-pty prebuilds
- **阶段五**: Web 管理界面 (可选)

---

## 关键实现细节

### 版本清理逻辑

```rust
// 保留规则:
// 1. 始终保留当前 stable 和 latest 指向的版本
// 2. 按下载时间倒序，保留前 max_versions 个
// 3. 删除时移除整个版本目录
```

### SHA256 校验流程

```
1. 下载时: 流式写盘并计算 SHA256
2. 校验来源:
   - Claude Code: manifest.json 中的 checksum
   - Codex: Release asset digest（若存在），否则使用下载计算值
3. 全平台校验通过才更新版本元数据与 tag
4. 任一平台失败则不更新 tag，保留旧版本
5. 记录到 metadata.json 供查询
```

### 同步与并发策略

- 启动时立即执行一次同步
- 定时同步按 interval 执行，避免与启动同步重复触发
- 手动 refresh 与定时同步互斥，避免并发写入

### 环境变量覆盖

```
MIRROR_PORT=1357
MIRROR_HOST=0.0.0.0
MIRROR_PUBLIC_URL=http://your-server-ip:1357  # 安装脚本使用的公开地址
MIRROR_CACHE_DIR=./cache
MIRROR_UPDATE_INTERVAL=10
MIRROR_CLAUDE_CODE_ENABLED=true
MIRROR_CODEX_ENABLED=true
MIRROR_CODEX_REPO=openai/codex
MIRROR_CODEX_INCLUDE_PRERELEASE=false
MIRROR_GEMINI_ENABLED=true
MIRROR_GEMINI_REPO=google-gemini/gemini-cli
MIRROR_GEMINI_INCLUDE_PRERELEASE=false
MIRROR_NODE_ENABLED=true
MIRROR_NODE_PTY_ENABLED=true
GITHUB_TOKEN=xxx   # 可选：提高 GitHub API 配额
```

---

## 安装脚本设计

### 核心特性

1. **中英文国际化**
   - Bash: 检测 `$LANG` 或 `$LC_ALL` 环境变量
   - PowerShell: 检测 `Get-UICulture`
   - 中文 (`zh*`) 显示中文，其他显示英文

2. **智能版本检测**
   - 首次安装: 下载并安装
   - 已是最新: 跳过下载，显示 "Already up to date"
   - `--upgrade`: 强制重新下载

3. **下载进度显示**
   - 显示下载百分比、已下载大小、下载速度、剩余时间

### 脚本列表 (独立脚本)

| 脚本 | 用途 |
|------|------|
| `/claude-code/install.sh` | Linux/macOS 安装 Claude Code |
| `/claude-code/install.ps1` | Windows 安装 Claude Code |
| `/codex/install.sh` | Linux/macOS 安装 Codex |
| `/codex/install.ps1` | Windows 安装 Codex |
| `/gemini/install.sh` | 预留（未实现） |
| `/gemini/install.ps1` | 预留（未实现） |

### 安装目录结构

```
~/.duckcoding/
├── bin/
│   ├── claude          # Claude Code 二进制 (或 claude.exe)
│   ├── codex           # Codex 二进制 (或 codex.exe)
│   ├── gemini          # Gemini 启动脚本
│   └── node            # 私有 Node.js 二进制
├── lib/
│   └── gemini.js       # Gemini CLI JS 文件
└── versions.json       # 已安装版本记录
```

### Linux/macOS 安装脚本 (install.sh)

实际实现见: `scripts/claude-code-install.sh` 与 `scripts/codex-install.sh`

**主要流程:**
1. 检测语言环境，设置国际化消息
2. 解析命令行参数
3. 检测平台和架构
4. 获取目标版本号
5. 检查本地已安装版本，决定是否跳过下载
6. 下载二进制（显示进度和速度）
7. 保存版本信息到 `versions.json`
8. 配置 PATH（符号链接或修改 shell 配置）

### Windows 安装脚本 (install.ps1)

实际实现见: `scripts/claude-code-install.ps1` 与 `scripts/codex-install.ps1`

**主要流程:**
1. 检测系统语言，设置国际化消息
2. 获取目标版本号
3. 检查本地已安装版本，决定是否跳过下载
4. 检测 claude.exe 是否运行，提示关闭或强制终止
5. 下载二进制（显示进度）
6. 保存版本信息到 `versions.json`
7. 配置用户级 PATH 环境变量

### 使用方式

```bash
# Linux/macOS 安装
curl -fsSL http://mirror:1357/claude-code/install.sh | bash

# Linux/macOS 更新
curl -fsSL http://mirror:1357/claude-code/install.sh | bash -s -- --upgrade

# Linux/macOS 卸载
curl -fsSL http://mirror:1357/claude-code/uninstall.sh | bash

# Linux/macOS 安装指定版本
curl -fsSL http://mirror:1357/claude-code/install.sh | bash -s -- --version 1.0.28

# Windows PowerShell 安装
irm http://mirror:1357/claude-code/install.ps1 | iex

# Windows PowerShell 更新
irm http://mirror:1357/claude-code/install.ps1 | iex -ArgumentList '--upgrade'
```

---

## 更新和卸载设计

### 更新流程

安装脚本内置智能更新逻辑:
1. 检查 `~/.duckcoding/versions.json` 获取当前版本
2. 获取镜像服务器最新版本
3. 版本相同 → 跳过下载，显示 "Already up to date"
4. 版本不同 → 下载新版本并替换
5. `--upgrade` 参数 → 强制重新下载

### Windows 进程检测

PowerShell 脚本内置进程检测:
- 检测 `claude.exe` 是否运行
- 如果运行中: 提示关闭或使用 `-Force` 强制终止
- 实现见: `scripts/claude-code-install.ps1`

### 卸载脚本

实际实现见:
- Linux/macOS: `scripts/claude-code-uninstall.sh`
- Windows: `scripts/claude-code-uninstall.ps1`

**卸载流程:**
1. 删除 `~/.duckcoding` 目录
2. 清理 shell 配置文件中的 PATH 设置
3. 删除 `~/.local/bin` 中的符号链接

### versions.json 结构

```json
{
  "claude": {
    "version": "2.0.76",
    "tag": "latest",
    "installed_at": "2026-01-06T12:00:00Z"
  }
}
```

---

## 安装脚本完整选项

### Linux/macOS (install.sh)

```
选项:
  --tag latest|stable       指定版本标签 (默认: latest)
  --version X.Y.Z          安装指定版本
  --upgrade                 升级到最新版本
  --no-modify-path         不修改 PATH 配置
  --check                  仅检查更新，不安装

环境变量:
  MIRROR_URL               镜像服务器地址 (默认: http://localhost:1357)
  HTTP_PROXY / HTTPS_PROXY 代理设置
```

### Windows (install.ps1)

```
选项:
  -Tag latest|stable        指定版本标签 (默认: latest)
  -Version X.Y.Z           安装指定版本
  -Upgrade                  升级到最新版本
  -NoModifyPath            不修改 PATH 配置
  -Check                   仅检查更新，不安装
  -Force                   强制终止运行中的进程

环境变量:
  MIRROR_URL               镜像服务器地址
  HTTP_PROXY / HTTPS_PROXY 代理设置
```

---

## 实现状态

### 第一阶段: Claude Code 镜像 ✅ 已完成

| 功能 | 状态 |
|------|------|
| 安装脚本 (install.sh/ps1) | ✅ 已实现 |
| 卸载脚本 (uninstall.sh/ps1) | ✅ 已实现 |
| PATH 配置 (混合方案) | ✅ 已实现 |
| Windows 进程检测 | ✅ 已实现 |
| 版本检查 + --upgrade | ✅ 已实现 |
| --version 指定版本 | ✅ 已实现 |
| JSON API (/api/claude-code/info) | ✅ 已实现 |
| JSON API (/api/claude-code/checksums) | ✅ 已实现 |
| 定时后台更新 | ✅ 已实现 |
| 中英文国际化 | ✅ 已实现 |
| 下载进度 + 速度显示 | ✅ 已实现 |
| public_url 配置 | ✅ 已实现 |
| 版本清理 (max_versions) | ✅ 已实现 |
| SHA256 校验 (服务端) | ✅ 已实现 |
| SHA256 校验 (客户端安装脚本) | ✅ 已实现 |
| HTTP_PROXY 代理支持 | ✅ 已实现 |
| 单元测试 + 集成测试 | ✅ 已实现（34 tests） |

### 第二阶段: Codex 镜像 ✅ 已完成（核心）

| 功能 | 状态 |
|------|------|
| Release 同步（stable/latest） | ✅ 已实现 |
| 资产下载 + SHA256 校验 | ✅ 已实现 |
| JSON API (/api/codex/info, /api/codex/checksums) | ✅ 已实现 |
| 安装脚本 (install.sh/ps1) | ✅ 已实现 |
| 卸载脚本 (uninstall.sh/ps1) | ✅ 已实现 |
| 定时后台更新 | ✅ 已实现 |

### 第三阶段: Gemini CLI + Node.js/node-pty 镜像 ✅ 已完成

| 功能 | 状态 |
|------|------|
| Gemini CLI Release 同步 | ✅ 已实现 |
| Gemini CLI 安装脚本 (install.sh/ps1) | ✅ 已实现 |
| Gemini CLI 卸载脚本 (uninstall.sh/ps1) | ✅ 已实现 |
| JSON API (/api/gemini/*) | ✅ 已实现 |
| Node.js 本地缓存验证 | ✅ 已实现 |
| Node.js 下载接口 | ✅ 已实现 |
| JSON API (/api/node/*) | ✅ 已实现 |
| node-pty 本地缓存验证 | ✅ 已实现 |
| node-pty 下载接口 | ✅ 已实现 |
| JSON API (/api/node-pty/*) | ✅ 已实现 |

> **说明**: Node.js 和 node-pty 采用本地缓存验证模式，文件通过 CI 构建后上传到服务器，Provider 仅验证文件完整性。这些接口供 Gemini 安装脚本和 Tauri 客户端使用。

### 后续阶段

| 阶段 | 内容 | 状态 |
|------|------|------|
| 阶段二 | Codex 镜像 (GitHub Release API) | ✅ 已实现 |
| 阶段三 | Gemini CLI + Node.js/node-pty 镜像 | ✅ 已实现 |
| 阶段四 | Web 管理界面 | ⏳ 可选 |
