//! Job registry for `./test serve`.

use std::collections::HashMap;
use std::sync::Arc;

use test_common::fw_agent::{JOB_CANCELLED, JOB_DONE, JOB_FAILED, JOB_PENDING, JOB_RUNNING};
use tokio::sync::Mutex;

use super::job_log::JobLog;

pub struct Jobs {
    next_id: std::sync::atomic::AtomicU64,
    inner: Arc<Mutex<Vec<Job>>>,
    logs: Arc<Mutex<HashMap<u64, JobLog>>>,
}

#[derive(Clone, Debug)]
pub struct Job {
    pub id: u64,
    pub bench: String,
    pub smoke: bool,
    pub formal: bool,
    pub state: u32,
    pub message: String,
}

impl Jobs {
    pub fn new() -> Self {
        Self {
            next_id: std::sync::atomic::AtomicU64::new(1),
            inner: Arc::new(Mutex::new(Vec::new())),
            logs: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn create(&self, bench: String, smoke: bool, formal: bool) -> Job {
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let job = Job {
            id,
            bench,
            smoke,
            formal,
            state: JOB_PENDING,
            message: "pending".into(),
        };
        self.inner.lock().await.push(job.clone());
        self.logs.lock().await.insert(id, JobLog::default());
        job
    }

    pub async fn log_for(&self, id: u64) -> JobLog {
        self.logs.lock().await.get(&id).cloned().unwrap_or_default()
    }

    pub async fn get(&self, id: u64) -> Option<Job> {
        self.inner.lock().await.iter().find(|j| j.id == id).cloned()
    }

    pub async fn set_state(&self, id: u64, state: u32, message: impl Into<String>) {
        let mut jobs = self.inner.lock().await;
        if let Some(j) = jobs.iter_mut().find(|j| j.id == id) {
            j.state = state;
            j.message = message.into();
        }
    }

    #[allow(dead_code)]
    pub async fn cancel(&self, id: u64) -> bool {
        let mut jobs = self.inner.lock().await;
        if let Some(j) = jobs.iter_mut().find(|j| j.id == id) {
            if is_terminal(j.state) {
                return false;
            }
            if matches!(j.state, JOB_PENDING | JOB_RUNNING) {
                j.state = JOB_CANCELLED;
                j.message = "cancelled".into();
                return true;
            }
        }
        false
    }
}

pub fn is_terminal(state: u32) -> bool {
    matches!(state, JOB_DONE | JOB_FAILED | JOB_CANCELLED)
}
