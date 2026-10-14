//! Default-off, outbound-only connector to the trusted public workbench relay.
#[cfg(test)]
mod login_tests;
mod storage;
mod transport;

use crate::workbench::remote_access::IngressHandle;
use anyhow::{Result, bail};
use codex_relay_protocol::{BindingPending, BindingPoll, BindingStart, Bound};
use serde::Serialize;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use storage::{Saved, Storage};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Snapshot {
    pub state: String,
    pub enabled: bool,
    pub server_url: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gateway_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gateway_origin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binding: Option<BindingStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BindingStatus {
    pub user_code: String,
    pub verification_url: String,
    pub expires_in_seconds: u64,
}
impl Snapshot {
    fn from_saved(saved: &Saved) -> Self {
        Self {
            state: if saved.authentication_required {
                "authenticationRequired"
            } else if saved.enabled {
                "offline"
            } else {
                "disabled"
            }
            .into(),
            enabled: saved.enabled,
            server_url: saved.server_url.clone(),
            name: saved.name.clone(),
            username: saved
                .bound
                .as_ref()
                .map(|b| b.username.clone())
                .filter(|v| !v.is_empty()),
            account_id: saved.bound.as_ref().map(|b| b.account_id),
            gateway_id: saved.bound.as_ref().map(|b| b.gateway_id),
            gateway_origin: saved.bound.as_ref().map(|b| b.gateway_origin.clone()),
            binding: None,
            error: None,
        }
    }
}
type Shared = Arc<Mutex<Snapshot>>;
#[derive(Clone)]
pub(crate) struct Service {
    inner: Arc<Handle>,
}
struct Handle {
    tx: mpsc::Sender<Command>,
    snapshot: Shared,
    ingress: IngressHandle,
}
impl Drop for Handle {
    fn drop(&mut self) {
        self.ingress.disconnect();
    }
}
struct Command {
    action: Action,
    reply: oneshot::Sender<Result<Snapshot>>,
}
enum Action {
    Bind(String, String, bool),
    Enable,
    Disable,
    Forget,
}
impl Service {
    pub(crate) fn start(root: PathBuf, instance_id: Uuid, ingress: IngressHandle) -> Result<Self> {
        let store = Storage::open(&root)?;
        let saved = store.load()?;
        validate_saved(&saved)?;
        let snapshot = Arc::new(Mutex::new(Snapshot::from_saved(&saved)));
        let (tx, rx) = mpsc::channel(16);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let thread_snapshot = snapshot.clone();
        let thread_ingress = ingress.clone();
        std::thread::Builder::new()
            .name("workbench-relay".into())
            .spawn(move || {
                runtime.block_on(worker(
                    store,
                    saved,
                    instance_id,
                    thread_ingress,
                    thread_snapshot,
                    rx,
                ))
            })?;
        Ok(Self {
            inner: Arc::new(Handle {
                tx,
                snapshot,
                ingress,
            }),
        })
    }
    pub(crate) async fn status(&self) -> Snapshot {
        self.inner.snapshot.lock().unwrap().clone()
    }
    async fn command(&self, action: Action) -> Result<Snapshot> {
        let (reply, rx) = oneshot::channel();
        self.inner
            .tx
            .send(Command { action, reply })
            .await
            .map_err(|_| anyhow::anyhow!("relay connector stopped"))?;
        rx.await
            .map_err(|_| anyhow::anyhow!("relay connector stopped"))?
    }
    pub(crate) async fn bind(
        &self,
        server_url: String,
        name: String,
        allow_insecure_loopback: bool,
    ) -> Result<Snapshot> {
        self.command(Action::Bind(server_url, name, allow_insecure_loopback))
            .await
    }
    pub(crate) async fn enable(&self) -> Result<Snapshot> {
        self.command(Action::Enable).await
    }
    pub(crate) async fn disable(&self) -> Result<Snapshot> {
        self.command(Action::Disable).await
    }
    pub(crate) async fn forget(&self) -> Result<Snapshot> {
        self.command(Action::Forget).await
    }
}
fn validate_saved(saved: &Saved) -> Result<()> {
    if !saved.server_url.is_empty() {
        transport::server_url(&saved.server_url, saved.allow_insecure_loopback)?;
    }
    if let Some(bound) = &saved.bound {
        let origin = transport::server_url(&bound.gateway_origin, saved.allow_insecure_loopback)?;
        if bound.account_id.is_nil()
            || bound.gateway_id.is_nil()
            || origin.origin().ascii_serialization() != bound.gateway_origin
        {
            bail!("invalid saved relay identity");
        }
        if bound.credential.is_empty() || bound.credential.len() > 4096 {
            bail!("invalid saved relay credential");
        }
    }
    Ok(())
}
enum Completed {
    Bound(Bound),
    AuthenticationRequired,
    Failed,
}
async fn worker(
    store: Storage,
    mut saved: Saved,
    instance: Uuid,
    ingress: IngressHandle,
    snapshot: Shared,
    mut rx: mpsc::Receiver<Command>,
) {
    let mut task = launch(&saved, instance, &ingress, &snapshot);
    loop {
        tokio::select! {
            command = rx.recv() => {
                let Some(command) = command else { break; };
                // Each local change is a fence: old streams lose permission before disk/network work.
                ingress.disconnect();
                if let Some(old) = task.take() { old.abort(); let _ = old.await; }
                let binding = matches!(command.action,Action::Bind(..));
                let stop_requested = matches!(command.action,Action::Disable | Action::Forget);
                let forget_requested = matches!(command.action,Action::Forget);
                let revoke_saved = forget_requested.then(|| saved.clone());
                let mut candidate = saved.clone();
                let result = (|| -> Result<()> {
                    match command.action {
                        Action::Bind(url,name,insecure) => {
                            let url = transport::server_url(&url,insecure)?.to_string();
                            if name.trim().is_empty() || name.len() > 120 { bail!("workbench name must contain 1 to 120 bytes"); }
                            if candidate.bound.is_some() && candidate.server_url != url { bail!("forget the existing binding before changing servers"); }
                            candidate.server_url=url; candidate.name=name; candidate.allow_insecure_loopback=insecure; candidate.enabled=false;
                        }
                        Action::Enable => { if candidate.bound.is_none() || candidate.authentication_required { bail!("log in before enabling relay"); } candidate.enabled=true; }
                        Action::Disable => candidate.enabled=false,
                        Action::Forget => { candidate.enabled=false; candidate.bound=None; candidate.authentication_required=false; }
                    }
                    store.save(&candidate)?; saved=candidate; Ok(())
                })();
                if result.is_err() && stop_requested {
                    saved.enabled=false;
                    if forget_requested { saved.bound=None; saved.authentication_required=false; }
                }
                *snapshot.lock().unwrap() = Snapshot::from_saved(&saved);
                if result.is_err() && stop_requested { snapshot.lock().unwrap().error=Some("private_state_save_failed".into()); }
                if let Some(previous)=revoke_saved {
                    // Local authority and persistence are fenced before this bounded best effort.
                    // Keep no credential or offline retry after the one revoke attempt.
                    let revoked=tokio::time::timeout(Duration::from_secs(8),revoke_server(&previous)).await;
                    if !matches!(revoked,Ok(Ok(()))) && result.is_ok() {
                        snapshot.lock().unwrap().error=Some("server_revocation_unconfirmed".into());
                    }
                }
                if result.is_ok() && binding {
                    snapshot.lock().unwrap().state="binding".into();
                    let cfg=saved.clone(); let view=snapshot.clone();
                    task=Some(tokio::spawn(async move {
                        match bind(&cfg,&view).await { Ok(bound)=>Completed::Bound(bound), Err(_)=> { let mut state=view.lock().unwrap(); state.state="bindingFailed".into(); state.binding=None; state.error=Some("binding_failed_or_expired".into()); Completed::Failed } }
                    }));
                } else { task=launch(&saved,instance,&ingress,&snapshot); }
                let reply = result.map(|()| snapshot.lock().unwrap().clone()); let _=command.reply.send(reply);
            }
            completed = async { match task.as_mut() { Some(task)=>Some(task.await),None=>std::future::pending().await } } => {
                task=None; ingress.disconnect();
                if let Some(Ok(Completed::Bound(bound)))=completed {
                    let mut next=saved.clone(); next.bound=Some(bound); next.enabled=true; next.authentication_required=false;
                    if store.save(&next).is_ok() { saved=next; *snapshot.lock().unwrap()=Snapshot::from_saved(&saved); task=launch(&saved,instance,&ingress,&snapshot); }
                    else {
                        // Never enable a credential that has not been durably stored.
                        let _=tokio::time::timeout(Duration::from_secs(8),revoke_server(&next)).await;
                        let mut state=snapshot.lock().unwrap(); state.state="error".into(); state.binding=None; state.error=Some("credential_save_failed".into());
                    }
                } else if let Some(Ok(Completed::AuthenticationRequired))=completed {
                    saved.enabled=false; saved.authentication_required=true;
                    let stored=store.save(&saved).is_ok();
                    *snapshot.lock().unwrap()=Snapshot::from_saved(&saved);
                    if !stored { snapshot.lock().unwrap().error=Some("private_state_save_failed".into()); }
                }
            }
        }
    }
    ingress.disconnect();
    if let Some(task) = task {
        task.abort();
        let _ = task.await;
    }
}
fn launch(
    saved: &Saved,
    instance: Uuid,
    ingress: &IngressHandle,
    snapshot: &Shared,
) -> Option<tokio::task::JoinHandle<Completed>> {
    if !saved.enabled || saved.bound.is_none() || saved.authentication_required {
        return None;
    }
    let saved = saved.clone();
    let ingress = ingress.clone();
    let snapshot = snapshot.clone();
    Some(tokio::spawn(async move {
        if transport::run(saved, instance, ingress, snapshot).await {
            Completed::AuthenticationRequired
        } else {
            Completed::Failed
        }
    }))
}
async fn revoke_server(saved: &Saved) -> Result<()> {
    let Some(bound) = &saved.bound else {
        return Ok(());
    };
    let origin = transport::server_url(&saved.server_url, saved.allow_insecure_loopback)?;
    let response = transport::client(&saved.server_url)?
        .post(origin.join("/relay/v1/gateway/revoke")?)
        .bearer_auth(&bound.credential)
        .header("content-type", "application/json")
        .body("{}")
        .send()
        .await?;
    if response.status() != reqwest::StatusCode::NO_CONTENT {
        bail!("server credential revocation not confirmed");
    }
    Ok(())
}

async fn bind(saved: &Saved, snapshot: &Shared) -> Result<Bound> {
    let client = transport::client(&saved.server_url)?;
    let pending: BindingPending = post(
        &client,
        &format!("{}relay/v1/bindings", saved.server_url),
        &BindingStart {
            name: saved.name.clone(),
            resume_gateway_id: saved.bound.as_ref().map(|b| b.gateway_id),
            resume_credential: saved.bound.as_ref().map(|b| b.credential.clone()),
        },
    )
    .await?;
    let verification = reqwest::Url::parse(&pending.verification_url)?;
    if verification.origin()
        != transport::server_url(&saved.server_url, saved.allow_insecure_loopback)?.origin()
        || !verification.username().is_empty()
        || verification.password().is_some()
    {
        bail!("invalid verification origin");
    }
    let ttl = pending.expires_in_seconds.min(600);
    if ttl == 0
        || pending.device_code.is_empty()
        || pending.device_code.len() > 4096
        || pending.user_code.len() > 64
    {
        bail!("invalid binding response");
    }
    snapshot.lock().unwrap().binding = Some(BindingStatus {
        user_code: pending.user_code,
        verification_url: pending.verification_url,
        expires_in_seconds: ttl,
    });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(ttl);
    loop {
        tokio::time::sleep(Duration::from_secs(pending.interval.clamp(5, 30))).await;
        if tokio::time::Instant::now() >= deadline {
            bail!("binding expired");
        }
        let response = client
            .post(format!(
                "{}relay/v1/bindings/{}/poll",
                saved.server_url, pending.binding_id
            ))
            .header("content-type", "application/json")
            .body(serde_json::to_vec(&BindingPoll {
                device_code: pending.device_code.clone(),
            })?)
            .send()
            .await?;
        if response.status() == reqwest::StatusCode::ACCEPTED {
            continue;
        }
        let bound: Bound = decode(response).await?;
        if saved.bound.as_ref().is_some_and(|previous| {
            previous.account_id != bound.account_id || previous.gateway_id != bound.gateway_id
        }) {
            bail!("relay reauthentication identity mismatch");
        }
        let mut check = saved.clone();
        check.bound = Some(bound.clone());
        validate_saved(&check)?;
        return Ok(bound);
    }
}
async fn post<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    url: &str,
    body: &impl Serialize,
) -> Result<T> {
    decode(
        client
            .post(url)
            .header("content-type", "application/json")
            .body(serde_json::to_vec(body)?)
            .send()
            .await?,
    )
    .await
}
async fn decode<T: serde::de::DeserializeOwned>(mut response: reqwest::Response) -> Result<T> {
    if !response.status().is_success() {
        bail!("relay request rejected");
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len() + chunk.len() > 32 * 1024 {
            bail!("relay response too large");
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("invalid relay response"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn status_cannot_disclose_credentials() {
        let saved = Saved {
            bound: Some(Bound {
                username: "synthetic".into(),
                account_id: Uuid::new_v4(),
                gateway_id: Uuid::new_v4(),
                credential: "do-not-disclose-synthetic".into(),
                gateway_origin: "https://g.example".into(),
            }),
            ..Saved::default()
        };
        let status = Snapshot::from_saved(&saved);
        let json = serde_json::to_string(&status).unwrap();
        assert!(!json.contains("credential"));
        assert!(!json.contains("deviceCode"));
        assert!(!format!("{status:?}").contains("do-not-disclose"));
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use crate::workbench::remote_access::Ingress;
    #[tokio::test]
    async fn offline_saved_binding_disable_forget_and_last_drop_stop_background() {
        let root = tempfile::tempdir().unwrap();
        let store = Storage::open(root.path()).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let unused = listener.local_addr().unwrap();
        drop(listener);
        let saved = Saved {
            server_url: format!("http://{unused}/"),
            name: "Offline synthetic".into(),
            allow_insecure_loopback: true,
            enabled: true,
            authentication_required: false,
            bound: Some(Bound {
                username: "synthetic".into(),
                account_id: Uuid::new_v4(),
                gateway_id: Uuid::new_v4(),
                credential: "private-synthetic-offline".into(),
                gateway_origin: "https://gateway.example".into(),
            }),
        };
        store.save(&saved).unwrap();
        let instance = Uuid::new_v4();
        let ingress = Ingress::start(axum::Router::new(), instance).unwrap();
        let service = Service::start(root.path().into(), instance, ingress.handle()).unwrap();
        let status = service.status().await;
        assert!(status.enabled);
        assert!(status.gateway_id.is_some());
        let disabled = service.disable().await.unwrap();
        assert!(!disabled.enabled);
        assert_eq!(disabled.state, "disabled");
        assert!(!store.load().unwrap().enabled);
        assert!(service.enable().await.unwrap().enabled);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let state_path = root.path().join("relay/state.json");
            std::fs::set_permissions(&state_path, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(service.disable().await.is_err());
            assert!(!service.status().await.enabled);
            assert_eq!(
                service.status().await.error.as_deref(),
                Some("private_state_save_failed")
            );
            std::fs::set_permissions(state_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let forgotten = service.forget().await.unwrap();
        assert!(forgotten.gateway_id.is_none());
        assert!(!forgotten.enabled);
        assert_eq!(
            forgotten.error.as_deref(),
            Some("server_revocation_unconfirmed")
        );
        assert!(store.load().unwrap().bound.is_none());
        assert!(service.enable().await.is_err());
        let background = Arc::downgrade(&service.inner.snapshot);
        drop(service);
        tokio::time::timeout(Duration::from_secs(2), async {
            while background.upgrade().is_some() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn invalid_binding_configuration_never_enables_outbound_connection() {
        let root = tempfile::tempdir().unwrap();
        let instance = Uuid::new_v4();
        let ingress = Ingress::start(axum::Router::new(), instance).unwrap();
        let service = Service::start(root.path().into(), instance, ingress.handle()).unwrap();
        assert!(
            service
                .bind("http://external.example".into(), "Synthetic".into(), true)
                .await
                .is_err()
        );
        assert!(!service.status().await.enabled);
        assert!(service.status().await.server_url.is_empty());
    }
}

#[cfg(test)]
mod revoke_tests {
    use super::*;
    use crate::workbench::remote_access::Ingress;
    use axum::{
        Router,
        http::{HeaderMap, StatusCode},
        routing::post,
    };

    #[tokio::test]
    async fn forget_fences_locally_before_confirming_server_revocation() {
        let root = tempfile::tempdir().unwrap();
        let store = Storage::open(root.path()).unwrap();
        let (seen, received) = oneshot::channel();
        let seen = Arc::new(Mutex::new(Some(seen)));
        let release = Arc::new(tokio::sync::Notify::new());
        let server_release = release.clone();
        let app = Router::new().route(
            "/relay/v1/gateway/revoke",
            post(move |headers: HeaderMap, body: String| {
                let seen = seen.clone();
                let release = server_release.clone();
                async move {
                    assert_eq!(body, "{}");
                    if let Some(seen) = seen.lock().unwrap().take() {
                        let _ = seen.send(headers);
                    }
                    release.notified().await;
                    StatusCode::NO_CONTENT
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let saved = Saved {
            server_url: format!("http://portal.synthetic.localhost:{}/", address.port()),
            name: "Synthetic".into(),
            allow_insecure_loopback: true,
            enabled: false,
            authentication_required: false,
            bound: Some(Bound {
                username: "synthetic".into(),
                account_id: Uuid::new_v4(),
                gateway_id: Uuid::new_v4(),
                credential: "synthetic-revoke-credential".into(),
                gateway_origin: "https://gateway.example".into(),
            }),
        };
        store.save(&saved).unwrap();
        let instance = Uuid::new_v4();
        let ingress = Ingress::start(Router::new(), instance).unwrap();
        let handle = ingress.handle();
        let bound = saved.bound.as_ref().unwrap();
        handle
            .begin(
                bound.account_id,
                bound.gateway_id,
                Uuid::new_v4(),
                bound.gateway_origin.clone(),
            )
            .unwrap();
        let session = Uuid::new_v4();
        assert!(handle.grant(session).is_ok());
        let service = Service::start(root.path().into(), instance, handle.clone()).unwrap();
        let action_service = service.clone();
        let forgetting = tokio::spawn(async move { action_service.forget().await });
        let headers = tokio::time::timeout(Duration::from_secs(2), received)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            headers["authorization"],
            "Bearer synthetic-revoke-credential"
        );
        assert!(!headers.contains_key("cookie"));
        assert_eq!(headers["content-type"], "application/json");
        assert!(handle.grant(session).is_err());
        assert!(store.load().unwrap().bound.is_none());
        assert!(service.status().await.gateway_id.is_none());
        assert!(!service.status().await.enabled);
        release.notify_one();
        let status = forgetting.await.unwrap().unwrap();
        assert!(status.error.is_none());
        assert!(status.gateway_id.is_none());
        let json = serde_json::to_string(&status).unwrap();
        assert!(!json.contains("synthetic-revoke-credential"));
        server.abort();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn forget_storage_failure_stays_unbound_and_prioritizes_disk_error() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let store = Storage::open(root.path()).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let saved = Saved {
            server_url: format!("http://{address}/"),
            allow_insecure_loopback: true,
            name: "Synthetic".into(),
            bound: Some(Bound {
                username: "synthetic".into(),
                account_id: Uuid::new_v4(),
                gateway_id: Uuid::new_v4(),
                credential: "synthetic-save-failure".into(),
                gateway_origin: "https://gateway.example".into(),
            }),
            ..Saved::default()
        };
        store.save(&saved).unwrap();
        let instance = Uuid::new_v4();
        let ingress = Ingress::start(Router::new(), instance).unwrap();
        let service = Service::start(root.path().into(), instance, ingress.handle()).unwrap();
        let path = root.path().join("relay/state.json");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(service.forget().await.is_err());
        let status = service.status().await;
        assert!(status.gateway_id.is_none());
        assert!(!status.enabled);
        assert_eq!(status.error.as_deref(), Some("private_state_save_failed"));
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(store.load().unwrap().bound.is_some());
    }
}
