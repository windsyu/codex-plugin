use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};

use crate::clock::now_ms;
use crate::domain::session::{
    AcquireThreadLease, CompleteTurnOwner, InputLeaseRecord, PersistInputLease,
    RegisterSessionWorker, SessionRecoveryReport, SessionWorkerRecord, SessionWorkerRegistration,
    SessionWorkerTransition, TerminalAttachmentRecord, ThreadLeaseRecord, TurnOwnerRecord,
};

use super::Database;

impl Database {
    pub(crate) fn register_session_worker_on(
        &self,
        connection: &mut Connection,
        registration: &SessionWorkerRegistration,
    ) -> Result<RegisterSessionWorker> {
        validate_registration(registration)?;
        let transaction = connection.transaction()?;
        if let Some(thread_id) = registration.codex_thread_id.as_deref()
            && let Some(existing) = active_thread_lease_on(
                &transaction,
                &registration.source_id,
                &registration.source_epoch,
                thread_id,
            )?
        {
            return Ok(RegisterSessionWorker::ThreadOwned {
                worker_id: existing.worker_id,
            });
        }
        let occurred_at_ms = now_ms();
        transaction.execute(
            "INSERT INTO session_workers(
               worker_id,create_command_id,principal_id,source_id,source_epoch,mode,state,version,
               primary_thread_id,canonical_cwd,rows,cols,runtime_dir_name,created_at_ms,updated_at_ms)
             VALUES (?1,?2,?3,?4,?5,?6,'starting',1,?7,?8,?9,?10,?11,?12,?12)",
            params![
                registration.worker_id,
                registration.create_command_id,
                registration.principal_id,
                registration.source_id,
                registration.source_epoch,
                registration.mode,
                registration.codex_thread_id,
                registration.canonical_cwd,
                i64::from(registration.rows),
                i64::from(registration.cols),
                registration.runtime_dir_name,
                occurred_at_ms,
            ],
        )?;
        append_worker_transition(
            &transaction,
            &registration.worker_id,
            None,
            "starting",
            None,
            1,
            Some("session_reserved"),
            Some(&registration.create_command_id),
            occurred_at_ms,
        )?;
        let initial_lease_state = if registration.codex_thread_id.is_some() {
            "active"
        } else {
            "acquiring"
        };
        transaction.execute(
            "INSERT INTO thread_leases(
               lease_id,source_id,source_epoch,codex_thread_id,reservation_id,worker_id,role,state,
               version,created_at_ms,updated_at_ms)
             VALUES (?1,?2,?3,?4,?5,?6,'primary',?7,1,?8,?8)",
            params![
                registration.primary_lease_id,
                registration.source_id,
                registration.source_epoch,
                registration.codex_thread_id,
                registration.reservation_id,
                registration.worker_id,
                initial_lease_state,
                occurred_at_ms,
            ],
        )?;
        append_lease_transition(
            &transaction,
            &registration.primary_lease_id,
            None,
            initial_lease_state,
            None,
            1,
            registration.codex_thread_id.as_deref(),
            registration.reservation_id.as_deref(),
            Some("session_reserved"),
            Some(&registration.create_command_id),
            occurred_at_ms,
        )?;
        let worker = session_worker_on(&transaction, &registration.worker_id)?
            .context("registered Session Worker disappeared")?;
        let lease = thread_lease_on(&transaction, &registration.primary_lease_id)?
            .context("registered ThreadLease disappeared")?;
        transaction.commit()?;
        Ok(RegisterSessionWorker::Created {
            worker: Box::new(worker),
            lease: Box::new(lease),
        })
    }

    pub(crate) fn upgrade_thread_reservation_on(
        &self,
        connection: &mut Connection,
        lease_id: &str,
        worker_id: &str,
        expected_version: i64,
        codex_thread_id: &str,
        command_id: Option<&str>,
    ) -> Result<AcquireThreadLease> {
        let transaction = connection.transaction()?;
        let current = thread_lease_on(&transaction, lease_id)?
            .context("ThreadLease reservation not found")?;
        if current.worker_id != worker_id {
            bail!("ThreadLease reservation belongs to another worker");
        }
        if current.codex_thread_id.as_deref() == Some(codex_thread_id) && current.state == "active"
        {
            return Ok(AcquireThreadLease::Existing(current));
        }
        if current.version != expected_version
            || current.state != "acquiring"
            || current.reservation_id.is_none()
        {
            return Ok(AcquireThreadLease::VersionConflict);
        }
        if let Some(existing) = active_thread_lease_on(
            &transaction,
            &current.source_id,
            &current.source_epoch,
            codex_thread_id,
        )? {
            return if existing.worker_id == worker_id {
                Ok(AcquireThreadLease::Existing(existing))
            } else {
                Ok(AcquireThreadLease::ThreadOwned {
                    worker_id: existing.worker_id,
                })
            };
        }
        let occurred_at_ms = now_ms();
        let next_version = current.version + 1;
        let changed = transaction.execute(
            "UPDATE thread_leases
             SET codex_thread_id=?1,reservation_id=NULL,state='active',version=?2,updated_at_ms=?3
             WHERE lease_id=?4 AND worker_id=?5 AND state='acquiring' AND version=?6",
            params![
                codex_thread_id,
                next_version,
                occurred_at_ms,
                lease_id,
                worker_id,
                expected_version,
            ],
        )?;
        if changed != 1 {
            return Ok(AcquireThreadLease::VersionConflict);
        }
        append_lease_transition(
            &transaction,
            lease_id,
            Some("acquiring"),
            "active",
            Some(current.version),
            next_version,
            Some(codex_thread_id),
            None,
            Some("thread_observed"),
            command_id,
            occurred_at_ms,
        )?;
        if current.role == "primary" {
            update_primary_thread(
                &transaction,
                worker_id,
                codex_thread_id,
                command_id,
                occurred_at_ms,
            )?;
        }
        let lease =
            thread_lease_on(&transaction, lease_id)?.context("upgraded ThreadLease disappeared")?;
        transaction.commit()?;
        Ok(AcquireThreadLease::Acquired(lease))
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn acquire_thread_lease_on(
        &self,
        connection: &mut Connection,
        lease_id: &str,
        source_id: &str,
        source_epoch: &str,
        codex_thread_id: &str,
        worker_id: &str,
        role: &str,
        command_id: Option<&str>,
    ) -> Result<AcquireThreadLease> {
        if !matches!(role, "primary" | "side" | "child") {
            bail!("invalid ThreadLease role");
        }
        let transaction = connection.transaction()?;
        if session_worker_on(&transaction, worker_id)?.is_none() {
            bail!("Session Worker not found");
        }
        if let Some(existing) =
            active_thread_lease_on(&transaction, source_id, source_epoch, codex_thread_id)?
        {
            return if existing.worker_id == worker_id {
                Ok(AcquireThreadLease::Existing(existing))
            } else {
                Ok(AcquireThreadLease::ThreadOwned {
                    worker_id: existing.worker_id,
                })
            };
        }
        let occurred_at_ms = now_ms();
        transaction.execute(
            "INSERT INTO thread_leases(
               lease_id,source_id,source_epoch,codex_thread_id,worker_id,role,state,version,
               created_at_ms,updated_at_ms)
             VALUES (?1,?2,?3,?4,?5,?6,'active',1,?7,?7)",
            params![
                lease_id,
                source_id,
                source_epoch,
                codex_thread_id,
                worker_id,
                role,
                occurred_at_ms,
            ],
        )?;
        append_lease_transition(
            &transaction,
            lease_id,
            None,
            "active",
            None,
            1,
            Some(codex_thread_id),
            None,
            Some("thread_claimed"),
            command_id,
            occurred_at_ms,
        )?;
        let lease =
            thread_lease_on(&transaction, lease_id)?.context("acquired ThreadLease disappeared")?;
        transaction.commit()?;
        Ok(AcquireThreadLease::Acquired(lease))
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn reserve_thread_lease_on(
        &self,
        connection: &mut Connection,
        lease_id: &str,
        reservation_id: &str,
        source_id: &str,
        source_epoch: &str,
        worker_id: &str,
        role: &str,
        command_id: Option<&str>,
    ) -> Result<ThreadLeaseRecord> {
        if !matches!(role, "primary" | "side" | "child") {
            bail!("invalid ThreadLease role");
        }
        let transaction = connection.transaction()?;
        let worker =
            session_worker_on(&transaction, worker_id)?.context("Session Worker not found")?;
        if matches!(
            worker.state.as_str(),
            "stopping" | "exited" | "failed" | "stale_epoch" | "orphaned"
        ) {
            bail!("Session Worker cannot reserve another Thread");
        }
        let occurred_at_ms = now_ms();
        transaction.execute(
            "INSERT INTO thread_leases(
               lease_id,source_id,source_epoch,codex_thread_id,reservation_id,worker_id,role,state,
               version,created_at_ms,updated_at_ms)
             VALUES (?1,?2,?3,NULL,?4,?5,?6,'acquiring',1,?7,?7)",
            params![
                lease_id,
                source_id,
                source_epoch,
                reservation_id,
                worker_id,
                role,
                occurred_at_ms,
            ],
        )?;
        append_lease_transition(
            &transaction,
            lease_id,
            None,
            "acquiring",
            None,
            1,
            None,
            Some(reservation_id),
            Some("protocol_thread_reserved"),
            command_id,
            occurred_at_ms,
        )?;
        let lease =
            thread_lease_on(&transaction, lease_id)?.context("reserved ThreadLease disappeared")?;
        transaction.commit()?;
        Ok(lease)
    }

    pub(crate) fn release_thread_lease_on(
        &self,
        connection: &mut Connection,
        source_id: &str,
        source_epoch: &str,
        codex_thread_id: &str,
        worker_id: &str,
        command_id: Option<&str>,
    ) -> Result<ThreadLeaseRecord> {
        let transaction = connection.transaction()?;
        let lease = active_thread_lease_on(&transaction, source_id, source_epoch, codex_thread_id)?
            .context("active ThreadLease not found")?;
        if lease.worker_id != worker_id {
            bail!("ThreadLease belongs to another Session Worker");
        }
        let occurred_at_ms = now_ms();
        let next_version = lease.version + 1;
        transaction.execute(
            "UPDATE thread_leases SET state='released',version=?1,updated_at_ms=?2
             WHERE lease_id=?3 AND version=?4",
            params![next_version, occurred_at_ms, lease.lease_id, lease.version],
        )?;
        append_lease_transition(
            &transaction,
            &lease.lease_id,
            Some(&lease.state),
            "released",
            Some(lease.version),
            next_version,
            Some(codex_thread_id),
            None,
            Some("protocol_thread_unsubscribed"),
            command_id,
            occurred_at_ms,
        )?;
        let worker =
            session_worker_on(&transaction, worker_id)?.context("Session Worker not found")?;
        if worker.primary_thread_id.as_deref() == Some(codex_thread_id) {
            let next_worker_version = worker.version + 1;
            transaction.execute(
                "UPDATE session_workers SET primary_thread_id=NULL,version=?1,updated_at_ms=?2
                 WHERE worker_id=?3 AND version=?4",
                params![
                    next_worker_version,
                    occurred_at_ms,
                    worker_id,
                    worker.version
                ],
            )?;
            append_worker_transition(
                &transaction,
                worker_id,
                Some(&worker.state),
                &worker.state,
                Some(worker.version),
                next_worker_version,
                Some("primary_thread_unsubscribed"),
                command_id,
                occurred_at_ms,
            )?;
        }
        let released = thread_lease_on(&transaction, &lease.lease_id)?
            .context("released ThreadLease disappeared")?;
        transaction.commit()?;
        Ok(released)
    }

    pub(crate) fn close_thread_reservation_on(
        &self,
        connection: &mut Connection,
        lease_id: &str,
        worker_id: &str,
        to_state: &str,
        reason_code: &str,
        command_id: Option<&str>,
    ) -> Result<ThreadLeaseRecord> {
        if !matches!(to_state, "released" | "orphaned") {
            bail!("invalid reservation terminal state");
        }
        let transaction = connection.transaction()?;
        let lease = thread_lease_on(&transaction, lease_id)?
            .context("ThreadLease reservation not found")?;
        if lease.worker_id != worker_id
            || lease.state != "acquiring"
            || lease.reservation_id.is_none()
        {
            bail!("ThreadLease reservation state conflict");
        }
        let occurred_at_ms = now_ms();
        let next_version = lease.version + 1;
        transaction.execute(
            "UPDATE thread_leases SET state=?1,version=?2,updated_at_ms=?3
             WHERE lease_id=?4 AND state='acquiring' AND version=?5",
            params![
                to_state,
                next_version,
                occurred_at_ms,
                lease_id,
                lease.version
            ],
        )?;
        append_lease_transition(
            &transaction,
            lease_id,
            Some("acquiring"),
            to_state,
            Some(lease.version),
            next_version,
            None,
            lease.reservation_id.as_deref(),
            Some(reason_code),
            command_id,
            occurred_at_ms,
        )?;
        let closed = thread_lease_on(&transaction, lease_id)?
            .context("closed ThreadLease reservation disappeared")?;
        transaction.commit()?;
        Ok(closed)
    }

    pub(crate) fn transition_session_worker_on(
        &self,
        connection: &mut Connection,
        transition: &SessionWorkerTransition,
    ) -> Result<SessionWorkerRecord> {
        validate_worker_state(&transition.to_state)?;
        let transaction = connection.transaction()?;
        let current = session_worker_on(&transaction, &transition.worker_id)?
            .context("Session Worker not found")?;
        if current.version != transition.expected_version {
            bail!("Session Worker version conflict");
        }
        let next_version = current.version + 1;
        let occurred_at_ms = now_ms();
        let changed = transaction.execute(
            "UPDATE session_workers
             SET state=?1,version=?2,pid=COALESCE(?3,pid),
               primary_thread_id=COALESCE(?4,primary_thread_id),error_code=?5,updated_at_ms=?6
             WHERE worker_id=?7 AND version=?8",
            params![
                transition.to_state,
                next_version,
                transition.pid.map(i64::from),
                transition.primary_thread_id,
                transition.error_code,
                occurred_at_ms,
                transition.worker_id,
                transition.expected_version,
            ],
        )?;
        if changed != 1 {
            bail!("Session Worker version conflict");
        }
        append_worker_transition(
            &transaction,
            &transition.worker_id,
            Some(&current.state),
            &transition.to_state,
            Some(current.version),
            next_version,
            transition.reason_code.as_deref(),
            transition.command_id.as_deref(),
            occurred_at_ms,
        )?;
        let worker = session_worker_on(&transaction, &transition.worker_id)?
            .context("transitioned Session Worker disappeared")?;
        transaction.commit()?;
        Ok(worker)
    }

    pub(crate) fn fail_session_before_write_on(
        &self,
        connection: &mut Connection,
        worker_id: &str,
        error_code: &str,
        command_id: &str,
    ) -> Result<()> {
        let transaction = connection.transaction()?;
        let worker =
            session_worker_on(&transaction, worker_id)?.context("Session Worker not found")?;
        if matches!(
            worker.state.as_str(),
            "exited" | "failed" | "stale_epoch" | "orphaned"
        ) {
            return Ok(());
        }
        let occurred_at_ms = now_ms();
        let next_worker_version = worker.version + 1;
        transaction.execute(
            "UPDATE session_workers
             SET state='failed',version=?1,error_code=?2,updated_at_ms=?3
             WHERE worker_id=?4 AND version=?5",
            params![
                next_worker_version,
                error_code,
                occurred_at_ms,
                worker_id,
                worker.version
            ],
        )?;
        append_worker_transition(
            &transaction,
            worker_id,
            Some(&worker.state),
            "failed",
            Some(worker.version),
            next_worker_version,
            Some(error_code),
            Some(command_id),
            occurred_at_ms,
        )?;

        let leases = thread_leases_for_worker_on(&transaction, worker_id)?;
        for lease in leases
            .into_iter()
            .filter(|lease| matches!(lease.state.as_str(), "acquiring" | "active" | "releasing"))
        {
            let next_lease_version = lease.version + 1;
            transaction.execute(
                "UPDATE thread_leases SET state='released',version=?1,updated_at_ms=?2
                 WHERE lease_id=?3 AND version=?4",
                params![
                    next_lease_version,
                    occurred_at_ms,
                    lease.lease_id,
                    lease.version
                ],
            )?;
            append_lease_transition(
                &transaction,
                &lease.lease_id,
                Some(&lease.state),
                "released",
                Some(lease.version),
                next_lease_version,
                lease.codex_thread_id.as_deref(),
                lease.reservation_id.as_deref(),
                Some("pre_write_start_failed"),
                Some(command_id),
                occurred_at_ms,
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn finalize_session_worker_on(
        &self,
        connection: &mut Connection,
        worker_id: &str,
        requested_state: &str,
        error_code: Option<&str>,
        reason_code: &str,
    ) -> Result<SessionWorkerRecord> {
        if !matches!(requested_state, "exited" | "failed" | "stale_epoch") {
            bail!("invalid final Session Worker state");
        }
        let transaction = connection.transaction()?;
        let current =
            session_worker_on(&transaction, worker_id)?.context("Session Worker not found")?;
        let final_state = if matches!(
            current.state.as_str(),
            "failed" | "stale_epoch" | "orphaned"
        ) {
            current.state.clone()
        } else {
            requested_state.into()
        };
        let occurred_at_ms = now_ms();
        if current.state != final_state {
            let next_version = current.version + 1;
            transaction.execute(
                "UPDATE session_workers
                 SET state=?1,version=?2,error_code=COALESCE(?3,error_code),updated_at_ms=?4
                 WHERE worker_id=?5 AND version=?6",
                params![
                    final_state,
                    next_version,
                    error_code,
                    occurred_at_ms,
                    worker_id,
                    current.version
                ],
            )?;
            append_worker_transition(
                &transaction,
                worker_id,
                Some(&current.state),
                &final_state,
                Some(current.version),
                next_version,
                Some(reason_code),
                None,
                occurred_at_ms,
            )?;
        }

        let failed = final_state != "exited";
        let lease_terminal_state = match final_state.as_str() {
            "exited" => "released",
            "stale_epoch" => "stale",
            _ => "orphaned",
        };
        for lease in thread_leases_for_worker_on(&transaction, worker_id)?
            .into_iter()
            .filter(|lease| matches!(lease.state.as_str(), "acquiring" | "active" | "releasing"))
        {
            let next_version = lease.version + 1;
            transaction.execute(
                "UPDATE thread_leases SET state=?1,version=?2,updated_at_ms=?3
                 WHERE lease_id=?4 AND version=?5",
                params![
                    lease_terminal_state,
                    next_version,
                    occurred_at_ms,
                    lease.lease_id,
                    lease.version
                ],
            )?;
            append_lease_transition(
                &transaction,
                &lease.lease_id,
                Some(&lease.state),
                lease_terminal_state,
                Some(lease.version),
                next_version,
                lease.codex_thread_id.as_deref(),
                lease.reservation_id.as_deref(),
                Some(reason_code),
                None,
                occurred_at_ms,
            )?;
        }

        let attachment_state = if failed { "orphaned" } else { "closed" };
        transaction.execute(
            "UPDATE terminal_attachments
             SET state=?1,version=version+1,updated_at_ms=?2
             WHERE worker_id=?3 AND state IN ('prepared','connected','detached')",
            params![attachment_state, occurred_at_ms, worker_id],
        )?;
        orphan_or_release_active_input(
            &transaction,
            worker_id,
            lease_terminal_state,
            reason_code,
            occurred_at_ms,
        )?;
        finalize_active_turn_owners(
            &transaction,
            worker_id,
            if final_state == "stale_epoch" {
                "stale"
            } else {
                "orphaned"
            },
            reason_code,
            occurred_at_ms,
        )?;
        let worker = session_worker_on(&transaction, worker_id)?
            .context("finalized Session Worker disappeared")?;
        transaction.commit()?;
        Ok(worker)
    }

    pub(crate) fn create_terminal_attachment_on(
        &self,
        connection: &mut Connection,
        attachment_id: &str,
        worker_id: &str,
        principal_id: &str,
        control_token_hash: &str,
    ) -> Result<()> {
        let transaction = connection.transaction()?;
        let worker =
            session_worker_on(&transaction, worker_id)?.context("Session Worker not found")?;
        if matches!(
            worker.state.as_str(),
            "stopping" | "exited" | "failed" | "stale_epoch" | "orphaned"
        ) {
            bail!("Session Worker is not attachable");
        }
        let occurred_at_ms = now_ms();
        transaction.execute(
            "INSERT INTO terminal_attachments(
               attachment_id,worker_id,principal_id,control_token_hash,state,version,created_at_ms,updated_at_ms)
             VALUES (?1,?2,?3,?4,'prepared',1,?5,?5)",
            params![attachment_id, worker_id, principal_id, control_token_hash, occurred_at_ms],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn transition_terminal_attachment_on(
        &self,
        connection: &mut Connection,
        attachment_id: &str,
        principal_id: &str,
        to_state: &str,
    ) -> Result<()> {
        if !matches!(
            to_state,
            "connected" | "detached" | "expired" | "closed" | "orphaned"
        ) {
            bail!("invalid terminal attachment state");
        }
        let changed = connection.execute(
            "UPDATE terminal_attachments SET state=?1,version=version+1,updated_at_ms=?2
             WHERE attachment_id=?3 AND principal_id=?4
               AND state IN ('prepared','connected','detached')",
            params![to_state, now_ms(), attachment_id, principal_id],
        )?;
        if changed != 1 {
            bail!("terminal attachment state conflict");
        }
        Ok(())
    }

    pub(crate) fn acquire_input_lease_on(
        &self,
        connection: &mut Connection,
        lease_id: &str,
        worker_id: &str,
        owner_id: &str,
        expected_version: i64,
    ) -> Result<PersistInputLease> {
        let transaction = connection.transaction()?;
        let worker =
            session_worker_on(&transaction, worker_id)?.context("Session Worker not found")?;
        if worker.input_lease_version != expected_version {
            return Ok(PersistInputLease::Conflict {
                version: worker.input_lease_version,
            });
        }
        let active: Option<(String, String)> = transaction
            .query_row(
                "SELECT lease_id,owner_id FROM input_leases
                 WHERE worker_id=?1 AND state IN ('active','releasing')",
                [worker_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((active_id, active_owner)) = active {
            return if active_owner == owner_id {
                Ok(PersistInputLease::Acquired {
                    lease_id: active_id,
                    version: expected_version,
                })
            } else {
                Ok(PersistInputLease::Conflict {
                    version: expected_version,
                })
            };
        }
        let next_version = expected_version + 1;
        let occurred_at_ms = now_ms();
        transaction.execute(
            "INSERT INTO input_leases(
               lease_id,worker_id,owner_type,owner_id,state,version,acquired_at_ms,updated_at_ms)
             VALUES (?1,?2,'terminal',?3,'active',?4,?5,?5)",
            params![lease_id, worker_id, owner_id, next_version, occurred_at_ms],
        )?;
        transaction.execute(
            "UPDATE session_workers SET input_lease_version=?1,updated_at_ms=?2
             WHERE worker_id=?3 AND input_lease_version=?4",
            params![next_version, occurred_at_ms, worker_id, expected_version],
        )?;
        transaction.execute(
            "INSERT INTO input_lease_transitions(
               lease_id,worker_id,from_state,to_state,from_version,to_version,owner_type,owner_id,
               reason_code,occurred_at_ms)
             VALUES (?1,?2,NULL,'active',?3,?4,'terminal',?5,'acquired',?6)",
            params![
                lease_id,
                worker_id,
                expected_version,
                next_version,
                owner_id,
                occurred_at_ms
            ],
        )?;
        transaction.commit()?;
        Ok(PersistInputLease::Acquired {
            lease_id: lease_id.into(),
            version: next_version,
        })
    }

    pub(crate) fn release_input_lease_on(
        &self,
        connection: &mut Connection,
        lease_id: &str,
        worker_id: &str,
        owner_id: &str,
        expected_version: i64,
    ) -> Result<PersistInputLease> {
        let transaction = connection.transaction()?;
        let worker =
            session_worker_on(&transaction, worker_id)?.context("Session Worker not found")?;
        if worker.input_lease_version != expected_version {
            return Ok(PersistInputLease::Conflict {
                version: worker.input_lease_version,
            });
        }
        let current: Option<(String, i64)> = transaction
            .query_row(
                "SELECT state,version FROM input_leases
                 WHERE lease_id=?1 AND worker_id=?2 AND owner_id=?3",
                params![lease_id, worker_id, owner_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((state, lease_version)) = current else {
            return Ok(PersistInputLease::Conflict {
                version: expected_version,
            });
        };
        if state != "active" || lease_version != expected_version {
            return Ok(PersistInputLease::Conflict {
                version: expected_version,
            });
        }
        let next_version = expected_version + 1;
        let occurred_at_ms = now_ms();
        transaction.execute(
            "UPDATE input_leases SET state='released',version=?1,updated_at_ms=?2
             WHERE lease_id=?3 AND state='active' AND version=?4",
            params![next_version, occurred_at_ms, lease_id, expected_version],
        )?;
        transaction.execute(
            "UPDATE session_workers SET input_lease_version=?1,updated_at_ms=?2
             WHERE worker_id=?3 AND input_lease_version=?4",
            params![next_version, occurred_at_ms, worker_id, expected_version],
        )?;
        transaction.execute(
            "INSERT INTO input_lease_transitions(
               lease_id,worker_id,from_state,to_state,from_version,to_version,owner_type,owner_id,
               reason_code,occurred_at_ms)
             VALUES (?1,?2,'active','released',?3,?4,'terminal',?5,'released',?6)",
            params![
                lease_id,
                worker_id,
                expected_version,
                next_version,
                owner_id,
                occurred_at_ms
            ],
        )?;
        transaction.commit()?;
        Ok(PersistInputLease::Released {
            version: next_version,
        })
    }

    pub(crate) fn session_worker(&self, worker_id: &str) -> Result<Option<SessionWorkerRecord>> {
        let connection = self.read_connection()?;
        session_worker_on(&connection, worker_id)
    }

    pub(crate) fn session_worker_for_command(
        &self,
        command_id: &str,
    ) -> Result<Option<SessionWorkerRecord>> {
        let connection = self.read_connection()?;
        connection
            .query_row(
                "SELECT worker_id,create_command_id,principal_id,source_id,source_epoch,mode,state,
                   version,input_lease_version,primary_thread_id,canonical_cwd,rows,cols,pid,runtime_dir_name,error_code
                 FROM session_workers WHERE create_command_id=?1",
                [command_id],
                session_worker_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub(crate) fn thread_leases_for_worker(
        &self,
        worker_id: &str,
    ) -> Result<Vec<ThreadLeaseRecord>> {
        let connection = self.read_connection()?;
        thread_leases_for_worker_on(&connection, worker_id)
    }

    pub(crate) fn active_thread_lease_owner(
        &self,
        source_id: &str,
        source_epoch: &str,
        codex_thread_id: &str,
    ) -> Result<Option<String>> {
        let connection = self.read_connection()?;
        Ok(
            active_thread_lease_on(&connection, source_id, source_epoch, codex_thread_id)?
                .map(|lease| lease.worker_id),
        )
    }

    #[cfg(test)]
    pub(crate) fn terminal_attachment(
        &self,
        attachment_id: &str,
    ) -> Result<Option<TerminalAttachmentRecord>> {
        let connection = self.read_connection()?;
        connection
            .query_row(
                "SELECT attachment_id,worker_id,principal_id,state,version,last_ack
                     FROM terminal_attachments WHERE attachment_id=?1",
                [attachment_id],
                |row| {
                    Ok(TerminalAttachmentRecord {
                        attachment_id: row.get(0)?,
                        worker_id: row.get(1)?,
                        principal_id: row.get(2)?,
                        state: row.get(3)?,
                        version: row.get(4)?,
                        last_ack: row.get(5)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub(crate) fn terminal_attachments_for_worker(
        &self,
        worker_id: &str,
    ) -> Result<Vec<TerminalAttachmentRecord>> {
        let connection = self.read_connection()?;
        let mut statement = connection.prepare(
            "SELECT attachment_id,worker_id,principal_id,state,version,last_ack
             FROM terminal_attachments WHERE worker_id=?1 ORDER BY created_at_ms,attachment_id",
        )?;
        Ok(statement
            .query_map([worker_id], |row| {
                Ok(TerminalAttachmentRecord {
                    attachment_id: row.get(0)?,
                    worker_id: row.get(1)?,
                    principal_id: row.get(2)?,
                    state: row.get(3)?,
                    version: row.get(4)?,
                    last_ack: row.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub(crate) fn active_input_lease(&self, worker_id: &str) -> Result<Option<InputLeaseRecord>> {
        let connection = self.read_connection()?;
        connection
            .query_row(
                "SELECT lease_id,worker_id,owner_type,owner_id,state,version
                     FROM input_leases
                     WHERE worker_id=?1 AND state IN ('active','releasing')",
                [worker_id],
                |row| {
                    Ok(InputLeaseRecord {
                        lease_id: row.get(0)?,
                        worker_id: row.get(1)?,
                        owner_type: row.get(2)?,
                        owner_id: row.get(3)?,
                        state: row.get(4)?,
                        version: row.get(5)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub(crate) fn upsert_turn_owner_on(
        &self,
        connection: &mut Connection,
        owner: &TurnOwnerRecord,
    ) -> Result<TurnOwnerRecord> {
        if owner.state != "active"
            || !matches!(
                owner.owner_type.as_str(),
                "terminal" | "channel" | "gateway"
            )
        {
            bail!("invalid active TurnOwner");
        }
        let transaction = connection.transaction()?;
        if let Some(current) = turn_owner_on(
            &transaction,
            &owner.source_id,
            &owner.source_epoch,
            &owner.codex_thread_id,
            &owner.codex_turn_id,
        )? {
            if current.state == "active"
                && current.worker_id == owner.worker_id
                && current.owner_type == owner.owner_type
                && current.owner_id == owner.owner_id
            {
                return Ok(current);
            }
            bail!("TurnOwner already exists with different ownership or terminal state");
        }
        let occurred_at_ms = now_ms();
        transaction.execute(
            "INSERT INTO turn_owners(source_id,source_epoch,worker_id,codex_thread_id,codex_turn_id,
               owner_type,owner_id,principal_id,input_lease_id,state,version,start_command_id,started_at_ms,updated_at_ms)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,'active',1,?10,?11,?11)",
            params![
                owner.source_id,
                owner.source_epoch,
                owner.worker_id,
                owner.codex_thread_id,
                owner.codex_turn_id,
                owner.owner_type,
                owner.owner_id,
                owner.principal_id,
                owner.input_lease_id,
                owner.start_command_id,
                occurred_at_ms,
            ],
        )?;
        append_turn_owner_transition(
            &transaction,
            owner,
            None,
            "active",
            None,
            1,
            "turn_started",
            owner.start_command_id.as_deref(),
            occurred_at_ms,
        )?;
        let created = turn_owner_on(
            &transaction,
            &owner.source_id,
            &owner.source_epoch,
            &owner.codex_thread_id,
            &owner.codex_turn_id,
        )?
        .context("created TurnOwner disappeared")?;
        transaction.commit()?;
        Ok(created)
    }

    pub(crate) fn complete_turn_owner_on(
        &self,
        connection: &mut Connection,
        completion: &CompleteTurnOwner,
    ) -> Result<TurnOwnerRecord> {
        if !matches!(
            completion.to_state.as_str(),
            "completed" | "interrupted" | "failed" | "orphaned" | "stale"
        ) {
            bail!("invalid terminal TurnOwner state");
        }
        let transaction = connection.transaction()?;
        let current = turn_owner_on(
            &transaction,
            &completion.source_id,
            &completion.source_epoch,
            &completion.codex_thread_id,
            &completion.codex_turn_id,
        )?
        .context("TurnOwner not found")?;
        if current.state != "active" {
            if current.state == completion.to_state {
                return Ok(current);
            }
            bail!("TurnOwner is already terminal");
        }
        let next_version = current.version + 1;
        let occurred_at_ms = now_ms();
        transaction.execute(
            "UPDATE turn_owners SET state=?1,version=?2,updated_at_ms=?3
             WHERE source_id=?4 AND source_epoch=?5 AND codex_thread_id=?6 AND codex_turn_id=?7
               AND state='active' AND version=?8",
            params![
                completion.to_state,
                next_version,
                occurred_at_ms,
                completion.source_id,
                completion.source_epoch,
                completion.codex_thread_id,
                completion.codex_turn_id,
                current.version
            ],
        )?;
        append_turn_owner_transition(
            &transaction,
            &current,
            Some("active"),
            &completion.to_state,
            Some(current.version),
            next_version,
            &completion.reason_code,
            completion.command_id.as_deref(),
            occurred_at_ms,
        )?;
        let completed = turn_owner_on(
            &transaction,
            &completion.source_id,
            &completion.source_epoch,
            &completion.codex_thread_id,
            &completion.codex_turn_id,
        )?
        .context("completed TurnOwner disappeared")?;
        transaction.commit()?;
        Ok(completed)
    }

    pub(crate) fn active_turn_owner(
        &self,
        source_id: &str,
        source_epoch: &str,
        codex_thread_id: &str,
        codex_turn_id: &str,
    ) -> Result<Option<TurnOwnerRecord>> {
        let connection = self.read_connection()?;
        Ok(turn_owner_on(
            &connection,
            source_id,
            source_epoch,
            codex_thread_id,
            codex_turn_id,
        )?
        .filter(|owner| owner.state == "active"))
    }

    pub(crate) fn active_turn_owners_for_worker(
        &self,
        worker_id: &str,
    ) -> Result<Vec<TurnOwnerRecord>> {
        let connection = self.read_connection()?;
        let mut statement = connection.prepare(
            "SELECT source_id,source_epoch,worker_id,codex_thread_id,codex_turn_id,owner_type,
               owner_id,principal_id,input_lease_id,state,version,start_command_id
             FROM turn_owners WHERE worker_id=?1 AND state='active'
             ORDER BY started_at_ms,codex_thread_id,codex_turn_id",
        )?;
        Ok(statement
            .query_map([worker_id], turn_owner_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub(crate) fn recover_sessions_after_restart(&self) -> Result<SessionRecoveryReport> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction()?;
        validate_session_projections(&transaction)?;
        let occurred_at_ms = now_ms();
        let mut report = SessionRecoveryReport::default();

        let workers = query_ids_versions_states(
            &transaction,
            "SELECT worker_id,version,state FROM session_workers
             WHERE state IN ('starting','connecting','ready','detached','stopping')",
        )?;
        for (worker_id, version, state) in workers {
            let next_version = version + 1;
            transaction.execute(
                "UPDATE session_workers SET state='orphaned',version=?1,error_code='GATEWAY_RESTARTED',
                   updated_at_ms=?2 WHERE worker_id=?3 AND version=?4",
                params![next_version, occurred_at_ms, worker_id, version],
            )?;
            append_worker_transition(
                &transaction,
                &worker_id,
                Some(&state),
                "orphaned",
                Some(version),
                next_version,
                Some("gateway_restart"),
                None,
                occurred_at_ms,
            )?;
            report.orphaned_workers += 1;
        }

        report.orphaned_thread_leases = orphan_thread_leases(&transaction, occurred_at_ms)?;
        report.orphaned_attachments = transaction.execute(
            "UPDATE terminal_attachments SET state='orphaned',version=version+1,updated_at_ms=?1
             WHERE state IN ('prepared','connected','detached')",
            [occurred_at_ms],
        )?;
        report.orphaned_input_leases = orphan_input_leases(&transaction, occurred_at_ms)?;
        let active_turn_workers = {
            let mut statement = transaction
                .prepare("SELECT DISTINCT worker_id FROM turn_owners WHERE state='active'")?;
            statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        for worker_id in active_turn_workers {
            finalize_active_turn_owners(
                &transaction,
                &worker_id,
                "orphaned",
                "gateway_restart",
                occurred_at_ms,
            )?;
        }
        transaction.execute(
            "UPDATE worker_connection_epochs SET state='closed',closed_at_ms=?1,
               close_reason='gateway_restart'
             WHERE state='open'",
            [occurred_at_ms],
        )?;
        transaction.commit()?;
        Ok(report)
    }

    #[cfg(test)]
    pub(crate) fn validate_session_transition_consistency_for_test(&self) -> Result<()> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction()?;
        validate_session_projections(&transaction)
    }
}

fn validate_registration(registration: &SessionWorkerRegistration) -> Result<()> {
    if !matches!(registration.mode.as_str(), "new" | "resume") {
        bail!("invalid Session Worker mode");
    }
    if registration.codex_thread_id.is_some() == registration.reservation_id.is_some() {
        bail!("Session Worker registration requires exactly one Thread or reservation ID");
    }
    if registration.mode == "resume" && registration.codex_thread_id.is_none() {
        bail!("resume Session Worker requires a Codex Thread ID");
    }
    Ok(())
}

fn validate_worker_state(state: &str) -> Result<()> {
    if matches!(
        state,
        "starting"
            | "connecting"
            | "ready"
            | "detached"
            | "stopping"
            | "exited"
            | "stale_epoch"
            | "failed"
            | "orphaned"
    ) {
        Ok(())
    } else {
        bail!("invalid Session Worker state")
    }
}

fn session_worker_on(
    connection: &Connection,
    worker_id: &str,
) -> Result<Option<SessionWorkerRecord>> {
    connection
        .query_row(
            "SELECT worker_id,create_command_id,principal_id,source_id,source_epoch,mode,state,
               version,input_lease_version,primary_thread_id,canonical_cwd,rows,cols,pid,runtime_dir_name,error_code
             FROM session_workers WHERE worker_id=?1",
            [worker_id],
            session_worker_from_row,
        )
        .optional()
        .map_err(Into::into)
}

fn session_worker_from_row(row: &Row<'_>) -> rusqlite::Result<SessionWorkerRecord> {
    let rows = row.get::<_, i64>(11)?;
    let cols = row.get::<_, i64>(12)?;
    let pid = row.get::<_, Option<i64>>(13)?;
    Ok(SessionWorkerRecord {
        worker_id: row.get(0)?,
        create_command_id: row.get(1)?,
        principal_id: row.get(2)?,
        source_id: row.get(3)?,
        source_epoch: row.get(4)?,
        mode: row.get(5)?,
        state: row.get(6)?,
        version: row.get(7)?,
        input_lease_version: row.get(8)?,
        primary_thread_id: row.get(9)?,
        canonical_cwd: row.get(10)?,
        rows: u16::try_from(rows).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                11,
                rusqlite::types::Type::Integer,
                Box::new(error),
            )
        })?,
        cols: u16::try_from(cols).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                12,
                rusqlite::types::Type::Integer,
                Box::new(error),
            )
        })?,
        pid: pid.and_then(|value| u32::try_from(value).ok()),
        runtime_dir_name: row.get(14)?,
        error_code: row.get(15)?,
    })
}

fn thread_lease_on(connection: &Connection, lease_id: &str) -> Result<Option<ThreadLeaseRecord>> {
    connection
        .query_row(
            "SELECT lease_id,source_id,source_epoch,codex_thread_id,reservation_id,worker_id,
               role,state,version FROM thread_leases WHERE lease_id=?1",
            [lease_id],
            thread_lease_from_row,
        )
        .optional()
        .map_err(Into::into)
}

fn thread_leases_for_worker_on(
    connection: &Connection,
    worker_id: &str,
) -> Result<Vec<ThreadLeaseRecord>> {
    let mut statement = connection.prepare(
        "SELECT lease_id,source_id,source_epoch,codex_thread_id,reservation_id,worker_id,
           role,state,version FROM thread_leases WHERE worker_id=?1 ORDER BY created_at_ms,lease_id",
    )?;
    Ok(statement
        .query_map([worker_id], thread_lease_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

fn active_thread_lease_on(
    connection: &Connection,
    source_id: &str,
    source_epoch: &str,
    codex_thread_id: &str,
) -> Result<Option<ThreadLeaseRecord>> {
    connection
        .query_row(
            "SELECT lease_id,source_id,source_epoch,codex_thread_id,reservation_id,worker_id,
               role,state,version FROM thread_leases
             WHERE source_id=?1 AND source_epoch=?2 AND codex_thread_id=?3
               AND state IN ('acquiring','active','releasing','orphaned')",
            params![source_id, source_epoch, codex_thread_id],
            thread_lease_from_row,
        )
        .optional()
        .map_err(Into::into)
}

fn thread_lease_from_row(row: &Row<'_>) -> rusqlite::Result<ThreadLeaseRecord> {
    Ok(ThreadLeaseRecord {
        lease_id: row.get(0)?,
        source_id: row.get(1)?,
        source_epoch: row.get(2)?,
        codex_thread_id: row.get(3)?,
        reservation_id: row.get(4)?,
        worker_id: row.get(5)?,
        role: row.get(6)?,
        state: row.get(7)?,
        version: row.get(8)?,
    })
}

fn turn_owner_on(
    connection: &Connection,
    source_id: &str,
    source_epoch: &str,
    codex_thread_id: &str,
    codex_turn_id: &str,
) -> Result<Option<TurnOwnerRecord>> {
    connection
        .query_row(
            "SELECT source_id,source_epoch,worker_id,codex_thread_id,codex_turn_id,owner_type,
               owner_id,principal_id,input_lease_id,state,version,start_command_id
             FROM turn_owners WHERE source_id=?1 AND source_epoch=?2
               AND codex_thread_id=?3 AND codex_turn_id=?4",
            params![source_id, source_epoch, codex_thread_id, codex_turn_id],
            turn_owner_from_row,
        )
        .optional()
        .map_err(Into::into)
}

fn turn_owner_from_row(row: &Row<'_>) -> rusqlite::Result<TurnOwnerRecord> {
    Ok(TurnOwnerRecord {
        source_id: row.get(0)?,
        source_epoch: row.get(1)?,
        worker_id: row.get(2)?,
        codex_thread_id: row.get(3)?,
        codex_turn_id: row.get(4)?,
        owner_type: row.get(5)?,
        owner_id: row.get(6)?,
        principal_id: row.get(7)?,
        input_lease_id: row.get(8)?,
        state: row.get(9)?,
        version: row.get(10)?,
        start_command_id: row.get(11)?,
    })
}

#[allow(clippy::too_many_arguments)]
fn append_worker_transition(
    transaction: &Transaction<'_>,
    worker_id: &str,
    from_state: Option<&str>,
    to_state: &str,
    from_version: Option<i64>,
    to_version: i64,
    reason_code: Option<&str>,
    command_id: Option<&str>,
    occurred_at_ms: i64,
) -> Result<()> {
    transaction.execute(
        "INSERT INTO session_worker_transitions(
           worker_id,from_state,to_state,from_version,to_version,reason_code,command_id,occurred_at_ms)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        params![
            worker_id,
            from_state,
            to_state,
            from_version,
            to_version,
            reason_code,
            command_id,
            occurred_at_ms,
        ],
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn append_lease_transition(
    transaction: &Transaction<'_>,
    lease_id: &str,
    from_state: Option<&str>,
    to_state: &str,
    from_version: Option<i64>,
    to_version: i64,
    codex_thread_id: Option<&str>,
    reservation_id: Option<&str>,
    reason_code: Option<&str>,
    command_id: Option<&str>,
    occurred_at_ms: i64,
) -> Result<()> {
    transaction.execute(
        "INSERT INTO thread_lease_transitions(
           lease_id,from_state,to_state,from_version,to_version,codex_thread_id,reservation_id,
           reason_code,command_id,occurred_at_ms)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        params![
            lease_id,
            from_state,
            to_state,
            from_version,
            to_version,
            codex_thread_id,
            reservation_id,
            reason_code,
            command_id,
            occurred_at_ms,
        ],
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn append_turn_owner_transition(
    transaction: &Transaction<'_>,
    owner: &TurnOwnerRecord,
    from_state: Option<&str>,
    to_state: &str,
    from_version: Option<i64>,
    to_version: i64,
    reason_code: &str,
    command_id: Option<&str>,
    occurred_at_ms: i64,
) -> Result<()> {
    transaction.execute(
        "INSERT INTO turn_owner_transitions(source_id,source_epoch,worker_id,codex_thread_id,
           codex_turn_id,from_state,to_state,from_version,to_version,owner_type,owner_id,
           principal_id,reason_code,command_id,occurred_at_ms)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
        params![
            owner.source_id,
            owner.source_epoch,
            owner.worker_id,
            owner.codex_thread_id,
            owner.codex_turn_id,
            from_state,
            to_state,
            from_version,
            to_version,
            owner.owner_type,
            owner.owner_id,
            owner.principal_id,
            reason_code,
            command_id,
            occurred_at_ms,
        ],
    )?;
    Ok(())
}

fn update_primary_thread(
    transaction: &Transaction<'_>,
    worker_id: &str,
    codex_thread_id: &str,
    command_id: Option<&str>,
    occurred_at_ms: i64,
) -> Result<()> {
    let worker = session_worker_on(transaction, worker_id)?.context("Session Worker not found")?;
    let next_version = worker.version + 1;
    transaction.execute(
        "UPDATE session_workers SET primary_thread_id=?1,version=?2,updated_at_ms=?3
         WHERE worker_id=?4 AND version=?5",
        params![
            codex_thread_id,
            next_version,
            occurred_at_ms,
            worker_id,
            worker.version
        ],
    )?;
    append_worker_transition(
        transaction,
        worker_id,
        Some(&worker.state),
        &worker.state,
        Some(worker.version),
        next_version,
        Some("primary_thread_observed"),
        command_id,
        occurred_at_ms,
    )
}

fn validate_session_projections(transaction: &Transaction<'_>) -> Result<()> {
    let inconsistent_worker: Option<String> = transaction
        .query_row(
            "SELECT w.worker_id FROM session_workers w
             WHERE NOT EXISTS(
               SELECT 1 FROM session_worker_transitions t WHERE t.worker_id=w.worker_id)
                OR w.version<>(SELECT t.to_version FROM session_worker_transitions t
                  WHERE t.worker_id=w.worker_id ORDER BY t.transition_seq DESC LIMIT 1)
                OR w.state<>(SELECT t.to_state FROM session_worker_transitions t
                  WHERE t.worker_id=w.worker_id ORDER BY t.transition_seq DESC LIMIT 1)
             LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(worker_id) = inconsistent_worker {
        bail!("Session Worker transition projection is inconsistent for {worker_id}");
    }
    let inconsistent_lease: Option<String> = transaction
        .query_row(
            "SELECT l.lease_id FROM thread_leases l
             WHERE NOT EXISTS(
               SELECT 1 FROM thread_lease_transitions t WHERE t.lease_id=l.lease_id)
                OR l.version<>(SELECT t.to_version FROM thread_lease_transitions t
                  WHERE t.lease_id=l.lease_id ORDER BY t.transition_seq DESC LIMIT 1)
                OR l.state<>(SELECT t.to_state FROM thread_lease_transitions t
                  WHERE t.lease_id=l.lease_id ORDER BY t.transition_seq DESC LIMIT 1)
             LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(lease_id) = inconsistent_lease {
        bail!("ThreadLease transition projection is inconsistent for {lease_id}");
    }
    let inconsistent_turn: Option<String> = transaction
        .query_row(
            "SELECT o.codex_turn_id FROM turn_owners o
             WHERE NOT EXISTS(
               SELECT 1 FROM turn_owner_transitions t
               WHERE t.source_id=o.source_id AND t.source_epoch=o.source_epoch
                 AND t.codex_thread_id=o.codex_thread_id AND t.codex_turn_id=o.codex_turn_id)
                OR o.version<>(SELECT t.to_version FROM turn_owner_transitions t
                  WHERE t.source_id=o.source_id AND t.source_epoch=o.source_epoch
                    AND t.codex_thread_id=o.codex_thread_id AND t.codex_turn_id=o.codex_turn_id
                  ORDER BY t.transition_seq DESC LIMIT 1)
                OR o.state<>(SELECT t.to_state FROM turn_owner_transitions t
                  WHERE t.source_id=o.source_id AND t.source_epoch=o.source_epoch
                    AND t.codex_thread_id=o.codex_thread_id AND t.codex_turn_id=o.codex_turn_id
                  ORDER BY t.transition_seq DESC LIMIT 1)
             LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(turn_id) = inconsistent_turn {
        bail!("TurnOwner transition projection is inconsistent for {turn_id}");
    }
    Ok(())
}

fn query_ids_versions_states(
    transaction: &Transaction<'_>,
    sql: &str,
) -> Result<Vec<(String, i64, String)>> {
    let mut statement = transaction.prepare(sql)?;
    Ok(statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

fn orphan_thread_leases(transaction: &Transaction<'_>, occurred_at_ms: i64) -> Result<usize> {
    let leases = query_ids_versions_states(
        transaction,
        "SELECT lease_id,version,state FROM thread_leases
         WHERE state IN ('acquiring','active','releasing')",
    )?;
    for (lease_id, version, state) in &leases {
        let current = thread_lease_on(transaction, lease_id)?
            .context("recoverable ThreadLease disappeared")?;
        let next_version = version + 1;
        transaction.execute(
            "UPDATE thread_leases SET state='orphaned',version=?1,updated_at_ms=?2
             WHERE lease_id=?3 AND version=?4",
            params![next_version, occurred_at_ms, lease_id, version],
        )?;
        append_lease_transition(
            transaction,
            lease_id,
            Some(state),
            "orphaned",
            Some(*version),
            next_version,
            current.codex_thread_id.as_deref(),
            current.reservation_id.as_deref(),
            Some("gateway_restart"),
            None,
            occurred_at_ms,
        )?;
    }
    Ok(leases.len())
}

fn orphan_or_release_active_input(
    transaction: &Transaction<'_>,
    worker_id: &str,
    to_state: &str,
    reason_code: &str,
    occurred_at_ms: i64,
) -> Result<usize> {
    if !matches!(to_state, "released" | "orphaned" | "stale") {
        bail!("invalid final InputLease state");
    }
    let lease: Option<(String, String, String, i64, String)> = transaction
        .query_row(
            "SELECT lease_id,owner_type,owner_id,version,state FROM input_leases
             WHERE worker_id=?1 AND state IN ('active','releasing')",
            [worker_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?;
    let Some((lease_id, owner_type, owner_id, version, from_state)) = lease else {
        return Ok(0);
    };
    let next_version = version + 1;
    transaction.execute(
        "UPDATE input_leases SET state=?1,version=?2,updated_at_ms=?3
         WHERE lease_id=?4 AND version=?5",
        params![to_state, next_version, occurred_at_ms, lease_id, version],
    )?;
    transaction.execute(
        "UPDATE session_workers SET input_lease_version=?1,updated_at_ms=?2
         WHERE worker_id=?3 AND input_lease_version=?4",
        params![next_version, occurred_at_ms, worker_id, version],
    )?;
    transaction.execute(
        "INSERT INTO input_lease_transitions(
           lease_id,worker_id,from_state,to_state,from_version,to_version,owner_type,owner_id,
           reason_code,occurred_at_ms)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        params![
            lease_id,
            worker_id,
            from_state,
            to_state,
            version,
            next_version,
            owner_type,
            owner_id,
            reason_code,
            occurred_at_ms,
        ],
    )?;
    Ok(1)
}

fn orphan_input_leases(transaction: &Transaction<'_>, occurred_at_ms: i64) -> Result<usize> {
    let worker_ids = {
        let mut statement = transaction.prepare(
            "SELECT DISTINCT worker_id FROM input_leases WHERE state IN ('active','releasing')",
        )?;
        statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let mut changed = 0;
    for worker_id in worker_ids {
        changed += orphan_or_release_active_input(
            transaction,
            &worker_id,
            "orphaned",
            "gateway_restart",
            occurred_at_ms,
        )?;
    }
    Ok(changed)
}

fn finalize_active_turn_owners(
    transaction: &Transaction<'_>,
    worker_id: &str,
    to_state: &str,
    reason_code: &str,
    occurred_at_ms: i64,
) -> Result<usize> {
    if !matches!(to_state, "orphaned" | "stale") {
        bail!("invalid final TurnOwner state");
    }
    let owners = {
        let mut statement = transaction.prepare(
            "SELECT source_id,source_epoch,worker_id,codex_thread_id,codex_turn_id,owner_type,
               owner_id,principal_id,input_lease_id,state,version,start_command_id
             FROM turn_owners WHERE worker_id=?1 AND state='active'",
        )?;
        statement
            .query_map([worker_id], turn_owner_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    for owner in &owners {
        let next_version = owner.version + 1;
        transaction.execute(
            "UPDATE turn_owners SET state=?1,version=?2,updated_at_ms=?3
             WHERE source_id=?4 AND source_epoch=?5 AND codex_thread_id=?6 AND codex_turn_id=?7
               AND state='active' AND version=?8",
            params![
                to_state,
                next_version,
                occurred_at_ms,
                owner.source_id,
                owner.source_epoch,
                owner.codex_thread_id,
                owner.codex_turn_id,
                owner.version
            ],
        )?;
        append_turn_owner_transition(
            transaction,
            owner,
            Some("active"),
            to_state,
            Some(owner.version),
            next_version,
            reason_code,
            None,
            occurred_at_ms,
        )?;
    }
    Ok(owners.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::gateway::{GatewayCommandOrigin, GatewayCommandTarget, NewGatewayCommand};
    use crate::store::Database;
    use serde_json::json;
    use tempfile::TempDir;

    fn registration(
        worker: &str,
        command: &str,
        thread: Option<&str>,
    ) -> SessionWorkerRegistration {
        SessionWorkerRegistration {
            worker_id: worker.into(),
            create_command_id: command.into(),
            principal_id: "local_bearer".into(),
            source_id: "source-1".into(),
            source_epoch: "epoch-1".into(),
            mode: if thread.is_some() { "resume" } else { "new" }.into(),
            canonical_cwd: "/synthetic".into(),
            rows: 24,
            cols: 80,
            runtime_dir_name: worker.into(),
            primary_lease_id: format!("lease-{worker}"),
            codex_thread_id: thread.map(str::to_string),
            reservation_id: thread.is_none().then(|| format!("reservation-{worker}")),
        }
    }

    fn seed_command(database: &Database, command_id: &str, key: &str) -> Result<()> {
        let mut connection = database.connect()?;
        database.receive_gateway_command_on(
            &mut connection,
            &NewGatewayCommand {
                command_id: command_id.into(),
                principal_id: "local_bearer".into(),
                capability: "session.create".into(),
                idempotency_key: key.into(),
                payload_hash: format!("hash-{key}"),
                target: GatewayCommandTarget {
                    source_id: "source-1".into(),
                    source_epoch: "epoch-1".into(),
                    thread_key: None,
                    codex_thread_id: None,
                    expected_turn_id: None,
                    expected_request_id: None,
                    expected_request_version: None,
                },
                input_summary_json: json!({"mode":"fixture"}).to_string(),
                origin: GatewayCommandOrigin::WorkerControl,
            },
        )?;
        Ok(())
    }

    #[test]
    fn reservation_upgrade_is_atomic_and_thread_owner_is_unique_across_reopen() -> Result<()> {
        let temp = TempDir::new()?;
        let path = temp.path().join("observer.sqlite");
        let database = Database::open(&path)?;
        database.migrate()?;
        seed_command(&database, "command-1", "key-1")?;
        seed_command(&database, "command-2", "key-2")?;
        let mut connection = database.connect()?;
        let first = registration("worker-1", "command-1", None);
        assert!(matches!(
            database.register_session_worker_on(&mut connection, &first)?,
            RegisterSessionWorker::Created { .. }
        ));
        assert!(matches!(
            database.upgrade_thread_reservation_on(
                &mut connection,
                &first.primary_lease_id,
                &first.worker_id,
                1,
                "thread-1",
                Some("command-1"),
            )?,
            AcquireThreadLease::Acquired(_)
        ));
        assert!(matches!(
            database.acquire_thread_lease_on(
                &mut connection,
                "lease-child-1",
                "source-1",
                "epoch-1",
                "thread-child-1",
                "worker-1",
                "child",
                Some("command-1"),
            )?,
            AcquireThreadLease::Acquired(_)
        ));
        drop(connection);
        drop(database);

        let reopened = Database::open(&path)?;
        reopened.migrate()?;
        let mut connection = reopened.connect()?;
        assert!(matches!(
            reopened.register_session_worker_on(
                &mut connection,
                &registration("worker-2", "command-2", Some("thread-1")),
            )?,
            RegisterSessionWorker::ThreadOwned { worker_id } if worker_id == "worker-1"
        ));
        let leases = reopened.thread_leases_for_worker("worker-1")?;
        assert_eq!(leases.len(), 2);
        assert_eq!(leases[0].codex_thread_id.as_deref(), Some("thread-1"));
        assert_eq!(leases[0].state, "active");
        assert_eq!(leases[1].codex_thread_id.as_deref(), Some("thread-child-1"));
        Ok(())
    }

    #[test]
    fn pre_write_start_failure_releases_reservation_and_preserves_transitions() -> Result<()> {
        let temp = TempDir::new()?;
        let database = Database::open(&temp.path().join("observer.sqlite"))?;
        database.migrate()?;
        seed_command(&database, "command-failed", "key-failed")?;
        let registration = registration("worker-failed", "command-failed", None);
        let mut connection = database.connect()?;
        assert!(matches!(
            database.register_session_worker_on(&mut connection, &registration)?,
            RegisterSessionWorker::Created { .. }
        ));
        database.fail_session_before_write_on(
            &mut connection,
            &registration.worker_id,
            "SESSION_WORKER_SPAWN_FAILED",
            &registration.create_command_id,
        )?;
        drop(connection);

        let worker = database
            .session_worker(&registration.worker_id)?
            .context("missing failed worker")?;
        assert_eq!(worker.state, "failed");
        assert_eq!(
            worker.error_code.as_deref(),
            Some("SESSION_WORKER_SPAWN_FAILED")
        );
        let leases = database.thread_leases_for_worker(&registration.worker_id)?;
        assert_eq!(leases.len(), 1);
        assert_eq!(leases[0].state, "released");
        let mut connection = database.connect()?;
        let transaction = connection.transaction()?;
        validate_session_projections(&transaction)?;
        Ok(())
    }

    #[test]
    fn restart_recovery_orphans_live_state_without_replaying_or_releasing_ownership() -> Result<()>
    {
        let temp = TempDir::new()?;
        let database = Database::open(&temp.path().join("observer.sqlite"))?;
        database.migrate()?;
        seed_command(&database, "command-1", "key-1")?;
        let mut connection = database.connect()?;
        let registration = registration("worker-1", "command-1", Some("thread-1"));
        database.register_session_worker_on(&mut connection, &registration)?;
        database.transition_session_worker_on(
            &mut connection,
            &SessionWorkerTransition {
                worker_id: "worker-1".into(),
                expected_version: 1,
                to_state: "connecting".into(),
                // A live PID is deliberately used: recovery must never signal or adopt it,
                // because persisted PIDs can be reused by an unrelated process.
                pid: Some(std::process::id()),
                primary_thread_id: Some("thread-1".into()),
                error_code: None,
                reason_code: Some("spawned".into()),
                command_id: Some("command-1".into()),
            },
        )?;
        drop(connection);

        let report = database.recover_sessions_after_restart()?;
        assert_eq!(report.orphaned_workers, 1);
        assert_eq!(report.orphaned_thread_leases, 1);
        let worker = database.session_worker("worker-1")?.unwrap();
        assert_eq!(worker.state, "orphaned");
        assert_eq!(worker.error_code.as_deref(), Some("GATEWAY_RESTARTED"));
        assert_eq!(
            database.thread_leases_for_worker("worker-1")?[0].state,
            "orphaned"
        );
        assert_eq!(
            database.recover_sessions_after_restart()?.orphaned_workers,
            0
        );
        // SAFETY: signal 0 performs only an existence/permission probe on this test process.
        assert!(unsafe { libc::kill(std::process::id() as libc::pid_t, 0) } == 0);
        Ok(())
    }
}
