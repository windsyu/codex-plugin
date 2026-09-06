use crate::clock::now_ms;
use crate::domain::gateway::{
    GatewayCommandError, GatewayCommandRecord, GatewayCommandTarget, GatewayTransition,
    NewGatewayCommand, ReceiveGatewayCommand, transition_allowed,
};
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::json;
use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::time::{Duration, UNIX_EPOCH};

use super::{Database, GatewayCommandPage};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PendingRequestClaim {
    Claimed(Box<GatewayCommandRecord>),
    NotPending,
    AlreadyResolved,
    SourceEpochStale,
}

#[derive(Debug, Clone)]
pub(crate) struct NewImageUpload {
    pub upload_id: String,
    pub principal_id: String,
    pub mime_type: String,
    pub size_bytes: i64,
    pub keyed_fingerprint: String,
    pub relative_path: String,
    pub expires_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImageUploadRecord {
    pub upload_id: String,
    pub principal_id: String,
    pub mime_type: String,
    pub size_bytes: i64,
    pub keyed_fingerprint: String,
    pub relative_path: String,
    pub state: String,
    pub expires_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StageImageUpload {
    Created(ImageUploadRecord),
    Existing(ImageUploadRecord),
    Conflict,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClaimedImageUpload {
    pub upload_id: String,
    pub relative_path: String,
    pub keyed_fingerprint: String,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct GatewayRecoveryReport {
    pub closed_epochs: usize,
    pub failed_before_dispatch: usize,
    pub outcome_unknown: usize,
    pub image_paths: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct PendingRequestTarget {
    pub source_id: String,
    pub source_epoch: String,
    pub request_id: String,
    pub thread_key: Option<String>,
    pub codex_thread_id: Option<String>,
    pub request_type: String,
    pub state: String,
    pub request_version: i64,
}

impl Database {
    pub(crate) fn open_worker_connection_on(
        &self,
        connection: &mut Connection,
        worker_id: &str,
        source_id: &str,
        source_epoch: &str,
        connection_epoch: &str,
    ) -> Result<()> {
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT INTO worker_connection_epochs(
               worker_id,connection_epoch,source_id,source_epoch,state,opened_at_ms)
             VALUES (?1,?2,?3,?4,'open',?5)",
            params![
                worker_id,
                connection_epoch,
                source_id,
                source_epoch,
                now_ms()
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn close_worker_connection_on(
        &self,
        connection: &mut Connection,
        worker_id: &str,
        connection_epoch: &str,
        last_proxy_seq: u64,
        reason: &str,
    ) -> Result<()> {
        let last_proxy_seq = i64::try_from(last_proxy_seq).context("proxy sequence overflow")?;
        let state = match reason {
            "connection_failed" => "failed",
            "outcome_unknown" => "outcome_unknown",
            _ => "closed",
        };
        let changed = connection.execute(
            "UPDATE worker_connection_epochs
             SET state=?1,closed_at_ms=?2,close_reason=?3,last_proxy_seq=?4
             WHERE worker_id=?5 AND connection_epoch=?6 AND state='open'",
            params![
                state,
                now_ms(),
                reason,
                last_proxy_seq,
                worker_id,
                connection_epoch
            ],
        )?;
        if changed != 1 {
            anyhow::bail!("worker connection epoch is not open");
        }
        Ok(())
    }

    pub(crate) fn image_staging_dir(&self) -> std::path::PathBuf {
        self.path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join("image-uploads")
    }

    pub(crate) fn recover_gateway_after_restart(&self) -> Result<GatewayRecoveryReport> {
        let mut connection = self.connect()?;
        let open_epochs = {
            let mut statement = connection.prepare(
                "SELECT e.source_id,e.epoch_id FROM source_epochs e JOIN sources s ON s.source_id=e.source_id
                 WHERE s.kind IN ('app_server','session_runtime') AND e.closed_at_ms IS NULL",
            )?;
            statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        for (source_id, epoch_id) in &open_epochs {
            self.close_live_epoch_on(&mut connection, source_id, epoch_id, "gateway_restart")?;
        }
        connection.execute(
            "UPDATE sources SET status='offline',last_error_json=?1,updated_at_ms=?2
             WHERE kind IN ('app_server','session_runtime')",
            params![
                json!({"message":"gateway restarted; live source must reconnect"}).to_string(),
                now_ms()
            ],
        )?;

        let transaction = connection.transaction()?;
        let inconsistent: Option<(String, String, Option<String>)> = transaction
            .query_row(
                "SELECT c.command_id,c.state,
                   (SELECT t.to_state FROM command_transitions t WHERE t.command_id=c.command_id
                    ORDER BY t.transition_seq DESC LIMIT 1)
                 FROM gateway_commands c
                 WHERE NOT EXISTS(SELECT 1 FROM command_transitions t WHERE t.command_id=c.command_id)
                    OR c.state<>(SELECT t.to_state FROM command_transitions t WHERE t.command_id=c.command_id
                       ORDER BY t.transition_seq DESC LIMIT 1)
                 LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        if let Some((command_id, state, transition_state)) = inconsistent {
            bail!(
                "Gateway command ledger is inconsistent for {command_id}: current={state}, transition={}",
                transition_state.as_deref().unwrap_or("missing")
            );
        }
        let command_ids = {
            let mut statement = transaction.prepare(
                "SELECT command_id FROM gateway_commands
                 WHERE state IN ('received','authorized','dispatching','accepted_by_source','running')
                 ORDER BY created_at_ms,command_id",
            )?;
            statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        let mut report = GatewayRecoveryReport {
            closed_epochs: open_epochs.len(),
            ..GatewayRecoveryReport::default()
        };
        let occurred_at_ms = now_ms();
        for command_id in command_ids {
            let current = gateway_command_with_private_on(&transaction, &command_id)?
                .context("recoverable Gateway command disappeared")?;
            let before_dispatch =
                matches!(current.record.state.as_str(), "received" | "authorized");
            let (to_state, error_code, error_message, reason_code, decision, outcome) =
                if before_dispatch {
                    report.failed_before_dispatch += 1;
                    (
                        "failed",
                        "GATEWAY_RESTARTED",
                        "the Gateway restarted before mutation dispatch began",
                        "GATEWAY_RESTARTED_BEFORE_DISPATCH",
                        "deny",
                        "failed",
                    )
                } else {
                    report.outcome_unknown += 1;
                    (
                        "outcome_unknown",
                        "OUTCOME_UNKNOWN",
                        "the Gateway restarted after mutation dispatch may have begun",
                        "GATEWAY_RESTARTED_AFTER_DISPATCH",
                        "allow",
                        "outcome_unknown",
                    )
                };
            let changed = transaction.execute(
                "UPDATE gateway_commands SET state=?1,error_code=?2,error_message=?3,updated_at_ms=?4
                 WHERE command_id=?5 AND state=?6",
                params![to_state,error_code,error_message,occurred_at_ms,command_id,current.record.state],
            )?;
            if changed != 1 {
                bail!("Gateway command changed during restart recovery");
            }
            append_transition(
                &transaction,
                &command_id,
                Some(&current.record.state),
                to_state,
                occurred_at_ms,
                Some(reason_code),
            )?;
            append_audit_fields(
                &transaction,
                &command_id,
                &current.record.principal_id,
                &current.record.capability,
                &current.record.target,
                occurred_at_ms,
                decision,
                outcome,
                &current.payload_hash,
                &current.input_summary_json,
            )?;
            transaction.execute(
                "UPDATE pending_requests SET state='source_disconnected'
                 WHERE state='resolving' AND resolving_command_id=?1",
                [&command_id],
            )?;
            if before_dispatch {
                let images = {
                    let mut statement = transaction.prepare(
                        "SELECT upload_id,relative_path FROM image_uploads
                         WHERE command_id=?1 AND state IN ('staged','attached')",
                    )?;
                    statement
                        .query_map([&command_id], |row| {
                            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                        })?
                        .collect::<rusqlite::Result<Vec<_>>>()?
                };
                for (upload_id, relative_path) in images {
                    validate_image_relative_path(&upload_id, &relative_path)?;
                    report.image_paths.push(
                        self.image_staging_dir()
                            .join(relative_path)
                            .to_string_lossy()
                            .to_string(),
                    );
                }
                transaction.execute(
                    "UPDATE image_uploads SET state='deleted',deleted_at_ms=?1
                     WHERE command_id=?2 AND state IN ('staged','attached')",
                    params![occurred_at_ms, command_id],
                )?;
            }
        }
        transaction.commit()?;
        Ok(report)
    }

    pub(crate) fn sweep_expired_image_uploads(&self, cutoff_ms: i64) -> Result<usize> {
        let mut connection = self.connect()?;
        let transaction = connection.transaction()?;
        let uploads = {
            let mut statement = transaction
                .prepare("SELECT upload_id,relative_path,state,expires_at_ms FROM image_uploads")?;
            statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        let mut known_paths = BTreeMap::new();
        let mut paths_to_remove = Vec::new();
        let mut expired_count = 0_usize;
        for (upload_id, relative_path, state, expires_at_ms) in uploads {
            validate_image_relative_path(&upload_id, &relative_path)?;
            known_paths.insert(relative_path.clone(), state.clone());
            if matches!(state.as_str(), "deleted" | "expired")
                || (matches!(state.as_str(), "staged" | "attached") && expires_at_ms <= cutoff_ms)
            {
                paths_to_remove.push(relative_path);
            }
            if matches!(state.as_str(), "staged" | "attached") && expires_at_ms <= cutoff_ms {
                expired_count += 1;
            }
        }
        transaction.execute(
            "UPDATE image_uploads SET state='expired',deleted_at_ms=?1
             WHERE state IN ('staged','attached') AND expires_at_ms<=?1",
            [cutoff_ms],
        )?;
        transaction.commit()?;

        let directory = self.image_staging_dir();
        match std::fs::symlink_metadata(&directory) {
            Ok(_) => crate::permissions::prepare_private_dir(&directory, "image staging")?,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(expired_count),
            Err(error) => return Err(error.into()),
        }
        let mut removed = expired_count;
        for relative in paths_to_remove {
            match std::fs::remove_file(directory.join(relative)) {
                Ok(()) => {}
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        let orphan_cutoff_ms =
            cutoff_ms.saturating_sub(Duration::from_secs(24 * 60 * 60).as_millis() as i64);
        for entry in std::fs::read_dir(&directory)? {
            let entry = entry?;
            let file_name = entry.file_name().to_string_lossy().to_string();
            if known_paths.contains_key(&file_name) {
                continue;
            }
            let metadata = std::fs::symlink_metadata(entry.path())?;
            if metadata.file_type().is_symlink() {
                std::fs::remove_file(entry.path())?;
                removed += 1;
                continue;
            }
            if !metadata.is_file() {
                bail!("image staging contains a non-file orphan");
            }
            let modified_ms = metadata
                .modified()?
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .min(i64::MAX as u128) as i64;
            if modified_ms <= orphan_cutoff_ms {
                std::fs::remove_file(entry.path())?;
                removed += 1;
            }
        }
        Ok(removed)
    }

    pub(crate) fn stage_image_upload_on(
        &self,
        connection: &mut Connection,
        upload: &NewImageUpload,
    ) -> Result<StageImageUpload> {
        let transaction = connection.transaction()?;
        let existing = image_upload_on(&transaction, &upload.upload_id)?;
        if let Some(existing) = existing {
            return Ok(if image_upload_matches(&existing, upload) {
                StageImageUpload::Existing(existing)
            } else {
                StageImageUpload::Conflict
            });
        }
        let created_at_ms = now_ms();
        transaction.execute(
            "INSERT INTO image_uploads(upload_id,principal_id,mime_type,size_bytes,keyed_fingerprint,
               relative_path,state,created_at_ms,expires_at_ms)
             VALUES (?1,?2,?3,?4,?5,?6,'staged',?7,?8)",
            params![upload.upload_id,upload.principal_id,upload.mime_type,upload.size_bytes,
                upload.keyed_fingerprint,upload.relative_path,created_at_ms,upload.expires_at_ms],
        )?;
        transaction.commit()?;
        Ok(StageImageUpload::Created(ImageUploadRecord {
            upload_id: upload.upload_id.clone(),
            principal_id: upload.principal_id.clone(),
            mime_type: upload.mime_type.clone(),
            size_bytes: upload.size_bytes,
            keyed_fingerprint: upload.keyed_fingerprint.clone(),
            relative_path: upload.relative_path.clone(),
            state: "staged".into(),
            expires_at_ms: upload.expires_at_ms,
        }))
    }

    #[allow(dead_code)] // Retained for legacy image-staging migration tests.
    pub(crate) fn image_upload(&self, upload_id: &str) -> Result<Option<ImageUploadRecord>> {
        let connection = self.read_connection()?;
        image_upload_on(&connection, upload_id)
    }

    pub(crate) fn claim_image_uploads_on(
        &self,
        connection: &mut Connection,
        command_id: &str,
        principal_id: &str,
        upload_ids: &[String],
    ) -> Result<Vec<ClaimedImageUpload>> {
        if upload_ids.len() > 4 {
            bail!("a message accepts at most four images");
        }
        let transaction = connection.transaction()?;
        let mut paths = Vec::with_capacity(upload_ids.len());
        let mut total = 0_i64;
        for upload_id in upload_ids {
            let (path, size, fingerprint) = transaction
                .query_row(
                    "SELECT relative_path,size_bytes,keyed_fingerprint FROM image_uploads
                 WHERE upload_id=?1 AND principal_id=?2 AND state='staged' AND expires_at_ms>?3",
                    params![upload_id, principal_id, now_ms()],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    },
                )
                .with_context(|| format!("image upload {upload_id} is unavailable"))?;
            total += size;
            paths.push(ClaimedImageUpload {
                upload_id: upload_id.clone(),
                relative_path: path,
                keyed_fingerprint: fingerprint,
            });
        }
        if total > 50 * 1024 * 1024 {
            bail!("message images exceed 50 MiB");
        }
        for upload_id in upload_ids {
            let changed = transaction.execute(
                "UPDATE image_uploads SET state='attached',command_id=?1
                 WHERE upload_id=?2 AND principal_id=?3 AND state='staged'",
                params![command_id, upload_id, principal_id],
            )?;
            if changed != 1 {
                bail!("image upload claim lost a concurrent race");
            }
        }
        transaction.commit()?;
        Ok(paths)
    }

    pub(crate) fn cleanup_command_images_on(
        &self,
        connection: &mut Connection,
        command_id: &str,
    ) -> Result<Vec<String>> {
        Ok(cleanup_image_paths(connection, "command_id", command_id)?
            .into_iter()
            .map(|path| {
                self.image_staging_dir()
                    .join(path)
                    .to_string_lossy()
                    .to_string()
            })
            .collect())
    }

    #[allow(dead_code)] // Retained for legacy image-staging migration tests.
    pub(crate) fn cleanup_turn_images_on(
        &self,
        connection: &mut Connection,
        turn_id: &str,
    ) -> Result<Vec<String>> {
        let transaction = connection.transaction()?;
        let paths = {
            let mut statement = transaction.prepare(
                "SELECT i.relative_path FROM image_uploads i JOIN gateway_commands g ON g.command_id=i.command_id
                 WHERE i.state='attached' AND (g.expected_turn_id=?1 OR json_extract(g.result_summary_json,'$.turnId')=?1)"
            )?;
            statement
                .query_map([turn_id], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        transaction.execute(
            "UPDATE image_uploads SET state='deleted',deleted_at_ms=?1 WHERE state='attached' AND command_id IN
             (SELECT command_id FROM gateway_commands WHERE expected_turn_id=?2 OR json_extract(result_summary_json,'$.turnId')=?2)",
            params![now_ms(),turn_id],
        )?;
        transaction.commit()?;
        Ok(paths
            .into_iter()
            .map(|path| {
                self.image_staging_dir()
                    .join(path)
                    .to_string_lossy()
                    .to_string()
            })
            .collect())
    }
    pub(crate) fn pending_request_target(
        &self,
        source_id: &str,
        source_epoch: &str,
        request_id: &str,
    ) -> Result<Option<PendingRequestTarget>> {
        let connection = self.read_connection()?;
        connection
            .query_row(
                "SELECT p.source_id,p.epoch_id,p.request_id,p.thread_key,t.codex_thread_id,
                   p.request_type,p.state,p.request_version
                 FROM pending_requests p LEFT JOIN threads t ON t.thread_key=p.thread_key
                 WHERE p.source_id=?1 AND p.epoch_id=?2 AND p.request_id=?3",
                params![source_id, source_epoch, request_id],
                |row| {
                    Ok(PendingRequestTarget {
                        source_id: row.get(0)?,
                        source_epoch: row.get(1)?,
                        request_id: row.get(2)?,
                        thread_key: row.get(3)?,
                        codex_thread_id: row.get(4)?,
                        request_type: row.get(5)?,
                        state: row.get(6)?,
                        request_version: row.get(7)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub(crate) fn claim_pending_request_on(
        &self,
        connection: &mut Connection,
        command_id: &str,
    ) -> Result<PendingRequestClaim> {
        let transaction = connection.transaction()?;
        let command = gateway_command_with_private_on(&transaction, command_id)?
            .context("Gateway command not found")?;
        if command.record.state != "authorized" {
            bail!("pending request claim requires an authorized Gateway command");
        }
        let target = &command.record.target;
        let request_id = target
            .expected_request_id
            .as_deref()
            .context("pending request command lacks expectedRequestId")?;
        let expected_version = target
            .expected_request_version
            .context("pending request command lacks expectedRequestVersion")?;
        let pending = transaction
            .query_row(
                "SELECT state,request_version FROM pending_requests
                 WHERE source_id=?1 AND epoch_id=?2 AND request_id=?3",
                params![target.source_id, target.source_epoch, request_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?;
        let Some((state, version)) = pending else {
            let another_epoch = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM pending_requests WHERE source_id=?1 AND request_id=?2)",
                params![target.source_id, request_id],
                |row| row.get::<_, bool>(0),
            )?;
            return Ok(if another_epoch {
                PendingRequestClaim::SourceEpochStale
            } else {
                PendingRequestClaim::NotPending
            });
        };
        if matches!(state.as_str(), "resolved" | "resolving") {
            return Ok(PendingRequestClaim::AlreadyResolved);
        }
        if state == "source_disconnected" {
            return Ok(PendingRequestClaim::SourceEpochStale);
        }
        if state != "pending" || version != expected_version {
            return Ok(PendingRequestClaim::NotPending);
        }

        let occurred_at_ms = now_ms();
        let claimed = transaction.execute(
            "UPDATE pending_requests SET state='resolving',resolving_command_id=?1,
               resolving_started_at_ms=?2
             WHERE source_id=?3 AND epoch_id=?4 AND request_id=?5
               AND state='pending' AND request_version=?6",
            params![
                command_id,
                occurred_at_ms,
                target.source_id,
                target.source_epoch,
                request_id,
                expected_version,
            ],
        )?;
        if claimed != 1 {
            return Ok(PendingRequestClaim::AlreadyResolved);
        }
        let transitioned = transaction.execute(
            "UPDATE gateway_commands SET state='dispatching',updated_at_ms=?1
             WHERE command_id=?2 AND state='authorized'",
            params![occurred_at_ms, command_id],
        )?;
        if transitioned != 1 {
            bail!("Gateway command state changed during pending request claim");
        }
        append_transition(
            &transaction,
            command_id,
            Some("authorized"),
            "dispatching",
            occurred_at_ms,
            None,
        )?;
        append_audit_fields(
            &transaction,
            command_id,
            &command.record.principal_id,
            &command.record.capability,
            target,
            occurred_at_ms,
            "allow",
            "dispatching",
            &command.payload_hash,
            &command.input_summary_json,
        )?;
        let record = gateway_command_on(&transaction, command_id)?
            .context("claimed Gateway command disappeared")?;
        transaction.commit()?;
        Ok(PendingRequestClaim::Claimed(Box::new(record)))
    }

    pub(crate) fn complete_pending_request_action_on(
        &self,
        connection: &mut Connection,
        command_id: &str,
        request_id: &str,
    ) -> Result<GatewayCommandRecord> {
        let transaction = connection.transaction()?;
        let command = gateway_command_with_private_on(&transaction, command_id)?
            .context("Gateway command not found")?;
        if command.record.state == "completed" {
            return Ok(command.record);
        }
        if command.record.state != "accepted_by_source" {
            bail!("pending request completion requires an accepted Gateway command");
        }
        if command.record.target.expected_request_id.as_deref() != Some(request_id) {
            bail!("pending request completion does not match Gateway command target");
        }
        let occurred_at_ms = now_ms();
        let pending_changed = transaction.execute(
            "UPDATE pending_requests
             SET state='resolved',request_version=request_version+1,resolving_started_at_ms=NULL
             WHERE source_id=?1 AND epoch_id=?2 AND request_id=?3
               AND state='resolving' AND resolving_command_id=?4",
            params![
                command.record.target.source_id,
                command.record.target.source_epoch,
                request_id,
                command_id,
            ],
        )?;
        if pending_changed != 1 {
            bail!("pending request is no longer resolving for this command");
        }
        let command_changed = transaction.execute(
            "UPDATE gateway_commands
             SET state='completed',result_summary_json=?1,updated_at_ms=?2
             WHERE command_id=?3 AND state='accepted_by_source'",
            params![
                json!({"requestId":request_id,"resolved":true}).to_string(),
                occurred_at_ms,
                command_id
            ],
        )?;
        if command_changed != 1 {
            bail!("Gateway command state changed during request completion");
        }
        append_transition(
            &transaction,
            command_id,
            Some("accepted_by_source"),
            "completed",
            occurred_at_ms,
            Some("pending_request_response_written"),
        )?;
        append_audit_fields(
            &transaction,
            command_id,
            &command.record.principal_id,
            &command.record.capability,
            &command.record.target,
            occurred_at_ms,
            "allow",
            "completed",
            &command.payload_hash,
            &command.input_summary_json,
        )?;
        let record = gateway_command_on(&transaction, command_id)?
            .context("completed request command disappeared")?;
        transaction.commit()?;
        Ok(record)
    }

    pub(crate) fn receive_gateway_command_on(
        &self,
        connection: &mut Connection,
        command: &NewGatewayCommand,
    ) -> Result<ReceiveGatewayCommand> {
        let transaction = connection.transaction()?;
        let existing = transaction
            .query_row(
                "SELECT payload_hash,command_id FROM gateway_commands
                 WHERE principal_id=?1 AND capability=?2 AND idempotency_key=?3",
                params![
                    command.principal_id,
                    command.capability,
                    command.idempotency_key
                ],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        if let Some((payload_hash, command_id)) = existing {
            let record = gateway_command_on(&transaction, &command_id)?
                .context("idempotency key references a missing Gateway command")?;
            return if payload_hash == command.payload_hash {
                Ok(ReceiveGatewayCommand::Existing(record))
            } else {
                Ok(ReceiveGatewayCommand::Conflict)
            };
        }

        let occurred_at_ms = now_ms();
        transaction.execute(
            "INSERT INTO gateway_commands(
               command_id,principal_id,capability,idempotency_key,payload_hash,source_id,source_epoch,
               thread_key,codex_thread_id,expected_turn_id,expected_request_id,
               expected_request_version,input_summary_json,origin,state,created_at_ms,updated_at_ms)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,'received',?15,?15)",
            params![
                command.command_id,
                command.principal_id,
                command.capability,
                command.idempotency_key,
                command.payload_hash,
                command.target.source_id,
                command.target.source_epoch,
                command.target.thread_key,
                command.target.codex_thread_id,
                command.target.expected_turn_id,
                command.target.expected_request_id,
                command.target.expected_request_version,
                command.input_summary_json,
                command.origin.as_str(),
                occurred_at_ms,
            ],
        )?;
        append_transition(
            &transaction,
            &command.command_id,
            None,
            "received",
            occurred_at_ms,
            None,
        )?;
        append_audit(&transaction, command, occurred_at_ms, "received", "pending")?;
        let record = gateway_command_on(&transaction, &command.command_id)?
            .context("new Gateway command was not persisted")?;
        transaction.commit()?;
        Ok(ReceiveGatewayCommand::Created(record))
    }

    pub(crate) fn transition_gateway_command_on(
        &self,
        connection: &mut Connection,
        transition: &GatewayTransition,
    ) -> Result<GatewayCommandRecord> {
        let transaction = connection.transaction()?;
        let current = gateway_command_with_private_on(&transaction, &transition.command_id)?
            .context("Gateway command not found")?;
        if current.record.state == transition.to_state {
            return Ok(current.record);
        }
        if !transition_allowed(&current.record.state, &transition.to_state) {
            bail!(
                "invalid Gateway command transition {} -> {}",
                current.record.state,
                transition.to_state
            );
        }
        let occurred_at_ms = now_ms();
        let changed = transaction.execute(
            "UPDATE gateway_commands SET state=?1,result_summary_json=COALESCE(?2,result_summary_json),
               error_code=?3,error_message=?4,updated_at_ms=?5
             WHERE command_id=?6 AND state=?7",
            params![
                transition.to_state,
                transition.result_summary_json,
                transition.error_code,
                transition.error_message,
                occurred_at_ms,
                transition.command_id,
                current.record.state,
            ],
        )?;
        if changed != 1 {
            bail!("Gateway command state changed concurrently");
        }
        append_transition(
            &transaction,
            &transition.command_id,
            Some(&current.record.state),
            &transition.to_state,
            occurred_at_ms,
            transition.reason_code.as_deref(),
        )?;
        append_audit_fields(
            &transaction,
            &transition.command_id,
            &current.record.principal_id,
            &current.record.capability,
            &current.record.target,
            occurred_at_ms,
            &transition.decision,
            &transition.outcome,
            &current.payload_hash,
            &current.input_summary_json,
        )?;
        if current.record.target.expected_request_id.is_some() {
            match transition.to_state.as_str() {
                "rejected" | "failed" | "cancelled" if current.record.state == "dispatching" => {
                    transaction.execute(
                        "UPDATE pending_requests
                         SET state='pending',resolving_command_id=NULL,resolving_started_at_ms=NULL
                         WHERE state='resolving' AND resolving_command_id=?1",
                        [&transition.command_id],
                    )?;
                }
                "outcome_unknown" => {
                    transaction.execute(
                        "UPDATE pending_requests
                         SET state='outcome_unknown',resolving_started_at_ms=NULL
                         WHERE state='resolving' AND resolving_command_id=?1",
                        [&transition.command_id],
                    )?;
                }
                _ => {}
            }
        }
        let record = gateway_command_on(&transaction, &transition.command_id)?
            .context("transitioned Gateway command disappeared")?;
        transaction.commit()?;
        Ok(record)
    }

    pub fn gateway_command(&self, command_id: &str) -> Result<Option<GatewayCommandRecord>> {
        let connection = self.read_connection()?;
        gateway_command_on(&connection, command_id)
    }

    pub fn gateway_commands_page(
        &self,
        thread_key: Option<&str>,
        state: Option<&str>,
        as_of_rowid: Option<i64>,
        after: Option<(i64, &str)>,
        limit: usize,
    ) -> Result<GatewayCommandPage> {
        let connection = self.read_connection()?;
        let as_of_rowid = match as_of_rowid {
            Some(value) => value,
            None => connection.query_row(
                "SELECT COALESCE(MAX(rowid),0) FROM gateway_commands",
                [],
                |row| row.get(0),
            )?,
        };
        let (after_created_at_ms, after_command_id) = after
            .map(|(created_at_ms, command_id)| (Some(created_at_ms), Some(command_id)))
            .unwrap_or((None, None));
        let command_ids = {
            let mut statement = connection.prepare(
                "SELECT command_id FROM gateway_commands
                 WHERE rowid<=?1
                   AND (?2 IS NULL OR thread_key=?2)
                   AND (?3 IS NULL OR state=?3)
                   AND (?4 IS NULL OR created_at_ms<?4
                     OR (created_at_ms=?4 AND command_id<?5))
                 ORDER BY created_at_ms DESC,command_id DESC LIMIT ?6",
            )?;
            statement
                .query_map(
                    params![
                        as_of_rowid,
                        thread_key,
                        state,
                        after_created_at_ms,
                        after_command_id,
                        limit as i64,
                    ],
                    |row| row.get::<_, String>(0),
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        let commands = command_ids
            .iter()
            .map(|command_id| {
                gateway_command_on(&connection, command_id)?
                    .context("paged Gateway command disappeared")
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(GatewayCommandPage {
            as_of_rowid,
            commands,
        })
    }
}

struct PrivateGatewayCommand {
    record: GatewayCommandRecord,
    payload_hash: String,
    input_summary_json: String,
}

fn cleanup_image_paths(
    connection: &mut Connection,
    column: &str,
    value: &str,
) -> Result<Vec<String>> {
    let transaction = connection.transaction()?;
    let sql = format!(
        "SELECT relative_path FROM image_uploads WHERE state IN ('staged','attached') AND {column}=?1"
    );
    let paths = {
        let mut statement = transaction.prepare(&sql)?;
        statement
            .query_map([value], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let sql = format!(
        "UPDATE image_uploads SET state='deleted',deleted_at_ms=?1 WHERE state IN ('staged','attached') AND {column}=?2"
    );
    transaction.execute(&sql, params![now_ms(), value])?;
    transaction.commit()?;
    Ok(paths)
}

fn image_upload_on(connection: &Connection, upload_id: &str) -> Result<Option<ImageUploadRecord>> {
    connection
        .query_row(
            "SELECT upload_id,principal_id,mime_type,size_bytes,keyed_fingerprint,relative_path,
               state,expires_at_ms FROM image_uploads WHERE upload_id=?1",
            [upload_id],
            |row| {
                Ok(ImageUploadRecord {
                    upload_id: row.get(0)?,
                    principal_id: row.get(1)?,
                    mime_type: row.get(2)?,
                    size_bytes: row.get(3)?,
                    keyed_fingerprint: row.get(4)?,
                    relative_path: row.get(5)?,
                    state: row.get(6)?,
                    expires_at_ms: row.get(7)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

fn image_upload_matches(existing: &ImageUploadRecord, upload: &NewImageUpload) -> bool {
    existing.principal_id == upload.principal_id
        && existing.mime_type == upload.mime_type
        && existing.size_bytes == upload.size_bytes
        && existing.keyed_fingerprint == upload.keyed_fingerprint
        && existing.relative_path == upload.relative_path
}

fn validate_image_relative_path(upload_id: &str, relative_path: &str) -> Result<()> {
    if relative_path != format!("{upload_id}.bin") {
        bail!("image upload metadata contains an invalid relative path");
    }
    Ok(())
}

fn gateway_command_with_private_on(
    connection: &Connection,
    command_id: &str,
) -> Result<Option<PrivateGatewayCommand>> {
    connection
        .query_row(
            "SELECT command_id,principal_id,capability,source_id,source_epoch,thread_key,
               codex_thread_id,expected_turn_id,expected_request_id,expected_request_version,
               state,result_summary_json,error_code,error_message,created_at_ms,updated_at_ms,
               payload_hash,input_summary_json
             FROM gateway_commands WHERE command_id=?1",
            [command_id],
            |row| {
                let error_code = row.get::<_, Option<String>>(12)?;
                let error_message = row.get::<_, Option<String>>(13)?;
                Ok(PrivateGatewayCommand {
                    record: GatewayCommandRecord {
                        command_id: row.get(0)?,
                        principal_id: row.get(1)?,
                        capability: row.get(2)?,
                        target: GatewayCommandTarget {
                            source_id: row.get(3)?,
                            source_epoch: row.get(4)?,
                            thread_key: row.get(5)?,
                            codex_thread_id: row.get(6)?,
                            expected_turn_id: row.get(7)?,
                            expected_request_id: row.get(8)?,
                            expected_request_version: row.get(9)?,
                        },
                        state: row.get(10)?,
                        result: row
                            .get::<_, Option<String>>(11)?
                            .and_then(|value| serde_json::from_str(&value).ok()),
                        error: error_code.map(|code| GatewayCommandError {
                            code,
                            message: error_message
                                .unwrap_or_else(|| "Gateway command failed".into()),
                        }),
                        created_at_ms: row.get(14)?,
                        updated_at_ms: row.get(15)?,
                    },
                    payload_hash: row.get(16)?,
                    input_summary_json: row.get(17)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

fn gateway_command_on(
    connection: &Connection,
    command_id: &str,
) -> Result<Option<GatewayCommandRecord>> {
    Ok(gateway_command_with_private_on(connection, command_id)?.map(|value| value.record))
}

fn append_transition(
    transaction: &Transaction<'_>,
    command_id: &str,
    from_state: Option<&str>,
    to_state: &str,
    occurred_at_ms: i64,
    reason_code: Option<&str>,
) -> Result<()> {
    transaction.execute(
        "INSERT INTO command_transitions(command_id,from_state,to_state,occurred_at_ms,
           reason_code,details_summary_json) VALUES (?1,?2,?3,?4,?5,?6)",
        params![
            command_id,
            from_state,
            to_state,
            occurred_at_ms,
            reason_code,
            json!({}).to_string()
        ],
    )?;
    Ok(())
}

fn append_audit(
    transaction: &Transaction<'_>,
    command: &NewGatewayCommand,
    occurred_at_ms: i64,
    decision: &str,
    outcome: &str,
) -> Result<()> {
    append_audit_fields(
        transaction,
        &command.command_id,
        &command.principal_id,
        &command.capability,
        &command.target,
        occurred_at_ms,
        decision,
        outcome,
        &command.payload_hash,
        &command.input_summary_json,
    )
}

#[allow(clippy::too_many_arguments)]
fn append_audit_fields(
    transaction: &Transaction<'_>,
    command_id: &str,
    principal_id: &str,
    capability: &str,
    target: &GatewayCommandTarget,
    occurred_at_ms: i64,
    decision: &str,
    outcome: &str,
    payload_hash: &str,
    input_summary_json: &str,
) -> Result<()> {
    transaction.execute(
        "INSERT INTO control_audit(command_id,principal_id,capability,source_id,source_epoch,
           thread_key,decision,outcome,payload_hash,input_summary_json,occurred_at_ms)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        params![
            command_id,
            principal_id,
            capability,
            target.source_id,
            target.source_epoch,
            target.thread_key,
            decision,
            outcome,
            payload_hash,
            input_summary_json,
            occurred_at_ms,
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::gateway::{GatewayCommandOrigin, GatewayCommandTarget, NewGatewayCommand};
    use tempfile::TempDir;

    fn command(id: &str, key: &str, hash: &str) -> NewGatewayCommand {
        NewGatewayCommand {
            command_id: id.into(),
            principal_id: "local_bearer".into(),
            capability: "turn.start".into(),
            idempotency_key: key.into(),
            payload_hash: hash.into(),
            target: GatewayCommandTarget {
                source_id: "source".into(),
                source_epoch: "epoch".into(),
                thread_key: Some("thread-key".into()),
                codex_thread_id: Some("thread".into()),
                expected_turn_id: None,
                expected_request_id: None,
                expected_request_version: None,
            },
            input_summary_json: json!({"textBytes":12}).to_string(),
            origin: GatewayCommandOrigin::LegacyApi,
        }
    }

    fn request_command(
        id: &str,
        key: &str,
        epoch: &str,
        request_id: &str,
        version: i64,
    ) -> NewGatewayCommand {
        let mut command = command(id, key, id);
        command.capability = "request.action".into();
        command.target.source_epoch = epoch.into();
        command.target.expected_request_id = Some(request_id.into());
        command.target.expected_request_version = Some(version);
        command.input_summary_json = json!({"jsonBytes":64}).to_string();
        command
    }

    fn authorize(
        database: &Database,
        connection: &mut Connection,
        command: &NewGatewayCommand,
    ) -> Result<()> {
        database.receive_gateway_command_on(connection, command)?;
        database.transition_gateway_command_on(
            connection,
            &GatewayTransition {
                command_id: command.command_id.clone(),
                to_state: "authorized".into(),
                result_summary_json: None,
                error_code: None,
                error_message: None,
                reason_code: None,
                decision: "allow".into(),
                outcome: "authorized".into(),
            },
        )?;
        Ok(())
    }

    fn stage_image(
        database: &Database,
        connection: &mut Connection,
        upload_id: &str,
        size_bytes: i64,
        expires_at_ms: i64,
    ) -> Result<()> {
        let staged = database.stage_image_upload_on(
            connection,
            &NewImageUpload {
                upload_id: upload_id.into(),
                principal_id: "local_bearer".into(),
                mime_type: "image/png".into(),
                size_bytes,
                keyed_fingerprint: format!("fingerprint-{upload_id}"),
                relative_path: format!("{upload_id}.bin"),
                expires_at_ms,
            },
        )?;
        assert!(matches!(staged, StageImageUpload::Created(_)));
        Ok(())
    }

    #[test]
    fn receive_is_idempotent_conflict_safe_and_survives_reopen() -> Result<()> {
        let temp = TempDir::new()?;
        let database = Database::open(&temp.path().join("observer.sqlite"))?;
        database.migrate()?;
        let mut connection = database.connect()?;
        let created = database
            .receive_gateway_command_on(&mut connection, &command("command-1", "key", "hash-a"))?;
        assert!(matches!(created, ReceiveGatewayCommand::Created(_)));
        let replay = database
            .receive_gateway_command_on(&mut connection, &command("command-2", "key", "hash-a"))?;
        assert!(matches!(replay, ReceiveGatewayCommand::Existing(_)));
        let conflict = database
            .receive_gateway_command_on(&mut connection, &command("command-3", "key", "hash-b"))?;
        assert!(matches!(conflict, ReceiveGatewayCommand::Conflict));
        drop(connection);

        let reopened = Database::open(&temp.path().join("observer.sqlite"))?;
        let record = reopened
            .gateway_command("command-1")?
            .context("missing command")?;
        assert_eq!(record.state, "received");
        let connection = reopened.connect()?;
        let counts: (i64, i64, i64) = connection.query_row(
            "SELECT (SELECT COUNT(*) FROM gateway_commands),
                    (SELECT COUNT(*) FROM command_transitions),
                    (SELECT COUNT(*) FROM control_audit)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert_eq!(counts, (1, 1, 1));
        Ok(())
    }

    #[test]
    fn transitions_validate_state_and_append_audit_without_message_body() -> Result<()> {
        let temp = TempDir::new()?;
        let database = Database::open(&temp.path().join("observer.sqlite"))?;
        database.migrate()?;
        let mut connection = database.connect()?;
        database.receive_gateway_command_on(&mut connection, &command("command", "key", "hash"))?;
        let transition = |to_state: &str| GatewayTransition {
            command_id: "command".into(),
            to_state: to_state.into(),
            result_summary_json: None,
            error_code: None,
            error_message: None,
            reason_code: None,
            decision: "allow".into(),
            outcome: to_state.into(),
        };
        database.transition_gateway_command_on(&mut connection, &transition("authorized"))?;
        assert!(
            database
                .transition_gateway_command_on(&mut connection, &transition("completed"))
                .is_err()
        );
        let stored: String = connection.query_row(
            "SELECT input_summary_json FROM gateway_commands WHERE command_id='command'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(stored, json!({"textBytes":12}).to_string());
        let audit_count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM control_audit WHERE command_id='command'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(audit_count, 2);
        Ok(())
    }

    #[test]
    fn pending_request_claim_is_single_winner_and_atomic_with_dispatch() -> Result<()> {
        let temp = TempDir::new()?;
        let database = Database::open(&temp.path().join("observer.sqlite"))?;
        database.migrate()?;
        let mut connection = database.connect()?;
        connection.execute(
            "INSERT INTO pending_requests(source_id,epoch_id,request_id,thread_key,request_type,
               state,request_event_seq,payload_json,request_version)
             VALUES ('source','epoch','request','thread-key','user_input','pending',1,'{}',3)",
            [],
        )?;
        let first = request_command("first", "key-first", "epoch", "request", 3);
        let second = request_command("second", "key-second", "epoch", "request", 3);
        authorize(&database, &mut connection, &first)?;
        authorize(&database, &mut connection, &second)?;

        let first_claim = database.claim_pending_request_on(&mut connection, "first")?;
        assert!(matches!(first_claim, PendingRequestClaim::Claimed(_)));
        let second_claim = database.claim_pending_request_on(&mut connection, "second")?;
        assert_eq!(second_claim, PendingRequestClaim::AlreadyResolved);
        let (request_state, resolver, command_state): (String, Option<String>, String) = connection
            .query_row(
                "SELECT p.state,p.resolving_command_id,c.state FROM pending_requests p
                 JOIN gateway_commands c ON c.command_id='first'
                 WHERE p.source_id='source' AND p.epoch_id='epoch' AND p.request_id='request'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
        assert_eq!(request_state, "resolving");
        assert_eq!(resolver.as_deref(), Some("first"));
        assert_eq!(command_state, "dispatching");
        database.transition_gateway_command_on(
            &mut connection,
            &GatewayTransition {
                command_id: "first".into(),
                to_state: "accepted_by_source".into(),
                result_summary_json: None,
                error_code: None,
                error_message: None,
                reason_code: Some("request_response_written".into()),
                decision: "allow".into(),
                outcome: "accepted_by_source".into(),
            },
        )?;
        let completed =
            database.complete_pending_request_action_on(&mut connection, "first", "request")?;
        assert_eq!(completed.state, "completed");
        let (request_state, request_version): (String, i64) = connection.query_row(
            "SELECT state,request_version FROM pending_requests WHERE request_id='request'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!((request_state.as_str(), request_version), ("resolved", 4));
        Ok(())
    }

    #[test]
    fn pending_request_claim_rejects_version_drift_and_stale_epoch() -> Result<()> {
        let temp = TempDir::new()?;
        let database = Database::open(&temp.path().join("observer.sqlite"))?;
        database.migrate()?;
        let mut connection = database.connect()?;
        connection.execute(
            "INSERT INTO pending_requests(source_id,epoch_id,request_id,thread_key,request_type,
               state,request_event_seq,payload_json,request_version)
             VALUES ('source','current','request','thread-key','approval','pending',1,'{}',2)",
            [],
        )?;
        authorize(
            &database,
            &mut connection,
            &request_command("wrong-version", "key-version", "current", "request", 1),
        )?;
        assert_eq!(
            database.claim_pending_request_on(&mut connection, "wrong-version")?,
            PendingRequestClaim::NotPending
        );
        authorize(
            &database,
            &mut connection,
            &request_command("old-epoch", "key-epoch", "old", "request", 2),
        )?;
        assert_eq!(
            database.claim_pending_request_on(&mut connection, "old-epoch")?,
            PendingRequestClaim::SourceEpochStale
        );
        Ok(())
    }

    #[test]
    fn pending_request_claim_is_retryable_before_write_and_terminal_after_unknown_write()
    -> Result<()> {
        let temp = TempDir::new()?;
        let database = Database::open(&temp.path().join("observer.sqlite"))?;
        database.migrate()?;
        let mut connection = database.connect()?;
        connection.execute(
            "INSERT INTO pending_requests(source_id,epoch_id,request_id,thread_key,request_type,
               state,request_event_seq,payload_json,request_version)
             VALUES ('source','epoch','request','thread-key','approval','pending',1,'{}',1)",
            [],
        )?;
        authorize(
            &database,
            &mut connection,
            &request_command("before-write", "key-before", "epoch", "request", 1),
        )?;
        assert!(matches!(
            database.claim_pending_request_on(&mut connection, "before-write")?,
            PendingRequestClaim::Claimed(_)
        ));
        database.transition_gateway_command_on(
            &mut connection,
            &GatewayTransition {
                command_id: "before-write".into(),
                to_state: "rejected".into(),
                result_summary_json: None,
                error_code: Some("REQUEST_NOT_PENDING".into()),
                error_message: None,
                reason_code: Some("pre_write_abort".into()),
                decision: "deny".into(),
                outcome: "rejected".into(),
            },
        )?;
        let reset: (String, Option<String>) = connection.query_row(
            "SELECT state,resolving_command_id FROM pending_requests WHERE request_id='request'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(reset, ("pending".into(), None));

        authorize(
            &database,
            &mut connection,
            &request_command("after-write", "key-after", "epoch", "request", 1),
        )?;
        assert!(matches!(
            database.claim_pending_request_on(&mut connection, "after-write")?,
            PendingRequestClaim::Claimed(_)
        ));
        database.transition_gateway_command_on(
            &mut connection,
            &GatewayTransition {
                command_id: "after-write".into(),
                to_state: "outcome_unknown".into(),
                result_summary_json: None,
                error_code: Some("OUTCOME_UNKNOWN".into()),
                error_message: None,
                reason_code: Some("write_outcome_unknown".into()),
                decision: "allow".into(),
                outcome: "outcome_unknown".into(),
            },
        )?;
        let unknown: (String, Option<String>) = connection.query_row(
            "SELECT state,resolving_command_id FROM pending_requests WHERE request_id='request'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(
            unknown,
            ("outcome_unknown".into(), Some("after-write".into()))
        );
        Ok(())
    }

    #[test]
    fn image_upload_claim_is_principal_scoped_bounded_and_cleanup_is_rebuildable() -> Result<()> {
        let temp = TempDir::new()?;
        let database = Database::open(&temp.path().join("observer.sqlite"))?;
        database.migrate()?;
        let mut connection = database.connect()?;
        database.receive_gateway_command_on(
            &mut connection,
            &command("command", "image-key", "hash"),
        )?;
        database.stage_image_upload_on(
            &mut connection,
            &NewImageUpload {
                upload_id: "upload".into(),
                principal_id: "local_bearer".into(),
                mime_type: "image/png".into(),
                size_bytes: 10,
                keyed_fingerprint: "fingerprint".into(),
                relative_path: "upload.bin".into(),
                expires_at_ms: now_ms() + 60_000,
            },
        )?;
        assert!(
            database
                .claim_image_uploads_on(&mut connection, "command", "other", &["upload".into()])
                .is_err()
        );
        let paths = database.claim_image_uploads_on(
            &mut connection,
            "command",
            "local_bearer",
            &["upload".into()],
        )?;
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0].upload_id, "upload");
        assert_eq!(paths[0].relative_path, "upload.bin");
        assert_eq!(paths[0].keyed_fingerprint, "fingerprint");
        let state: String = connection.query_row(
            "SELECT state FROM image_uploads WHERE upload_id='upload'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(state, "attached");
        assert_eq!(
            database
                .cleanup_command_images_on(&mut connection, "command")?
                .len(),
            1
        );
        let state: String = connection.query_row(
            "SELECT state FROM image_uploads WHERE upload_id='upload'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(state, "deleted");
        Ok(())
    }

    #[test]
    fn image_claim_limits_and_duplicate_validation_are_atomic() -> Result<()> {
        let temp = TempDir::new()?;
        let database = Database::open(&temp.path().join("observer.sqlite"))?;
        database.migrate()?;
        let mut connection = database.connect()?;
        for (index, command_id) in ["duplicate-command", "count-command", "size-command"]
            .into_iter()
            .enumerate()
        {
            database.receive_gateway_command_on(
                &mut connection,
                &command(
                    command_id,
                    &format!("key-{index}"),
                    &format!("hash-{index}"),
                ),
            )?;
        }
        let expires = now_ms() + 60_000;
        stage_image(&database, &mut connection, "duplicate", 10, expires)?;
        assert!(
            database
                .claim_image_uploads_on(
                    &mut connection,
                    "duplicate-command",
                    "local_bearer",
                    &["duplicate".into(), "duplicate".into()],
                )
                .is_err()
        );
        let state: String = connection.query_row(
            "SELECT state FROM image_uploads WHERE upload_id='duplicate'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(state, "staged");

        let mut count_ids = Vec::new();
        for index in 0..5 {
            let upload_id = format!("count-{index}");
            stage_image(&database, &mut connection, &upload_id, 10, expires)?;
            count_ids.push(upload_id);
        }
        assert!(
            database
                .claim_image_uploads_on(
                    &mut connection,
                    "count-command",
                    "local_bearer",
                    &count_ids,
                )
                .is_err()
        );

        let mut size_ids = Vec::new();
        for index in 0..3 {
            let upload_id = format!("size-{index}");
            stage_image(
                &database,
                &mut connection,
                &upload_id,
                18 * 1024 * 1024,
                expires,
            )?;
            size_ids.push(upload_id);
        }
        assert!(
            database
                .claim_image_uploads_on(&mut connection, "size-command", "local_bearer", &size_ids,)
                .is_err()
        );
        let attached: i64 = connection.query_row(
            "SELECT COUNT(*) FROM image_uploads WHERE state='attached'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(attached, 0);
        Ok(())
    }

    #[test]
    fn expired_image_cannot_be_claimed_and_turn_terminal_cleanup_uses_expected_turn() -> Result<()>
    {
        let temp = TempDir::new()?;
        let database = Database::open(&temp.path().join("observer.sqlite"))?;
        database.migrate()?;
        let mut connection = database.connect()?;
        database.receive_gateway_command_on(
            &mut connection,
            &command("turn-command", "turn-key", "turn-hash"),
        )?;
        connection.execute(
            "UPDATE gateway_commands SET expected_turn_id='turn-active' WHERE command_id='turn-command'",
            [],
        )?;
        stage_image(&database, &mut connection, "expired", 10, now_ms() - 1)?;
        assert!(
            database
                .claim_image_uploads_on(
                    &mut connection,
                    "turn-command",
                    "local_bearer",
                    &["expired".into()],
                )
                .is_err()
        );
        stage_image(
            &database,
            &mut connection,
            "turn-image",
            10,
            now_ms() + 60_000,
        )?;
        database.claim_image_uploads_on(
            &mut connection,
            "turn-command",
            "local_bearer",
            &["turn-image".into()],
        )?;
        assert_eq!(
            database
                .cleanup_turn_images_on(&mut connection, "turn-active")?
                .len(),
            1
        );
        let state: String = connection.query_row(
            "SELECT state FROM image_uploads WHERE upload_id='turn-image'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(state, "deleted");
        Ok(())
    }

    #[test]
    fn startup_image_sweep_removes_expired_metadata_and_old_physical_orphans() -> Result<()> {
        let temp = TempDir::new()?;
        let database = Database::open(&temp.path().join("observer.sqlite"))?;
        database.migrate()?;
        let mut connection = database.connect()?;
        let directory = database.image_staging_dir();
        crate::permissions::prepare_private_dir(&directory, "test image staging")?;
        stage_image(&database, &mut connection, "expired-file", 10, now_ms() - 1)?;
        std::fs::write(directory.join("expired-file.bin"), b"expired")?;
        std::fs::write(directory.join("physical-orphan.bin"), b"orphan")?;
        drop(connection);

        let removed = database.sweep_expired_image_uploads(
            now_ms() + Duration::from_secs(25 * 60 * 60).as_millis() as i64,
        )?;
        assert_eq!(removed, 2);
        assert!(!directory.join("expired-file.bin").exists());
        assert!(!directory.join("physical-orphan.bin").exists());
        let state: String = database.connect()?.query_row(
            "SELECT state FROM image_uploads WHERE upload_id='expired-file'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(state, "expired");
        Ok(())
    }

    #[test]
    fn startup_image_sweep_rejects_metadata_path_traversal() -> Result<()> {
        let temp = TempDir::new()?;
        let database = Database::open(&temp.path().join("observer.sqlite"))?;
        database.migrate()?;
        let mut connection = database.connect()?;
        stage_image(&database, &mut connection, "traversal", 10, now_ms() - 1)?;
        connection.execute(
            "UPDATE image_uploads SET relative_path='../outside.bin' WHERE upload_id='traversal'",
            [],
        )?;
        let outside = temp.path().join("outside.bin");
        std::fs::write(&outside, b"outside")?;
        drop(connection);
        assert!(database.sweep_expired_image_uploads(now_ms()).is_err());
        assert_eq!(std::fs::read(outside)?, b"outside");
        Ok(())
    }

    #[test]
    fn restart_recovery_never_replays_and_marks_prewrite_vs_ambiguous_outcomes() -> Result<()> {
        let temp = TempDir::new()?;
        let database = Database::open(&temp.path().join("observer.sqlite"))?;
        database.migrate()?;
        let mut connection = database.connect()?;
        connection.execute(
            "INSERT INTO sources(source_id,kind,stable_identity,config_json,status,created_at_ms,updated_at_ms)
             VALUES ('source','app_server','socket','{}','ready',0,0)",
            [],
        )?;
        connection.execute(
            "INSERT INTO source_epochs(source_id,epoch_id,opened_at_ms) VALUES ('source','epoch',0)",
            [],
        )?;
        for (index, id) in [
            "received",
            "authorized",
            "dispatching",
            "accepted",
            "running",
        ]
        .into_iter()
        .enumerate()
        {
            database.receive_gateway_command_on(
                &mut connection,
                &command(
                    id,
                    &format!("restart-key-{index}"),
                    &format!("restart-hash-{index}"),
                ),
            )?;
        }
        let transition = |command_id: &str, to_state: &str| GatewayTransition {
            command_id: command_id.into(),
            to_state: to_state.into(),
            result_summary_json: None,
            error_code: None,
            error_message: None,
            reason_code: None,
            decision: "allow".into(),
            outcome: to_state.into(),
        };
        for id in ["authorized", "dispatching", "accepted", "running"] {
            database
                .transition_gateway_command_on(&mut connection, &transition(id, "authorized"))?;
        }
        for id in ["dispatching", "accepted", "running"] {
            database
                .transition_gateway_command_on(&mut connection, &transition(id, "dispatching"))?;
        }
        for id in ["accepted", "running"] {
            database.transition_gateway_command_on(
                &mut connection,
                &transition(id, "accepted_by_source"),
            )?;
        }
        database
            .transition_gateway_command_on(&mut connection, &transition("running", "running"))?;
        let expires = now_ms() + 60_000;
        stage_image(&database, &mut connection, "prewrite-image", 10, expires)?;
        database.claim_image_uploads_on(
            &mut connection,
            "authorized",
            "local_bearer",
            &["prewrite-image".into()],
        )?;
        stage_image(&database, &mut connection, "ambiguous-image", 10, expires)?;
        database.claim_image_uploads_on(
            &mut connection,
            "dispatching",
            "local_bearer",
            &["ambiguous-image".into()],
        )?;
        connection.execute(
            "INSERT INTO pending_requests(source_id,epoch_id,request_id,thread_key,request_type,state,
               request_event_seq,payload_json,request_version,resolving_command_id,resolving_started_at_ms)
             VALUES ('source','epoch','request',NULL,'approval','resolving',1,'{}',1,'dispatching',1)",
            [],
        )?;
        drop(connection);

        let report = database.recover_gateway_after_restart()?;
        assert_eq!(report.closed_epochs, 1);
        assert_eq!(report.failed_before_dispatch, 2);
        assert_eq!(report.outcome_unknown, 3);
        assert_eq!(report.image_paths.len(), 1);
        assert!(report.image_paths[0].ends_with("prewrite-image.bin"));
        let connection = database.connect()?;
        for id in ["received", "authorized"] {
            let command = database
                .gateway_command(id)?
                .context("missing recovered command")?;
            assert_eq!(command.state, "failed");
            assert_eq!(
                command.error.context("missing restart error")?.code,
                "GATEWAY_RESTARTED"
            );
        }
        for id in ["dispatching", "accepted", "running"] {
            let command = database
                .gateway_command(id)?
                .context("missing recovered command")?;
            assert_eq!(command.state, "outcome_unknown");
            assert_eq!(
                command.error.context("missing unknown error")?.code,
                "OUTCOME_UNKNOWN"
            );
        }
        let image_states: String = connection.query_row(
            "SELECT group_concat(upload_id||':'||state,',') FROM image_uploads ORDER BY upload_id",
            [],
            |row| row.get(0),
        )?;
        assert!(image_states.contains("prewrite-image:deleted"));
        assert!(image_states.contains("ambiguous-image:attached"));
        let pending_state: String = connection.query_row(
            "SELECT state FROM pending_requests WHERE request_id='request'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(pending_state, "source_disconnected");
        let epoch: (Option<i64>, String) = connection.query_row(
            "SELECT closed_at_ms,close_reason FROM source_epochs WHERE source_id='source' AND epoch_id='epoch'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert!(epoch.0.is_some());
        assert_eq!(epoch.1, "gateway_restart");
        let reasons: i64 = connection.query_row(
            "SELECT COUNT(*) FROM command_transitions WHERE reason_code LIKE 'GATEWAY_RESTARTED_%'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(reasons, 5);
        drop(connection);
        assert_eq!(
            database.recover_gateway_after_restart()?,
            GatewayRecoveryReport::default()
        );
        Ok(())
    }

    #[test]
    fn restart_recovery_fails_closed_on_command_projection_drift() -> Result<()> {
        let temp = TempDir::new()?;
        let database = Database::open(&temp.path().join("observer.sqlite"))?;
        database.migrate()?;
        let mut connection = database.connect()?;
        database.receive_gateway_command_on(
            &mut connection,
            &command("drift", "drift-key", "drift-hash"),
        )?;
        connection.execute(
            "UPDATE gateway_commands SET state='authorized' WHERE command_id='drift'",
            [],
        )?;
        drop(connection);
        let error = database
            .recover_gateway_after_restart()
            .expect_err("projection drift must stop recovery");
        assert!(error.to_string().contains("ledger is inconsistent"));
        let connection = database.connect()?;
        let (state, transitions): (String, i64) = connection.query_row(
            "SELECT c.state,(SELECT COUNT(*) FROM command_transitions t WHERE t.command_id=c.command_id)
             FROM gateway_commands c WHERE c.command_id='drift'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(state, "authorized");
        assert_eq!(transitions, 1);
        Ok(())
    }
}
