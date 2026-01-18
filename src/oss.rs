use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose};
use bytes::Bytes;
use futures::{Stream, StreamExt};
use hmac::{Hmac, Mac};
use reqwest::{Client, StatusCode, header};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;

use crate::config::OssConfig;

type HmacSha1 = Hmac<Sha1>;

pub fn presign_get_url(config: &OssConfig, object_key: &str) -> Result<String> {
    presign_url(config, "GET", object_key, None)
}

pub fn presign_put_url(config: &OssConfig, object_key: &str, content_type: &str) -> Result<String> {
    presign_url(config, "PUT", object_key, Some(content_type))
}

pub fn presign_delete_url(config: &OssConfig, object_key: &str) -> Result<String> {
    presign_url(config, "DELETE", object_key, None)
}

pub async fn put_bytes(
    config: &OssConfig,
    client: &Client,
    object_key: &str,
    content_type: &str,
    body: Vec<u8>,
) -> Result<()> {
    let url = presign_put_url(config, object_key, content_type)?;
    let response = client
        .put(url)
        .header(header::CONTENT_TYPE, content_type)
        .body(body)
        .send()
        .await?;

    if !response.status().is_success() {
        return Err(anyhow::anyhow!(
            "Failed to upload object {}: {}",
            object_key,
            response.status()
        ));
    }
    Ok(())
}

pub async fn get_object_bytes(
    config: &OssConfig,
    client: &Client,
    object_key: &str,
) -> Result<Bytes> {
    let url = presign_get_url(config, object_key)?;
    let response = client.get(url).send().await?;
    if response.status() == StatusCode::NOT_FOUND {
        return Err(anyhow::anyhow!("OSS object not found: {}", object_key));
    }
    if !response.status().is_success() {
        return Err(anyhow::anyhow!(
            "Failed to fetch object {}: {}",
            object_key,
            response.status()
        ));
    }
    Ok(response.bytes().await?)
}

pub async fn upload_stream<S>(
    config: &OssConfig,
    client: &Client,
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

    let upload_result = match upload_file(
        config,
        client,
        object_key,
        content_type,
        &temp_path,
        upload_result.size,
    )
    .await
    {
        Ok(()) => upload_result,
        Err(err) => {
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err(err);
        }
    };

    let _ = tokio::fs::remove_file(&temp_path).await;
    Ok(upload_result)
}

pub async fn delete_object(config: &OssConfig, client: &Client, object_key: &str) -> Result<()> {
    let url = presign_delete_url(config, object_key)?;
    let response = client.delete(url).send().await?;
    if response.status() == StatusCode::NOT_FOUND {
        return Ok(());
    }
    if !response.status().is_success() {
        return Err(anyhow::anyhow!(
            "Failed to delete object {}: {}",
            object_key,
            response.status()
        ));
    }
    Ok(())
}

pub async fn upload_file(
    config: &OssConfig,
    client: &Client,
    object_key: &str,
    content_type: &str,
    path: &Path,
    size: u64,
) -> Result<()> {
    let url = presign_put_url(config, object_key, content_type)?;
    let file = tokio::fs::File::open(path).await?;
    let stream = ReaderStream::new(file);
    let response = client
        .put(url)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_LENGTH, size.to_string())
        .body(reqwest::Body::wrap_stream(stream))
        .send()
        .await?;

    if !response.status().is_success() {
        return Err(anyhow::anyhow!(
            "Failed to upload object {}: {}",
            object_key,
            response.status()
        ));
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
