//! A private loopback ingress for the authenticated outbound connector.
//! Network headers alone never construct an owner on existing local/LAN servers.
use crate::workbench::permission::Permission;
use anyhow::Result;
use axum::extract::FromRequestParts;
use axum::extract::State;
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::net::{Ipv4Addr, SocketAddr};
use std::ops::Deref;
use std::sync::Arc;
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::watch;
use uuid::Uuid;

pub(crate) fn secret() -> String {
    rand::random::<[u8; 32]>()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
pub(crate) fn matches_secret(value: &str, expected: &str) -> bool {
    value.len() == expected.len()
        && value
            .bytes()
            .zip(expected.bytes())
            .fold(0u8, |difference, (a, b)| difference | (a ^ b))
            == 0
}

fn error(status: StatusCode, code: &'static str) -> Response {
    (
        status,
        Json(serde_json::json!({"error":{"source":"remote_access","code":code}})),
    )
        .into_response()
}

#[derive(Clone)]
pub(crate) struct RemoteOwner {
    instance_id: Uuid,
    origin: String,
    permission: Permission,
}
impl RemoteOwner {
    pub(crate) fn permission(&self) -> Permission {
        self.permission.clone()
    }
    pub(crate) fn active(&self) -> bool {
        self.permission.active()
    }
    pub(crate) fn instance_id(&self) -> Uuid {
        self.instance_id
    }
}

/// Carries a trusted request extension alongside ordinary untrusted headers.
#[derive(Clone)]
pub(crate) struct Auth {
    headers: HeaderMap,
    pub(crate) remote: Option<RemoteOwner>,
}
impl Auth {
    pub(crate) fn from_request(request: &axum::extract::Request) -> Self {
        Self {
            headers: request.headers().clone(),
            remote: request.extensions().get::<RemoteOwner>().cloned(),
        }
    }
    pub(crate) fn remote_origin(&self) -> bool {
        self.remote.as_ref().is_some_and(|owner| {
            owner.active()
                && self.get_all(header::ORIGIN).iter().count() == 1
                && self.get(header::ORIGIN).and_then(|v| v.to_str().ok())
                    == Some(owner.origin.as_str())
        })
    }
}
impl From<HeaderMap> for Auth {
    fn from(headers: HeaderMap) -> Self {
        Self {
            headers,
            remote: None,
        }
    }
}
impl Deref for Auth {
    type Target = HeaderMap;
    fn deref(&self) -> &HeaderMap {
        &self.headers
    }
}
impl<S: Send + Sync> FromRequestParts<S> for Auth {
    type Rejection = Infallible;
    async fn from_request_parts(
        parts: &mut Parts,
        _: &S,
    ) -> std::result::Result<Self, Self::Rejection> {
        Ok(Self {
            headers: parts.headers.clone(),
            remote: parts.extensions.get::<RemoteOwner>().cloned(),
        })
    }
}

struct Context {
    account: Uuid,
    gateway: Uuid,
    epoch: Uuid,
    origin: String,
}
struct Session {
    token: String,
    owner: RemoteOwner,
}
struct Book {
    instance: Uuid,
    context: Option<Context>,
    sessions: HashMap<Uuid, Session>,
    revoked: HashSet<Uuid>,
}
#[derive(Clone)]
pub(crate) struct IngressHandle {
    address: SocketAddr,
    book: Arc<Mutex<Book>>,
}
impl IngressHandle {
    pub(crate) fn address(&self) -> SocketAddr {
        self.address
    }
    pub(crate) fn begin(
        &self,
        account: Uuid,
        gateway: Uuid,
        epoch: Uuid,
        origin: String,
    ) -> Result<()> {
        let parsed = reqwest::Url::parse(&origin)?;
        anyhow::ensure!(
            matches!(parsed.scheme(), "http" | "https")
                && parsed.origin().ascii_serialization() == origin,
            "invalid relay origin"
        );
        anyhow::ensure!(
            !account.is_nil() && !gateway.is_nil() && !epoch.is_nil(),
            "invalid relay identity"
        );
        self.disconnect();
        self.book.lock().unwrap().context = Some(Context {
            account,
            gateway,
            epoch,
            origin,
        });
        Ok(())
    }
    pub(crate) fn grant(&self, id: Uuid) -> Result<String> {
        let mut book = self.book.lock().unwrap();
        anyhow::ensure!(
            !id.is_nil() && !book.revoked.contains(&id),
            "relay session revoked"
        );
        if let Some(session) = book.sessions.get(&id) {
            anyhow::ensure!(session.owner.active(), "relay session expired");
            return Ok(session.token.clone());
        }
        anyhow::ensure!(book.sessions.len() < 256, "relay session capacity");
        let context = book
            .context
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("relay disconnected"))?;
        // These are set only by the credential-authenticated connector handshake.
        anyhow::ensure!(
            !context.account.is_nil() && !context.gateway.is_nil() && !context.epoch.is_nil(),
            "invalid relay context"
        );
        let owner = RemoteOwner {
            instance_id: book.instance,
            origin: context.origin.clone(),
            permission: Permission::leased(Duration::from_secs(
                codex_relay_protocol::LEASE_SECONDS,
            )),
        };
        let token = secret();
        book.sessions.insert(
            id,
            Session {
                token: token.clone(),
                owner,
            },
        );
        Ok(token)
    }
    pub(crate) fn renew(&self, ids: &[Uuid]) {
        let book = self.book.lock().unwrap();
        for id in ids.iter().take(256) {
            if let Some(session) = book.sessions.get(id) {
                session
                    .owner
                    .permission
                    .renew(Duration::from_secs(codex_relay_protocol::LEASE_SECONDS));
            }
        }
    }
    pub(crate) fn revoke(&self, id: Uuid) {
        let mut book = self.book.lock().unwrap();
        if let Some(session) = book.sessions.remove(&id) {
            session.owner.permission.revoke();
        }
        book.revoked.insert(id);
        // A malicious/failed peer cannot grow tombstones indefinitely.
        if book.revoked.len() > 4096 {
            drop(book);
            self.disconnect();
        }
    }
    pub(crate) fn disconnect(&self) {
        let mut book = self.book.lock().unwrap();
        for session in book.sessions.values() {
            session.owner.permission.revoke();
        }
        book.sessions.clear();
        book.revoked.clear();
        book.context = None;
    }
    fn authenticate(&self, headers: &HeaderMap) -> Option<RemoteOwner> {
        if headers.get_all(header::AUTHORIZATION).iter().count() != 1 {
            return None;
        }
        let token = headers
            .get(header::AUTHORIZATION)?
            .to_str()
            .ok()?
            .strip_prefix("Bearer ")?;
        let book = self.book.lock().unwrap();
        book.context.as_ref()?;
        book.sessions
            .values()
            .find(|session| matches_secret(token, &session.token) && session.owner.active())
            .map(|s| s.owner.clone())
    }
}

pub(crate) struct Ingress {
    handle: IngressHandle,
    stop: watch::Sender<bool>,
    thread: Option<JoinHandle<()>>,
}
impl Ingress {
    pub(crate) fn start(app: Router, instance: Uuid) -> Result<Self> {
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let handle = IngressHandle {
            address: listener.local_addr()?,
            book: Arc::new(Mutex::new(Book {
                instance,
                context: None,
                sessions: HashMap::new(),
                revoked: HashSet::new(),
            })),
        };
        let app = app.layer(middleware::from_fn_with_state(handle.clone(), guard));
        let (stop, mut stopped) = watch::channel(false);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let background = handle.clone();
        let thread = std::thread::Builder::new().name("relay-ingress".into()).spawn(move || runtime.block_on(async move {
            let expiry = async {
                let mut tick = tokio::time::interval(Duration::from_millis(250));
                loop { tick.tick().await; let book = background.book.lock().unwrap(); for session in book.sessions.values() { let _ = session.owner.active(); } }
            };
            tokio::select! { biased; _ = stopped.changed() => {}, _ = expiry => {}, _ = axum::serve(TcpListener::from_std(listener).expect("relay ingress listener"), app) => {} }
            background.disconnect();
        }))?;
        Ok(Self {
            handle,
            stop,
            thread: Some(thread),
        })
    }
    pub(crate) fn handle(&self) -> IngressHandle {
        self.handle.clone()
    }
}
impl Drop for Ingress {
    fn drop(&mut self) {
        self.handle.disconnect();
        let _ = self.stop.send(true);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

async fn guard(
    State(handle): State<IngressHandle>,
    mut request: axum::extract::Request,
    next: Next,
) -> Response {
    if !codex_relay_protocol::allowed(request.method().as_str(), &request.uri().to_string()) {
        return error(StatusCode::FORBIDDEN, "relay_route_forbidden");
    }
    let Some(owner) = handle.authenticate(request.headers()) else {
        return error(StatusCode::UNAUTHORIZED, "relay_session_required");
    };
    let headers = request.headers();
    let authority = owner
        .origin
        .split_once("://")
        .map(|(_, authority)| authority)
        .unwrap_or("");
    if headers.get_all(header::HOST).iter().count() != 1
        || headers.get(header::HOST).and_then(|v| v.to_str().ok()) != Some(authority)
        || headers.get_all(header::ORIGIN).iter().count() > 1
        || headers
            .get(header::ORIGIN)
            .is_some_and(|v| v.to_str().ok() != Some(owner.origin.as_str()))
        || (request.method() != axum::http::Method::GET && !headers.contains_key(header::ORIGIN))
    {
        return error(StatusCode::FORBIDDEN, "invalid_origin_or_host");
    }
    request.headers_mut().remove(header::AUTHORIZATION);
    request.headers_mut().remove(header::COOKIE);
    request.extensions_mut().insert(owner);
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::get;
    #[tokio::test]
    async fn private_ingress_requires_live_grant_and_rejects_escape_routes() {
        let app = Router::new().route(
            "/workbench/v1/application",
            get(|auth: Auth| async move {
                if auth.remote.is_some_and(|owner| owner.active()) {
                    StatusCode::OK
                } else {
                    StatusCode::UNAUTHORIZED
                }
            }),
        );
        let ingress = Ingress::start(app, Uuid::new_v4()).unwrap();
        let handle = ingress.handle();
        handle
            .begin(
                Uuid::new_v4(),
                Uuid::new_v4(),
                Uuid::new_v4(),
                "https://g.example.com".into(),
            )
            .unwrap();
        let id = Uuid::new_v4();
        let cap = handle.grant(id).unwrap();
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let url = format!("http://{}", handle.address());
        assert_eq!(
            client
                .get(format!("{url}/workbench/v1/application"))
                .header(header::HOST, "g.example.com")
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            client
                .get(format!("{url}/workbench/v1/application"))
                .header(header::HOST, "g.example.com")
                .bearer_auth(&cap)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            client
                .post(format!("{url}/workbench/v1/application/connect"))
                .header(header::HOST, "g.example.com")
                .bearer_auth(&cap)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        handle.revoke(id);
        assert!(handle.grant(id).is_err());
        assert_eq!(
            client
                .get(format!("{url}/workbench/v1/application"))
                .header(header::HOST, "g.example.com")
                .bearer_auth(&cap)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        handle.disconnect();
        assert!(handle.grant(Uuid::new_v4()).is_err());
    }
}
