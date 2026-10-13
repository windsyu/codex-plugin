//! Revocable browser authorization shared with the PTY actor. No stored lease.
use std::sync::{
    Arc, Mutex,
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
    deadline: Mutex<Option<std::time::Instant>>,
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
            deadline: Mutex::new(None),
        }))
    }
    pub fn id(&self) -> Uuid {
        self.0.id
    }
    pub fn active(&self) -> bool {
        let expired = self
            .0
            .deadline
            .lock()
            .unwrap()
            .is_some_and(|deadline| std::time::Instant::now() >= deadline);
        if expired {
            self.revoke();
        }
        !self.0.revoked.load(Ordering::Acquire)
    }
    pub(crate) fn leased(duration: std::time::Duration) -> Self {
        let permission = Self::new();
        *permission.0.deadline.lock().unwrap() = Some(std::time::Instant::now() + duration);
        permission
    }
    pub(crate) fn renew(&self, duration: std::time::Duration) -> bool {
        if !self.active() {
            return false;
        }
        let mut deadline = self.0.deadline.lock().unwrap();
        if self.0.revoked.load(Ordering::Acquire)
            || deadline.is_some_and(|value| std::time::Instant::now() >= value)
        {
            drop(deadline);
            self.revoke();
            return false;
        }
        *deadline = Some(std::time::Instant::now() + duration);
        true
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn expired_or_revoked_lease_cannot_be_renewed() {
        let expired = Permission::leased(std::time::Duration::ZERO);
        assert!(!expired.active());
        assert!(!expired.renew(std::time::Duration::from_secs(30)));
        let revoked = Permission::leased(std::time::Duration::from_secs(30));
        revoked.revoke();
        assert!(!revoked.renew(std::time::Duration::from_secs(30)));
        assert!(Permission::new().active());
    }
}
