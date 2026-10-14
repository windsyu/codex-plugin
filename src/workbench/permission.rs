//! Revocable browser authorization shared with the PTY actor. No stored lease.
use super::lease_clock::{Clock, Lease};
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
    deadline: Mutex<Option<Lease>>,
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
            .as_ref()
            .is_some_and(|lease| lease.remaining().is_none());
        if expired {
            self.revoke();
        }
        !self.0.revoked.load(Ordering::Acquire)
    }
    pub(crate) fn leased(duration: std::time::Duration) -> Self {
        Self::leased_with_clock(duration, Clock::default())
    }
    pub(crate) fn leased_with_clock(duration: std::time::Duration, clock: Clock) -> Self {
        let permission = Self::new();
        match Lease::new(clock, duration) {
            Some(lease) => *permission.0.deadline.lock().unwrap() = Some(lease),
            None => permission.revoke(),
        }
        permission
    }
    pub(crate) fn renew(&self, duration: std::time::Duration) -> bool {
        let mut deadline = self.0.deadline.lock().unwrap();
        if self.0.revoked.load(Ordering::Acquire)
            || !match deadline.as_mut() {
                Some(lease) => lease.renew(duration),
                None => match Lease::new(Clock::default(), duration) {
                    Some(lease) => {
                        *deadline = Some(lease);
                        true
                    }
                    None => false,
                },
            }
        {
            drop(deadline);
            self.revoke();
            return false;
        }
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
    #[test]
    fn suspended_age_fences_dequeued_terminal_input_before_renewal() {
        use crate::workbench::control::{ControlError, InputControl};
        use std::time::{Duration, Instant};
        let clock = Clock::manual(Duration::from_secs(100));
        let permission = Permission::leased_with_clock(Duration::from_secs(30), clock.clone());
        let mut control = InputControl::new(24, 80).unwrap();
        let remote = control
            .connect_authorized(Some(permission.clone()))
            .unwrap();
        let awake = Instant::now();
        let grant = control.claim(remote, awake).unwrap();
        // Only the authorization oracle advances: simulate sleep while the
        // actor's ordinary monotonic scheduling clock has barely moved.
        clock.set(Some(Duration::from_secs(131)));
        assert!(awake.elapsed() < Duration::from_secs(1));
        assert_eq!(
            control.input(remote, grant.generation, 1, b"queued input", |_| panic!(
                "expired PTY write"
            )),
            Err(ControlError::AccessRevoked)
        );
        assert!(!permission.renew(Duration::from_secs(30)));
        clock.set(Some(Duration::from_secs(100)));
        assert!(
            !permission.active(),
            "revocation is irreversible even if the clock recovers"
        );
        assert!(
            Permission::new().active(),
            "unleased local/LAN authority is unaffected"
        );
    }
    #[test]
    fn leased_clock_failure_revokes_and_cannot_be_repaired_by_renew() {
        use std::time::Duration;
        for failure in [None, Some(Duration::from_secs(99)), Some(Duration::MAX)] {
            let clock = Clock::manual(Duration::from_secs(100));
            let permission = Permission::leased_with_clock(Duration::from_secs(30), clock.clone());
            clock.set(failure);
            assert!(!permission.renew(Duration::from_secs(30)));
            clock.set(Some(Duration::from_secs(101)));
            assert!(!permission.active());
        }
        let clock = Clock::manual(Duration::MAX);
        assert!(!Permission::leased_with_clock(Duration::from_secs(30), clock.clone()).active());
        clock.set(None);
        assert!(!Permission::leased_with_clock(Duration::from_secs(30), clock).active());
    }
}
