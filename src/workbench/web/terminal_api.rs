//! Authenticated terminal transport. Connection identity is server-owned;
//! authority changes and the private reconnect grant stay on that WebSocket.

use super::*;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use futures_util::{SinkExt, stream::SplitSink};
use serde_json::{Value, json};
use tokio::sync::OwnedSemaphorePermit;

use crate::workbench::terminal::{Attachment, TerminalEvent, TerminalHandle};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Epoch {
    epoch: Uuid,
}

pub(super) async fn upgrade(
    State(state): State<Arc<WebState>>,
    headers: HeaderMap,
    Query(epoch): Query<Epoch>,
    upgrade: WebSocketUpgrade,
) -> Response {
    if !authorised(&headers, &state) {
        return error(StatusCode::UNAUTHORIZED, "pairing_required");
    }
    // Browsers always provide an Origin for a WebSocket. An absent one cannot
    // silently downgrade CSRF checks to the read-only HTTP route policy.
    if !same_origin(&headers, &state) {
        return error(StatusCode::FORBIDDEN, "origin_required");
    }
    if epoch.epoch != state.hub.epoch() {
        return error(StatusCode::CONFLICT, "stale_run");
    }
    let Some(terminal) = state.options.terminal.clone() else {
        return error(StatusCode::NOT_FOUND, "terminal_unavailable");
    };
    let Ok(permit) = state.terminal_slots.clone().try_acquire_owned() else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "terminal_limit");
    };
    let permission = remote_permission(&headers, &state);
    let attachment = match terminal.attach_authorized(permission.clone()).await {
        Ok(attachment) => attachment,
        Err(_) => return error(StatusCode::SERVICE_UNAVAILABLE, "terminal_unavailable"),
    };
    upgrade
        .max_message_size(96 * 1024)
        .max_frame_size(96 * 1024)
        .on_upgrade(move |socket| {
            serve(
                socket,
                attachment,
                terminal,
                state.stop.clone(),
                permit,
                permission,
            )
        })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ClientFrame {
    run_epoch: Uuid,
    id: u64,
    command: ClientCommand,
}

#[derive(Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum ClientCommand {
    Claim,
    Takeover {
        confirmed: bool,
    },
    Reconnect {
        secret: String,
    },
    Release {
        generation: u64,
    },
    Input {
        generation: u64,
        sequence: u64,
        data: String,
    },
    Resize {
        generation: u64,
        rows: u16,
        cols: u16,
    },
}

async fn send(writer: &mut SplitSink<WebSocket, Message>, value: Value) -> bool {
    // A stalled network socket cannot retain a terminal attachment indefinitely.
    matches!(
        tokio::time::timeout(
            Duration::from_secs(2),
            writer.send(Message::Text(value.to_string().into()))
        )
        .await,
        Ok(Ok(()))
    )
}

fn rejected(epoch: Uuid, id: Option<u64>, code: &'static str) -> Value {
    json!({"type":"error","id":id,"source":"workbench_terminal_web","runEpoch":epoch,"code":code})
}

async fn serve(
    socket: WebSocket,
    mut attachment: Attachment,
    terminal: TerminalHandle,
    mut stop: watch::Receiver<bool>,
    _permit: OwnedSemaphorePermit,
    permission: Option<crate::workbench::permission::Permission>,
) {
    let revoked = async {
        if let Some(permission) = &permission {
            permission.revoked().await;
        } else {
            std::future::pending::<()>().await;
        }
    };
    tokio::pin!(revoked);
    let epoch = terminal.epoch();
    let connection = attachment.connection_id;
    let snapshot = &attachment.snapshot;
    let hello = json!({
        "type":"snapshot", "runEpoch":epoch, "connectionId":connection,
        "outputSeq":snapshot.to_seq, "checkpointSeq":snapshot.checkpoint_seq,
        "rows":snapshot.rows,"cols":snapshot.cols,
        "screen":STANDARD.encode(&snapshot.screen),"replay":STANDARD.encode(&snapshot.replay),
        "complete":snapshot.complete,"truncated":snapshot.truncated,
        "control":attachment.control,"exit":attachment.exit,"fault":attachment.fault,
        "retainedBytes":attachment.retained_bytes,"checkpointBytes":attachment.checkpoint_bytes
    });
    let (mut writer, mut reader) = socket.split();
    if !send(&mut writer, hello).await {
        return;
    }
    let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_seen = std::time::Instant::now();
    let mut last_id = 0;
    loop {
        tokio::select! {
            biased;
            _ = &mut revoked => break,
            _ = stop.changed() => break,
            _ = heartbeat.tick() => {
                if last_seen.elapsed() > Duration::from_secs(45) { break; }
                if !matches!(tokio::time::timeout(Duration::from_secs(2), writer.send(Message::Ping(Bytes::new()))).await, Ok(Ok(()))) { break; }
            }
            event = attachment.events.recv() => {
                let frame = match event {
                    Some(TerminalEvent::Output(output)) => json!({"type":"output","runEpoch":epoch,"outputSeq":output.output_seq,"data":STANDARD.encode(&output.data)}),
                    Some(TerminalEvent::State {control,exit}) => json!({"type":"state","runEpoch":epoch,"control":control,"exit":exit}),
                    Some(TerminalEvent::Fault(fault)) => json!({"type":"fault","runEpoch":epoch,"error":fault}),
                    None => { let _ = send(&mut writer, json!({"type":"snapshot_required","runEpoch":epoch})).await; break; }
                };
                if !send(&mut writer, frame).await { break; }
            }
            incoming = reader.next() => {
                last_seen = std::time::Instant::now();
                let text = match incoming {
                    Some(Ok(Message::Text(text))) => text,
                    Some(Ok(Message::Pong(_))) | Some(Ok(Message::Ping(_))) => continue,
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    _ => { let _ = send(&mut writer, rejected(epoch,None,"invalid_frame")).await; break; }
                };
                let Ok(frame) = serde_json::from_str::<ClientFrame>(&text) else {
                    let _ = send(&mut writer,rejected(epoch,None,"invalid_frame")).await;
                    break;
                };
                if frame.run_epoch != epoch {
                    let _ = send(&mut writer,rejected(epoch,Some(frame.id),"stale_run")).await;
                    break;
                }
                if frame.id == 0 || frame.id <= last_id {
                    let _ = send(&mut writer,rejected(epoch,Some(frame.id),"request_sequence")).await;
                    break;
                }
                if permission.as_ref().is_some_and(|p| !p.active()) { break; }
                last_id = frame.id;
                let response = dispatch(&terminal,connection,frame).await;
                if !send(&mut writer,response).await { break; }
            }
        }
    }
    // Dropping the receiver makes the actor release this connection/reserve
    // its reconnect window. No close path retries an input or spawns a CLI.
}

async fn dispatch(terminal: &TerminalHandle, connection: Uuid, frame: ClientFrame) -> Value {
    let epoch = terminal.epoch();
    let id = frame.id;
    let ack = || json!({"type":"ack","id":id,"runEpoch":epoch});
    let grant = |grant| json!({"type":"grant","id":id,"runEpoch":epoch,"grant":grant});
    let result = match frame.command {
        ClientCommand::Claim => terminal.claim(connection).await.map(grant),
        ClientCommand::Takeover { confirmed: true } => {
            terminal.takeover(connection).await.map(grant)
        }
        ClientCommand::Takeover { confirmed: false } => {
            return rejected(epoch, Some(id), "takeover_confirmation_required");
        }
        ClientCommand::Reconnect { secret } => {
            if secret.len() != 64 {
                return rejected(epoch, Some(id), "invalid_reconnect");
            }
            terminal.reconnect(connection, secret).await.map(grant)
        }
        ClientCommand::Release { generation } => terminal
            .release(connection, generation)
            .await
            .map(|_| ack()),
        ClientCommand::Input {
            generation,
            sequence,
            data,
        } => {
            if data.len() > 87384 {
                return rejected(epoch, Some(id), "input_size");
            }
            let Ok(bytes) = STANDARD.decode(data) else {
                return rejected(epoch, Some(id), "invalid_input");
            };
            terminal
                .input(connection, generation, sequence, bytes)
                .await
                .map(|_| ack())
        }
        ClientCommand::Resize {
            generation,
            rows,
            cols,
        } => terminal
            .resize(connection, generation, rows, cols)
            .await
            .map(|changed| json!({"type":"ack","id":id,"runEpoch":epoch,"resized":changed})),
    };
    result.unwrap_or_else(|error| json!({"type":"error","id":id,"runEpoch":epoch,"error":error}))
}

pub(super) async fn stop_run(
    State(state): State<Arc<WebState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !authorised(&headers, &state) {
        return error(StatusCode::UNAUTHORIZED, "pairing_required");
    }
    if !same_origin(&headers, &state) {
        return error(StatusCode::FORBIDDEN, "origin_required");
    }
    let Ok(epoch) = serde_json::from_slice::<Epoch>(&body) else {
        return error(StatusCode::BAD_REQUEST, "invalid_stop");
    };
    if epoch.epoch != state.hub.epoch() {
        return error(StatusCode::CONFLICT, "stale_run");
    }
    let Some(terminal) = &state.options.terminal else {
        return error(StatusCode::NOT_FOUND, "terminal_unavailable");
    };
    match terminal.stop().await {
        Ok(()) => StatusCode::ACCEPTED.into_response(),
        Err(_) => error(StatusCode::SERVICE_UNAVAILABLE, "terminal_unavailable"),
    }
}
