# TODO

- [x] Fix Clippy warnings (run `cargo clippy -- -D warnings`)
  - [x] `src/s3.rs`: redundant closure
- [x] Fix local storage mode persistence for `checksums.json` / `SHASUMS256.txt`
  - [x] `CacheManager::get_file_path` should not require the file to already exist
  - [x] Add regression tests for local mode file path building
- [x] Align installer platform naming (target triples) across:
  - [x] `src/config.rs`: `default_installer_platforms()`
  - [x] `config.toml.example` installer platforms
- [x] Harden high-risk HTTP surfaces
  - [x] Protect `POST /api/*/refresh` with an admin token
  - [x] Tighten CORS (avoid `allow_methods(Any)`; allow `Authorization` header for token auth)
  - [x] Avoid panics when building responses with dynamic headers (`Content-Disposition`, redirects)
- [x] Consistency fixes
  - [x] `claude-code/stable` should fall back to `latest` if `stable` tag is missing
  - [x] Handle `metadata.json` parse failures (warn + backup instead of silent reset)
  - [x] Make `metadata.json` writes atomic (temp file + rename)
- [x] Improve test coverage
  - [x] Test install script placeholder injection via HTTP endpoints
  - [x] Test `claude-code` stable fallback behavior
  - [x] Test refresh auth gating behavior (no network)
- [x] Correctness: make provider sync report failures when any tag fails
  - [x] Avoid marking sync as success in API status when some tags failed
- [ ] Deferred: dependency dedupe / feature-gating AWS+OSS stacks (tracked, not doing now)
  - [x] Trim AWS default features (disable `aws-config` SSO/credentials-process; disable `aws-sdk-s3` sigv4a)

## Backlog (Nice-to-have)

- [x] Performance: cache S3/OSS clients instead of rebuilding per request/provider call
  - [x] `src/storage_clients.rs`: create and reuse clients built once at startup
- [x] Performance: avoid cloning full `CacheMetadata` on every request
  - [x] `CacheManager::with_provider_metadata` + server hot paths migrated
- [x] Download UX: support Range requests / resumable downloads for local storage mode
  - [x] `tower_http::services::ServeFile` for local serving + Range test
- [x] Security/robustness: validate or escape `server.public_url` before injecting into scripts
  - [x] `config::normalize_public_url` + startup validation in `server::run`
- [x] Security: tighten CORS further (GET-only; refresh is not browser-facing)
- [x] Ops: add rate limiting for refresh endpoints (token is auth, not throttling)
  - [x] `server.refresh_min_interval_seconds` + per-provider in-memory throttle
- [x] Ops: expose sync status (last_success, last_error, durations) in JSON API for monitoring
  - [x] Persist to metadata.json and expose via `/api/*/info` `sync` field
- [x] Hygiene: reduce remaining `unwrap/expect` in non-test code paths where feasible
