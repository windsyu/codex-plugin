//! Revocable browser authorization shared with the PTY actor. No stored lease.
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::watch;
use uuid::Uuid;

#[derive(Clone)]
pub struct Permission(Arc<Inner>);
struct Inner {
    id: Uuid,
    revoked: AtomicBool,
    changed: watch::Sender<bool>,
}
impl Default for Permission {
    fn default() -> Self {
        Self::new()
    }
}
impl Permission {
    pub fn new() -> Self {
        Self(Arc::new(Inner {
            id: Uuid::new_v4(),
            revoked: AtomicBool::new(false),
            changed: watch::channel(false).0,
        }))
    }
    pub fn id(&self) -> Uuid {
        self.0.id
    }
    pub fn active(&self) -> bool {
        !self.0.revoked.load(Ordering::Acquire)
    }
    pub fn revoke(&self) {
        self.0.revoked.store(true, Ordering::Release);
        self.0.changed.send_replace(true);
    }
    pub async fn revoked(&self) {
        let mut rx = self.0.changed.subscribe();
        let _ = rx.wait_for(|value| *value).await;
    }
}
