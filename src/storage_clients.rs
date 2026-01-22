use anyhow::Result;
use aws_sdk_s3::Client as S3Client;
use oss_sdk_rs::oss::OSS;
use std::sync::Arc;

use crate::config::{StorageConfig, StorageMode};
use crate::{oss, s3};

#[derive(Clone, Default)]
pub struct StorageClients {
    pub oss: Option<Arc<OSS<'static>>>,
    pub s3: Option<Arc<S3Client>>,
}

impl StorageClients {
    pub async fn new(storage: &StorageConfig) -> Result<Self> {
        match storage.mode {
            StorageMode::Local => Ok(Self::default()),
            StorageMode::Oss => Ok(Self {
                oss: Some(Arc::new(oss::build_client(&storage.oss)?)),
                s3: None,
            }),
            StorageMode::S3 => Ok(Self {
                oss: None,
                s3: Some(Arc::new(s3::build_client(&storage.s3).await?)),
            }),
        }
    }

    pub fn oss(&self) -> Option<&OSS<'static>> {
        self.oss.as_deref()
    }

    pub fn s3(&self) -> Option<&S3Client> {
        self.s3.as_deref()
    }
}
