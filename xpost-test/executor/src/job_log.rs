//! Append-only job stdout/stderr capture (served via HTTP / RPC).

use std::sync::Arc;

use tokio::sync::Mutex;

#[derive(Clone, Default)]
pub struct JobLog {
    inner: Arc<Mutex<Vec<u8>>>,
}

impl JobLog {
    pub async fn append(&self, chunk: &[u8]) {
        if chunk.is_empty() {
            return;
        }
        self.inner.lock().await.extend_from_slice(chunk);
    }

    pub async fn read_from(&self, offset: usize) -> (usize, Vec<u8>) {
        let buf = self.inner.lock().await;
        let off = offset.min(buf.len());
        (off, buf[off..].to_vec())
    }

    pub async fn len(&self) -> usize {
        self.inner.lock().await.len()
    }
}
