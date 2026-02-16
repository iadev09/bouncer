use std::path::PathBuf;

use anyhow::{Context, Result};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct Spool {
    pub root: PathBuf,
    pub incoming: PathBuf,
    pub processing: PathBuf,
    pub done: PathBuf,
    pub failed: PathBuf
}

impl Spool {
    pub fn new(root: PathBuf) -> Self {
        Self {
            incoming: root.join("incoming"),
            processing: root.join("processing"),
            done: root.join("done"),
            failed: root.join("failed"),
            root
        }
    }

    pub async fn ensure_dirs(&self) -> Result<()> {
        for dir in [
            &self.root,
            &self.incoming,
            &self.processing,
            &self.done,
            &self.failed
        ] {
            tokio::fs::create_dir_all(dir).await.with_context(|| {
                format!("failed to create dir {}", dir.display())
            })?;
        }
        Ok(())
    }

    pub async fn enqueue_mail(
        &self,
        payload: &[u8]
    ) -> Result<PathBuf> {
        let id = Uuid::now_v7();
        let file_name = format!("{id}.eml");
        let tmp_name = format!("{id}.eml.tmp");

        let tmp_path = self.incoming.join(tmp_name);
        let final_path = self.incoming.join(file_name);

        let mut file =
            tokio::fs::File::create(&tmp_path).await.with_context(|| {
                format!("failed to create {}", tmp_path.display())
            })?;

        file.write_all(payload).await.with_context(|| {
            format!("failed to write {}", tmp_path.display())
        })?;

        file.sync_all().await.with_context(|| {
            format!("failed to fsync {}", tmp_path.display())
        })?;

        drop(file);

        tokio::fs::rename(&tmp_path, &final_path).await.with_context(|| {
            format!(
                "failed to rename {} -> {}",
                tmp_path.display(),
                final_path.display()
            )
        })?;

        Ok(final_path)
    }
}
