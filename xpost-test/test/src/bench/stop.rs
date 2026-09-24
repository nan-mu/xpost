//! Cooperative stop: first SIGINT/SIGTERM trips the flag; a second one aborts.

use std::time::Duration;

use tokio::sync::watch;

#[derive(Clone)]
pub struct Stop {
    tx: watch::Sender<bool>,
}

impl Stop {
    pub fn new() -> Self {
        let (tx, _) = watch::channel(false);
        Self { tx }
    }

    pub fn trip(&self) {
        let _ = self.tx.send(true);
    }

    pub fn is_set(&self) -> bool {
        *self.tx.borrow()
    }

    pub async fn sleep_or(&self, d: Duration) {
        let mut rx = self.tx.subscribe();
        if *rx.borrow() {
            return;
        }
        tokio::select! {
            _ = tokio::time::sleep(d) => {}
            _ = rx.wait_for(|v| *v) => {}
        }
    }
}

pub fn spawn_watch(stop: Stop) {
    tokio::spawn(async move {
        let mut sigint =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()) {
                Ok(s) => s,
                Err(_) => return,
            };
        let mut sigterm =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(s) => s,
                Err(_) => return,
            };
        tokio::select! {
            _ = sigint.recv() => {}
            _ = sigterm.recv() => {}
        }
        eprintln!("\nstopping (Ctrl+C again to abort)");
        stop.trip();
        tokio::select! {
            _ = sigint.recv() => {}
            _ = sigterm.recv() => {}
        }
        eprintln!("abort");
        std::process::exit(130);
    });
}
