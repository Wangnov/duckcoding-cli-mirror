use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose};
use bytes::Bytes;
use futures::{Stream, StreamExt};
use hmac::{Hmac, Mac};
use oss_sdk_rs::errors::OSSError;
use oss_sdk_rs::object::ObjectAPI;
use oss_sdk_rs::oss::OSS;
use sha1::Sha1;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{debug, info, warn};

use crate::config::OssConfig;

type HmacSha1 = Hmac<Sha1>;
const APPEND_CHUNK_SIZE: usize = 8 * 1024 * 1024;

pub fn presign_get_url(config: &OssConfig, object_key: &str) -> Result<String> {
    presign_url(config, "GET", object_key, None)
}

pub async fn put_bytes(
    config: &OssConfig,
    object_key: &str,
    content_type: &str,
    body: Vec<u8>,
) -> Result<()> {
    let client = oss_client(config)?;
    let key = object_key_with_prefix(config, object_key);
    let headers = content_type_headers(content_type);
    client
        .put_object(
            body.as_slice(),
            key,
            headers,
            None::<std::collections::HashMap<String, Option<String>>>,
        )
        .await
        .map_err(|err| {
            anyhow::anyhow!(
                "Failed to upload object {}: {}",
                object_key,
                format_oss_error(&err)
            )
        })?;
    Ok(())
}

#[allow(dead_code)]
pub async fn get_object_bytes(config: &OssConfig, object_key: &str) -> Result<Bytes> {
    let client = oss_client(config)?;
    let key = object_key_with_prefix(config, object_key);
    let content = client
        .get_object(
            key,
            None::<std::collections::HashMap<String, String>>,
            None::<std::collections::HashMap<String, Option<String>>>,
        )
        .await
        .map_err(|err| {
            anyhow::anyhow!(
                "Failed to fetch object {}: {}",
                object_key,
                format_oss_error(&err)
            )
        })?;
    Ok(content)
}

pub async fn upload_stream<S>(
    config: &OssConfig,
    object_key: &str,
    content_type: &str,
    total_size: Option<u64>,
    stream: S,
) -> Result<UploadResult>
where
    S: Stream<Item = Result<Bytes, reqwest::Error>> + Send + Unpin + 'static,
{
    let log_every_bytes = progress_log_interval(total_size);
    let total_hint = total_size
        .filter(|t| *t > 0)
        .map(|t| format!(" (total: {})", format_bytes(t)))
        .unwrap_or_default();
    info!(
        "Downloading upstream stream for OSS upload {}{}",
        object_key, total_hint
    );
    let temp_path = temp_path_for(object_key)?;
    let mut file = tokio::fs::File::create(&temp_path).await?;
    let mut hasher = Sha256::new();
    let mut size: u64 = 0;
    let mut last_logged: u64 = 0;
    let download_started = Instant::now();

    let upload_result = async {
        let mut upstream = Box::pin(stream);
        while let Some(chunk) = upstream.next().await {
            let chunk = chunk?;
            size += chunk.len() as u64;
            hasher.update(&chunk);
            file.write_all(&chunk).await?;
            if size.saturating_sub(last_logged) >= log_every_bytes {
                let speed = format_rate(size, download_started.elapsed());
                if let Some(total) = total_size.filter(|t| *t > 0) {
                    let pct = (size as f64 / total as f64) * 100.0;
                    debug!(
                        "Downloaded {}/{} ({:.1}%), {} for OSS object {}",
                        format_bytes(size),
                        format_bytes(total),
                        pct.min(100.0),
                        speed,
                        object_key
                    );
                } else {
                    debug!(
                        "Downloaded {} ({}) for OSS object {}",
                        format_bytes(size),
                        speed,
                        object_key
                    );
                }
                last_logged = size;
            }
        }
        file.flush().await?;
        Ok::<_, anyhow::Error>(UploadResult {
            size,
            sha256: hex::encode(hasher.finalize()),
        })
    }
    .await;

    let upload_result = match upload_result {
        Ok(result) => result,
        Err(err) => {
            drop(file);
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err(err);
        }
    };

    info!(
        "Downloaded upstream to temp file for OSS object {} ({} bytes, sha256: {})",
        object_key, upload_result.size, upload_result.sha256
    );

    drop(file);

    info!("Uploading to OSS object {}", object_key);
    let upload_started = Instant::now();
    let upload_result = match upload_file(config, object_key, content_type, &temp_path).await {
        Ok(()) => upload_result,
        Err(err) => {
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err(err);
        }
    };

    let _ = tokio::fs::remove_file(&temp_path).await;
    info!(
        "Uploaded OSS object {} ({} bytes) in {:?}",
        object_key,
        upload_result.size,
        upload_started.elapsed()
    );
    Ok(upload_result)
}

pub async fn delete_object(config: &OssConfig, object_key: &str) -> Result<()> {
    let client = oss_client(config)?;
    let key = object_key_with_prefix(config, object_key);
    client.delete_object(key).await.map_err(|err| {
        anyhow::anyhow!(
            "Failed to delete object {}: {}",
            object_key,
            format_oss_error(&err)
        )
    })?;
    Ok(())
}

pub async fn upload_file(
    config: &OssConfig,
    object_key: &str,
    content_type: &str,
    path: &PathBuf,
) -> Result<()> {
    let client = oss_client(config)?;
    let key = object_key_with_prefix(config, object_key);
    ensure_object_absent(&client, &key).await?;

    let total_size = tokio::fs::metadata(path)
        .await
        .map(|m| m.len())
        .unwrap_or(0);
    let mut file = tokio::fs::File::open(path).await?;
    let mut position: u64 = 0;
    let mut buffer = vec![0u8; APPEND_CHUNK_SIZE];
    let mut uploaded: u64 = 0;
    let mut last_logged: u64 = 0;
    const LOG_EVERY_BYTES: u64 = 64 * 1024 * 1024;

    loop {
        let read = file.read(&mut buffer).await?;
        if read == 0 {
            break;
        }

        position = append_chunk(&client, &key, content_type, position, &buffer[..read]).await?;
        uploaded += read as u64;
        if uploaded.saturating_sub(last_logged) >= LOG_EVERY_BYTES {
            info!(
                "Uploading OSS object {} ({} / {} bytes)",
                object_key, uploaded, total_size
            );
            last_logged = uploaded;
        }
    }

    if total_size > 0 {
        info!("Finished OSS upload {} ({} bytes)", object_key, total_size);
    }

    Ok(())
}

fn validate_config(config: &OssConfig) -> Result<()> {
    if config.endpoint.is_empty() {
        anyhow::bail!("OSS endpoint is not configured");
    }
    if config.bucket.is_empty() {
        anyhow::bail!("OSS bucket is not configured");
    }
    if config.access_key_id.is_empty() {
        anyhow::bail!("OSS access_key_id is not configured");
    }
    if config.access_key_secret.is_empty() {
        anyhow::bail!("OSS access_key_secret is not configured");
    }
    Ok(())
}

fn join_prefix(prefix: &str, key: &str) -> String {
    let prefix = prefix.trim_matches('/');
    if prefix.is_empty() {
        key.to_string()
    } else {
        format!("{}/{}", prefix, key)
    }
}

fn temp_path_for(object_key: &str) -> Result<PathBuf> {
    let mut name = object_key.replace('/', "_");
    if name.len() > 80 {
        name = name[name.len() - 80..].to_string();
    }
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("Failed to get time for temp path")?
        .as_nanos();
    Ok(std::env::temp_dir().join(format!("dc-mirror-{}-{}.tmp", name, ts)))
}

fn progress_log_interval(total_size: Option<u64>) -> u64 {
    let max_interval = 5 * 1024 * 1024;
    let min_interval = 512 * 1024;
    match total_size {
        Some(total) if total > 0 => {
            let by_percent = total / 20;
            let mut interval = by_percent.clamp(min_interval, max_interval);
            if total < min_interval {
                interval = total;
            }
            if interval == 0 {
                interval = min_interval;
            }
            interval
        }
        _ => max_interval,
    }
}

fn format_rate(bytes: u64, elapsed: Duration) -> String {
    let secs = elapsed.as_secs_f64();
    if secs <= 0.0 {
        return "0 B/s".to_string();
    }
    let per_sec = (bytes as f64 / secs).round().max(0.0) as u64;
    format!("{}/s", format_bytes(per_sec))
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut size = bytes as f64;
    let mut idx = 0usize;
    while size >= 1024.0 && idx < UNITS.len() - 1 {
        size /= 1024.0;
        idx += 1;
    }
    if idx == 0 {
        format!("{} {}", bytes, UNITS[idx])
    } else if size >= 100.0 {
        format!("{:.0} {}", size, UNITS[idx])
    } else if size >= 10.0 {
        format!("{:.1} {}", size, UNITS[idx])
    } else {
        format!("{:.2} {}", size, UNITS[idx])
    }
}

fn object_key_with_prefix(config: &OssConfig, object_key: &str) -> String {
    join_prefix(&config.prefix, object_key)
}

fn oss_client(config: &OssConfig) -> Result<OSS<'static>> {
    validate_config(config)?;
    if config.path_style {
        anyhow::bail!("oss-sdk-rs does not support path-style endpoints");
    }
    if config.security_token.is_some() {
        warn!(
            "OSS security_token is set but oss-sdk-rs does not support STS; token will be ignored"
        );
    }
    let endpoint = normalize_endpoint(config);
    Ok(OSS::new(
        config.access_key_id.clone(),
        config.access_key_secret.clone(),
        endpoint,
        config.bucket.clone(),
    ))
}

fn normalize_endpoint(config: &OssConfig) -> String {
    let endpoint = config.endpoint.trim();
    if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
        endpoint.to_string()
    } else {
        let scheme = if config.https { "https" } else { "http" };
        format!("{scheme}://{endpoint}")
    }
}

fn content_type_headers(content_type: &str) -> std::collections::HashMap<String, String> {
    let mut headers = std::collections::HashMap::new();
    headers.insert("content-type".to_string(), content_type.to_string());
    headers
}

fn format_oss_error(err: &OSSError) -> String {
    format!("{:?}", err)
}

async fn ensure_object_absent(client: &OSS<'_>, key: &str) -> Result<()> {
    if let Err(err) = client.delete_object(key).await {
        return Err(anyhow::anyhow!(
            "Failed to delete OSS object {}: {}",
            key,
            format_oss_error(&err)
        ));
    }
    Ok(())
}

async fn append_chunk(
    client: &OSS<'_>,
    key: &str,
    content_type: &str,
    position: u64,
    chunk: &[u8],
) -> Result<u64> {
    let headers = content_type_headers(content_type);
    let mut resources: HashMap<String, Option<String>> = HashMap::new();
    resources.insert("append".to_string(), None);
    resources.insert("position".to_string(), Some(position.to_string()));

    let next = client
        .append_object(chunk, key, headers, resources)
        .await
        .map_err(|err| {
            anyhow::anyhow!(
                "Failed to append OSS object {}: {}",
                key,
                format_oss_error(&err)
            )
        })?;

    Ok(next.unwrap_or(position + chunk.len() as u64))
}

fn encode_object_key(key: &str) -> String {
    key.split('/')
        .map(|segment| urlencoding::encode(segment).to_string())
        .collect::<Vec<_>>()
        .join("/")
}

fn sign(secret: &str, string_to_sign: &str) -> Result<String> {
    let mut mac =
        HmacSha1::new_from_slice(secret.as_bytes()).context("Invalid OSS access_key_secret")?;
    mac.update(string_to_sign.as_bytes());
    let signature = mac.finalize().into_bytes();
    Ok(general_purpose::STANDARD.encode(signature))
}

fn presign_url(
    config: &OssConfig,
    method: &str,
    object_key: &str,
    content_type: Option<&str>,
) -> Result<String> {
    validate_config(config)?;

    let expires_at = SystemTime::now()
        .checked_add(Duration::from_secs(config.expires_seconds))
        .context("Failed to compute OSS presign expiry")?
        .duration_since(UNIX_EPOCH)
        .context("Failed to compute OSS presign expiry")?
        .as_secs();

    let object_key = join_prefix(&config.prefix, object_key);
    let encoded_key = encode_object_key(&object_key);
    let canonical_resource = format!("/{}/{}", config.bucket, encoded_key);

    let content_type = content_type.unwrap_or("");
    let string_to_sign = format!(
        "{}\n\n{}\n{}\n{}",
        method, content_type, expires_at, canonical_resource
    );
    let signature = sign(&config.access_key_secret, &string_to_sign)?;
    let signature = urlencoding::encode(&signature);

    let scheme = if config.https { "https" } else { "http" };
    let host = if config.path_style {
        format!("{scheme}://{}", config.endpoint)
    } else {
        format!("{scheme}://{}.{}", config.bucket, config.endpoint)
    };
    let path = if config.path_style {
        format!("/{}/{}", config.bucket, encoded_key)
    } else {
        format!("/{}", encoded_key)
    };

    let mut url = format!(
        "{host}{path}?OSSAccessKeyId={}&Expires={}&Signature={}",
        config.access_key_id, expires_at, signature
    );

    if let Some(token) = config.security_token.as_ref() {
        let token = urlencoding::encode(token);
        url.push_str("&security-token=");
        url.push_str(&token);
    }

    Ok(url)
}

pub struct UploadResult {
    pub size: u64,
    pub sha256: String,
}
