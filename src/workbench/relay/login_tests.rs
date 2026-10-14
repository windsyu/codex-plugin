use super::*;
use crate::workbench::remote_access::Ingress;
use axum::{
    Json, Router,
    http::StatusCode,
    routing::{get, post},
};
use std::sync::atomic::{AtomicUsize, Ordering};

struct LoginServer {
    origin: String,
    polls: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
    connections: Arc<AtomicUsize>,
    bound: Bound,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for LoginServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl LoginServer {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let polls = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let connections = Arc::new(AtomicUsize::new(0));
        let bound = Bound {
            username: "synthetic".into(),
            account_id: Uuid::new_v4(),
            gateway_id: Uuid::new_v4(),
            credential: "synthetic-new-credential".into(),
            gateway_origin: origin.clone(),
        };
        let pending = BindingPending {
            binding_id: Uuid::new_v4(),
            device_code: "synthetic-device".into(),
            user_code: "SYNTHETIC".into(),
            verification_url: format!("{origin}/#bind=SYNTHETIC"),
            expires_in_seconds: 60,
            interval: 5,
        };
        let output = bound.clone();
        let reached = polls.clone();
        let wait = release.clone();
        let seen = connections.clone();
        let app = Router::new()
            .route(
                "/relay/v1/bindings",
                post(move || {
                    let pending = pending.clone();
                    async move { Json(pending) }
                }),
            )
            .route(
                "/relay/v1/bindings/{id}/poll",
                post(move || {
                    let bound = output.clone();
                    let reached = reached.clone();
                    let wait = wait.clone();
                    async move {
                        reached.notify_one();
                        wait.notified().await;
                        Json(bound)
                    }
                }),
            )
            .route(
                "/relay/v1/tunnel",
                get(move || {
                    let seen = seen.clone();
                    async move {
                        seen.fetch_add(1, Ordering::SeqCst);
                        StatusCode::UNAUTHORIZED
                    }
                }),
            )
            .route(
                "/relay/v1/gateway/revoke",
                post(|| async { StatusCode::NO_CONTENT }),
            );
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            origin,
            polls,
            release,
            connections,
            bound,
            task,
        }
    }
    async fn polling(&self) {
        tokio::time::timeout(Duration::from_secs(10), self.polls.notified())
            .await
            .unwrap();
    }
}

async fn wait_state(service: &Service, state: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if service.status().await.state == state {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn approved_login_persists_then_connects_and_rejected_credential_stays_stopped_on_restart() {
    let server = LoginServer::start().await;
    let root = tempfile::tempdir().unwrap();
    let instance = Uuid::new_v4();
    let ingress = Ingress::start(Router::new(), instance).unwrap();
    let service = Service::start(root.path().into(), instance, ingress.handle()).unwrap();
    service
        .bind(server.origin.clone(), "Synthetic".into(), true)
        .await
        .unwrap();
    server.polling().await;
    assert_eq!(server.connections.load(Ordering::SeqCst), 0);
    server.release.notify_one();
    wait_state(&service, "authenticationRequired").await;
    let saved = Storage::open(root.path()).unwrap().load().unwrap();
    assert_eq!(saved.bound.unwrap().gateway_id, server.bound.gateway_id);
    assert!(saved.authentication_required);
    assert!(!saved.enabled);
    assert_eq!(
        service.status().await.username.as_deref(),
        Some("synthetic")
    );
    assert_eq!(server.connections.load(Ordering::SeqCst), 1);
    assert!(service.enable().await.is_err());
    drop(service);
    let restarted = Service::start(root.path().into(), instance, ingress.handle()).unwrap();
    assert_eq!(restarted.status().await.state, "authenticationRequired");
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(server.connections.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cancel_fences_inflight_approval_and_never_enables_connection() {
    let server = LoginServer::start().await;
    let root = tempfile::tempdir().unwrap();
    let instance = Uuid::new_v4();
    let ingress = Ingress::start(Router::new(), instance).unwrap();
    let service = Service::start(root.path().into(), instance, ingress.handle()).unwrap();
    service
        .bind(server.origin.clone(), "Synthetic".into(), true)
        .await
        .unwrap();
    server.polling().await;
    service.disable().await.unwrap();
    server.release.notify_one();
    tokio::time::sleep(Duration::from_millis(150)).await;
    let status = service.status().await;
    assert!(!status.enabled);
    assert!(status.binding.is_none());
    assert!(status.gateway_id.is_none());
    assert_eq!(server.connections.load(Ordering::SeqCst), 0);
    assert!(
        Storage::open(root.path())
            .unwrap()
            .load()
            .unwrap()
            .bound
            .is_none()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn unsafe_private_storage_rejects_approved_credential_without_connecting() {
    use std::os::unix::fs::PermissionsExt;
    let server = LoginServer::start().await;
    let root = tempfile::tempdir().unwrap();
    let instance = Uuid::new_v4();
    let ingress = Ingress::start(Router::new(), instance).unwrap();
    let service = Service::start(root.path().into(), instance, ingress.handle()).unwrap();
    service
        .bind(server.origin.clone(), "Synthetic".into(), true)
        .await
        .unwrap();
    server.polling().await;
    let path = root.path().join("relay/state.json");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    server.release.notify_one();
    wait_state(&service, "error").await;
    assert_eq!(
        service.status().await.error.as_deref(),
        Some("credential_save_failed")
    );
    assert!(!service.status().await.enabled);
    assert_eq!(server.connections.load(Ordering::SeqCst), 0);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(
        Storage::open(root.path())
            .unwrap()
            .load()
            .unwrap()
            .bound
            .is_none()
    );
}
