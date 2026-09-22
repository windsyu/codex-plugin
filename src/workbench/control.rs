//! In-memory authority for one native PTY. The terminal actor owns this value
//! and performs writes inside the same serialized command, never via a database
//! lease or a permission check detached from a later write.

use super::permission::Permission;
use std::collections::HashMap;
use std::io;
use std::time::{Duration, Instant};

use serde::Serialize;
use uuid::Uuid;

const RECONNECT_WINDOW: Duration = Duration::from_secs(30);
const MAX_CONNECTIONS: usize = 32;
const MAX_INPUT_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlError {
    Ended,
    UnknownConnection,
    AccessRevoked,
    TooManyConnections,
    InputHeld,
    ReconnectRejected,
    StaleGeneration,
    InputSequence,
    InputSize,
    InvalidSize,
    PtyWriteFailed,
    PtyResizeFailed,
}

/// Only send this on the claiming connection. Deliberately not Debug/Clone.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlGrant {
    pub generation: u64,
    pub reconnect_secret: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlView {
    pub controller_connection: Option<Uuid>,
    pub generation: u64,
    pub reconnect_reserved: bool,
    pub ended: bool,
    pub rows: u16,
    pub cols: u16,
}

struct Owner {
    permission: Option<Permission>,
    connection: Option<Uuid>,
    secret: String,
    reserved_until: Option<Instant>,
    last_input_seq: u64,
}
pub struct InputControl {
    connections: HashMap<Uuid, Option<Permission>>,
    owner: Option<Owner>,
    generation: u64,
    ended: bool,
    rows: u16,
    cols: u16,
}

impl InputControl {
    pub fn new(rows: u16, cols: u16) -> Result<Self, ControlError> {
        valid_size(rows, cols)?;
        Ok(Self {
            connections: HashMap::new(),
            owner: None,
            generation: 0,
            ended: false,
            rows,
            cols,
        })
    }

    /// Registration is read-only. Only an explicit claim/takeover grants input.
    pub fn connect(&mut self) -> Result<Uuid, ControlError> {
        self.connect_authorized(None)
    }

    pub fn connect_authorized(
        &mut self,
        permission: Option<Permission>,
    ) -> Result<Uuid, ControlError> {
        if permission.as_ref().is_some_and(|p| !p.active()) {
            return Err(ControlError::AccessRevoked);
        }
        if self.connections.len() >= MAX_CONNECTIONS {
            return Err(ControlError::TooManyConnections);
        }
        let id = Uuid::new_v4();
        self.connections.insert(id, permission);
        Ok(id)
    }

    pub fn prune_revoked(&mut self) -> Vec<Uuid> {
        let removed: Vec<_> = self
            .connections
            .iter()
            .filter(|(_, p)| p.as_ref().is_some_and(|p| !p.active()))
            .map(|(id, _)| *id)
            .collect();
        for id in &removed {
            self.connections.remove(id);
        }
        if self
            .owner
            .as_ref()
            .is_some_and(|o| o.permission.as_ref().is_some_and(|p| !p.active()))
        {
            self.owner = None;
            self.advance_generation();
        }
        removed
    }

    pub fn disconnect(&mut self, id: Uuid, now: Instant) {
        self.connections.remove(&id);
        if let Some(owner) = self.owner.as_mut()
            && owner.connection == Some(id)
        {
            owner.connection = None;
            owner.reserved_until = Some(now + RECONNECT_WINDOW);
            // Invalidate queued input immediately, even before another claim.
            self.advance_generation();
        }
    }

    pub fn claim(&mut self, id: Uuid, now: Instant) -> Result<ControlGrant, ControlError> {
        self.require_live_connection(id)?;
        self.expire(now);
        if self.owner.is_some() {
            return Err(ControlError::InputHeld);
        }
        Ok(self.grant(id))
    }

    /// The authenticated page must display the effect and explicitly confirm.
    /// No old-owner approval or native turn boundary is required by this model.
    pub fn takeover(&mut self, id: Uuid) -> Result<ControlGrant, ControlError> {
        self.require_live_connection(id)?;
        Ok(self.grant(id))
    }

    pub fn reconnect(
        &mut self,
        id: Uuid,
        secret: &str,
        now: Instant,
    ) -> Result<ControlGrant, ControlError> {
        self.require_live_connection(id)?;
        self.expire(now);
        if !self.owner.as_ref().is_some_and(|owner| {
            owner.permission.as_ref().is_none_or(Permission::active)
                && owner.permission.as_ref().map(Permission::id)
                    == self
                        .connections
                        .get(&id)
                        .and_then(|p| p.as_ref())
                        .map(Permission::id)
                && secret_matches(secret, &owner.secret)
        }) {
            return Err(ControlError::ReconnectRejected);
        }
        // The previous socket may not yet have reported its disconnect. Its
        // generation still becomes invalid before this private reply is sent.
        Ok(self.grant(id))
    }

    pub fn release(&mut self, id: Uuid, generation: u64) -> Result<(), ControlError> {
        self.require_controller(id, generation)?;
        self.owner = None;
        self.advance_generation();
        Ok(())
    }

    /// Called at dequeue time by the single terminal actor. Old queued commands
    /// cannot keep authority after takeover/reconnect. Consume a sequence even
    /// on partial I/O failure; the browser must never replay uncertain bytes.
    pub fn input(
        &mut self,
        id: Uuid,
        generation: u64,
        sequence: u64,
        bytes: &[u8],
        write: impl FnOnce(&[u8]) -> io::Result<()>,
    ) -> Result<(), ControlError> {
        self.require_controller(id, generation)?;
        if bytes.is_empty() || bytes.len() > MAX_INPUT_BYTES {
            return Err(ControlError::InputSize);
        }
        let owner = self.owner.as_mut().unwrap();
        if owner.last_input_seq.checked_add(1) != Some(sequence) {
            return Err(ControlError::InputSequence);
        }
        owner.last_input_seq = sequence;
        write(bytes).map_err(|_| ControlError::PtyWriteFailed)
    }

    pub fn resize(
        &mut self,
        id: Uuid,
        generation: u64,
        rows: u16,
        cols: u16,
        resize: impl FnOnce(u16, u16) -> io::Result<()>,
    ) -> Result<bool, ControlError> {
        self.require_controller(id, generation)?;
        valid_size(rows, cols)?;
        if (rows, cols) == (self.rows, self.cols) {
            return Ok(false);
        }
        resize(rows, cols).map_err(|_| ControlError::PtyResizeFailed)?;
        self.rows = rows;
        self.cols = cols;
        Ok(true)
    }

    pub fn end(&mut self) {
        if !self.ended {
            self.ended = true;
            self.owner = None;
            self.advance_generation();
        }
    }

    pub fn view(&mut self, now: Instant) -> ControlView {
        self.expire(now);
        ControlView {
            controller_connection: self.owner.as_ref().and_then(|owner| owner.connection),
            generation: self.generation,
            reconnect_reserved: self
                .owner
                .as_ref()
                .is_some_and(|owner| owner.reserved_until.is_some()),
            ended: self.ended,
            rows: self.rows,
            cols: self.cols,
        }
    }

    fn expire(&mut self, now: Instant) {
        if self
            .owner
            .as_ref()
            .and_then(|owner| owner.reserved_until)
            .is_some_and(|until| now >= until)
        {
            self.owner = None;
            self.advance_generation();
        }
    }
    fn require_live_connection(&self, id: Uuid) -> Result<(), ControlError> {
        if self.ended {
            return Err(ControlError::Ended);
        }
        if !self.connections.contains_key(&id) {
            return Err(ControlError::UnknownConnection);
        }
        if self
            .connections
            .get(&id)
            .and_then(|p| p.as_ref())
            .is_some_and(|p| !p.active())
        {
            return Err(ControlError::AccessRevoked);
        }
        Ok(())
    }
    fn require_controller(&self, id: Uuid, generation: u64) -> Result<(), ControlError> {
        self.require_live_connection(id)?;
        if self.generation != generation
            || self.owner.as_ref().and_then(|owner| owner.connection) != Some(id)
        {
            return Err(ControlError::StaleGeneration);
        }
        Ok(())
    }
    fn grant(&mut self, id: Uuid) -> ControlGrant {
        self.advance_generation();
        let secret = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        self.owner = Some(Owner {
            permission: self.connections.get(&id).cloned().flatten(),
            connection: Some(id),
            secret: secret.clone(),
            reserved_until: None,
            last_input_seq: 0,
        });
        ControlGrant {
            generation: self.generation,
            reconnect_secret: secret,
        }
    }
    fn advance_generation(&mut self) {
        // A local process cannot practically exhaust this counter. Never wrap
        // and accidentally make an old connection generation valid again.
        self.generation = self
            .generation
            .checked_add(1)
            .expect("input generation exhausted");
    }
}

fn valid_size(rows: u16, cols: u16) -> Result<(), ControlError> {
    if !(2..=200).contains(&rows) || !(20..=500).contains(&cols) {
        return Err(ControlError::InvalidSize);
    }
    Ok(())
}
fn secret_matches(value: &str, expected: &str) -> bool {
    value.len() == expected.len()
        && value
            .bytes()
            .zip(expected.bytes())
            .fold(0u8, |diff, (a, b)| diff | (a ^ b))
            == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_claim_and_takeover_only_allow_the_current_connection_to_write() {
        let now = Instant::now();
        let mut control = InputControl::new(24, 80).unwrap();
        let first = control.connect().unwrap();
        let second = control.connect().unwrap();
        assert!(control.view(now).controller_connection.is_none());
        let grant = control.claim(first, now).unwrap();
        assert_eq!(
            control.claim(second, now).err(),
            Some(ControlError::InputHeld)
        );
        assert_eq!(
            control.input(second, grant.generation, 1, b"bad", |_| panic!(
                "viewer write"
            )),
            Err(ControlError::StaleGeneration)
        );
        let next = control.takeover(second).unwrap();
        assert!(next.generation > grant.generation);
        assert_ne!(next.reconnect_secret, grant.reconnect_secret);
        assert_eq!(
            control.input(first, grant.generation, 1, b"queued", |_| panic!(
                "stale queued input"
            )),
            Err(ControlError::StaleGeneration)
        );
        let mut written = Vec::new();
        control
            .input(
                second,
                next.generation,
                1,
                "中文\n多行".as_bytes(),
                |bytes| {
                    written.extend_from_slice(bytes);
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(written, "中文\n多行".as_bytes());
    }

    #[test]
    fn reconnect_rotates_private_secret_and_generation_without_replaying_bytes() {
        let now = Instant::now();
        let mut control = InputControl::new(24, 80).unwrap();
        let first = control.connect().unwrap();
        let grant = control.claim(first, now).unwrap();
        control
            .input(first, grant.generation, 1, b"x", |_| Ok(()))
            .unwrap();
        control.disconnect(first, now);
        assert!(control.view(now).reconnect_reserved);
        let next = control.connect().unwrap();
        assert_eq!(
            control.reconnect(next, "wrong", now).err(),
            Some(ControlError::ReconnectRejected)
        );
        let recovered = control
            .reconnect(next, &grant.reconnect_secret, now + Duration::from_secs(29))
            .unwrap();
        assert!(recovered.generation > grant.generation);
        assert_eq!(
            control.reconnect(next, &grant.reconnect_secret, now).err(),
            Some(ControlError::ReconnectRejected)
        );
        assert_eq!(
            control.input(next, recovered.generation, 2, b"replayed", |_| panic!(
                "replayed sequence"
            )),
            Err(ControlError::InputSequence)
        );
        control
            .input(next, recovered.generation, 1, b"new", |_| Ok(()))
            .unwrap();
    }

    #[test]
    fn refresh_can_rebind_before_old_socket_closes_and_old_disconnect_is_harmless() {
        let now = Instant::now();
        let mut control = InputControl::new(24, 80).unwrap();
        let old = control.connect().unwrap();
        let grant = control.claim(old, now).unwrap();
        let fresh = control.connect().unwrap();
        let recovered = control
            .reconnect(fresh, &grant.reconnect_secret, now)
            .unwrap();
        control.disconnect(old, now);
        assert_eq!(control.view(now).controller_connection, Some(fresh));
        control
            .input(fresh, recovered.generation, 1, b"new", |_| Ok(()))
            .unwrap();
    }

    #[test]
    fn reserved_timeout_release_and_takeover_allow_only_explicit_new_claims() {
        let now = Instant::now();
        let mut control = InputControl::new(24, 80).unwrap();
        let old = control.connect().unwrap();
        let grant = control.claim(old, now).unwrap();
        control.disconnect(old, now);
        let fresh = control.connect().unwrap();
        assert_eq!(
            control.claim(fresh, now).err(),
            Some(ControlError::InputHeld)
        );
        assert_eq!(
            control
                .reconnect(fresh, &grant.reconnect_secret, now + RECONNECT_WINDOW)
                .err(),
            Some(ControlError::ReconnectRejected)
        );
        assert!(
            control
                .view(now + RECONNECT_WINDOW)
                .controller_connection
                .is_none()
        );
        let next = control.claim(fresh, now + RECONNECT_WINDOW).unwrap();
        control.release(fresh, next.generation).unwrap();
        assert_eq!(
            control.input(fresh, next.generation, 1, b"late", |_| panic!(
                "released write"
            )),
            Err(ControlError::StaleGeneration)
        );
        let current = control.claim(fresh, now + RECONNECT_WINDOW).unwrap();
        control.disconnect(fresh, now);
        let other = control.connect().unwrap();
        let takeover = control.takeover(other).unwrap();
        assert!(takeover.generation > current.generation);
    }

    #[test]
    fn duplicate_out_of_order_oversized_and_uncertain_inputs_never_replay() {
        let now = Instant::now();
        let mut control = InputControl::new(24, 80).unwrap();
        let id = control.connect().unwrap();
        let grant = control.claim(id, now).unwrap();
        assert_eq!(
            control.input(
                id,
                grant.generation,
                1,
                &vec![b'x'; MAX_INPUT_BYTES + 1],
                |_| panic!("oversize")
            ),
            Err(ControlError::InputSize)
        );
        assert_eq!(
            control.input(id, grant.generation, 1, b"partial", |_| Err(
                io::Error::other("synthetic-private-error")
            )),
            Err(ControlError::PtyWriteFailed)
        );
        assert_eq!(
            control.input(id, grant.generation, 1, b"partial", |_| panic!("duplicate")),
            Err(ControlError::InputSequence)
        );
        assert_eq!(
            control.input(id, grant.generation, 3, b"gap", |_| panic!("gap")),
            Err(ControlError::InputSequence)
        );
        control
            .input(id, grant.generation, 2, b"new", |_| Ok(()))
            .unwrap();
    }

    #[test]
    fn only_controller_resizes_and_duplicate_or_failed_sizes_do_not_commit() {
        let now = Instant::now();
        let mut control = InputControl::new(24, 80).unwrap();
        let id = control.connect().unwrap();
        let viewer = control.connect().unwrap();
        let grant = control.claim(id, now).unwrap();
        assert_eq!(
            control.resize(viewer, grant.generation, 40, 120, |_, _| panic!(
                "viewer resize"
            )),
            Err(ControlError::StaleGeneration)
        );
        assert!(
            !control
                .resize(id, grant.generation, 24, 80, |_, _| panic!(
                    "duplicate resize"
                ))
                .unwrap()
        );
        assert_eq!(
            control.resize(id, grant.generation, 1, 80, |_, _| panic!("invalid size")),
            Err(ControlError::InvalidSize)
        );
        assert_eq!(
            control.resize(id, grant.generation, 40, 120, |_, _| Err(io::Error::other(
                "private"
            ))),
            Err(ControlError::PtyResizeFailed)
        );
        assert_eq!((control.view(now).rows, control.view(now).cols), (24, 80));
        assert!(
            control
                .resize(id, grant.generation, 40, 120, |_, _| Ok(()))
                .unwrap()
        );
        assert_eq!((control.view(now).rows, control.view(now).cols), (40, 120));
    }

    #[test]
    fn ended_terminal_never_grants_input_or_starts_another_process() {
        let now = Instant::now();
        let mut control = InputControl::new(24, 80).unwrap();
        let id = control.connect().unwrap();
        let grant = control.claim(id, now).unwrap();
        control.end();
        assert!(control.view(now).ended);
        assert_eq!(control.claim(id, now).err(), Some(ControlError::Ended));
        assert_eq!(control.takeover(id).err(), Some(ControlError::Ended));
        assert_eq!(
            control.reconnect(id, &grant.reconnect_secret, now).err(),
            Some(ControlError::Ended)
        );
        assert_eq!(
            control.input(id, grant.generation, 1, b"late", |_| panic!("ended write")),
            Err(ControlError::Ended)
        );
        let generation = control.view(now).generation;
        control.end();
        assert_eq!(control.view(now).generation, generation);
    }

    #[test]
    fn connection_budget_unknown_clients_and_public_state_do_not_leak_the_secret() {
        let now = Instant::now();
        let mut control = InputControl::new(24, 80).unwrap();
        assert_eq!(
            control.claim(Uuid::new_v4(), now).err(),
            Some(ControlError::UnknownConnection)
        );
        let ids = (0..MAX_CONNECTIONS)
            .map(|_| control.connect().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(control.connect(), Err(ControlError::TooManyConnections));
        let grant = control.claim(ids[0], now).unwrap();
        let public = serde_json::to_string(&control.view(now)).unwrap();
        assert!(!public.contains(&grant.reconnect_secret));
        assert!(!public.contains("secret"));
        control.disconnect(ids[1], now);
        assert!(control.connect().is_ok());
    }
}

#[cfg(test)]
mod access_tests {
    use super::*;
    #[test]
    fn revocation_blocks_dequeued_input_and_releases_disconnected_reservations() {
        let mut c = InputControl::new(24, 80).unwrap();
        let p = Permission::new();
        let remote = c.connect_authorized(Some(p.clone())).unwrap();
        let grant = c.claim(remote, Instant::now()).unwrap();
        p.revoke();
        assert_eq!(
            c.input(remote, grant.generation, 1, b"queued", |_| panic!(
                "revoked input"
            )),
            Err(ControlError::AccessRevoked)
        );
        c.disconnect(remote, Instant::now());
        c.prune_revoked();
        assert!(!c.view(Instant::now()).reconnect_reserved);
        let another = c.connect_authorized(Some(Permission::new())).unwrap();
        assert_eq!(
            c.reconnect(another, &grant.reconnect_secret, Instant::now())
                .err(),
            Some(ControlError::ReconnectRejected)
        );
        assert!(c.claim(another, Instant::now()).is_ok());
    }
    #[test]
    fn reconnect_secret_is_bound_to_browser_permission_even_before_revocation() {
        let mut c = InputControl::new(24, 80).unwrap();
        let p = Permission::new();
        let first = c.connect_authorized(Some(p.clone())).unwrap();
        let grant = c.claim(first, Instant::now()).unwrap();
        c.disconnect(first, Instant::now());
        let other = c.connect_authorized(Some(Permission::new())).unwrap();
        let local = c.connect().unwrap();
        for id in [other, local] {
            assert_eq!(
                c.reconnect(id, &grant.reconnect_secret, Instant::now())
                    .err(),
                Some(ControlError::ReconnectRejected)
            );
        }
        let same = c.connect_authorized(Some(p)).unwrap();
        assert!(
            c.reconnect(same, &grant.reconnect_secret, Instant::now())
                .is_ok()
        );
    }
}
