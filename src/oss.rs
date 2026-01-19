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
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::AsyncWriteExt;
use tracing::warn;

use crate::config::OssConfig;

type HmacSha1 = Hmac<Sha1>;

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
    Ok(Bytes::from(content))
}

pub async fn upload_stream<S>(
    config: &OssConfig,
    object_key: &str,
    content_type: &str,
    stream: S,
) -> Result<UploadResult>
where
    S: Stream<Item = Result<Bytes, reqwest::Error>> + Send + Unpin + 'static,
{
    let temp_path = temp_path_for(object_key)?;
    let mut file = tokio::fs::File::create(&temp_path).await?;
    let mut hasher = Sha256::new();
    let mut size: u64 = 0;

    let upload_result = async {
        let mut upstream = Box::pin(stream);
        while let Some(chunk) = upstream.next().await {
            let chunk = chunk?;
            size += chunk.len() as u64;
            hasher.update(&chunk);
            file.write_all(&chunk).await?;
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

    drop(file);

    let upload_result = match upload_file(config, object_key, content_type, &temp_path).await {
        Ok(()) => upload_result,
        Err(err) => {
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err(err);
        }
    };

    let _ = tokio::fs::remove_file(&temp_path).await;
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
    let content = tokio::fs::read(path).await?;
    let headers = content_type_headers(content_type);
    client
        .put_object(
            content.as_slice(),
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
