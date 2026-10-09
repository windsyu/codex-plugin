//! Outbound-only Bot API transport. No native execution, polling loop, or retry policy.
//! Unit tests use synthetic local HTTP servers, not a real Telegram account.

use std::time::Duration;

use reqwest::{StatusCode, redirect::Policy};
use serde_json::{Value, json};

const MAX_BODY_BYTES: usize = 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

// No Debug implementation: the endpoint contains the bot credential.
pub struct Client {
    http: reqwest::Client,
    endpoint: String,
}

pub struct BotIdentity {
    pub id: i64,
}

// Labels and references are private interaction content.
pub struct Button {
    pub label: String,
    pub reference: String,
}

/// Telegram accepted a send != user read it != native task completed.
/// Unknown must not be automatically replayed, including after a restart.
pub enum SendOutcome {
    Confirmed { chat_id: i64, message_id: i64 },
    Rejected { retry_after: Option<u32> },
    Unknown,
}

/// Safe categories only: never retain reqwest errors, URLs, or Telegram descriptions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    InvalidToken,
    InvalidRequest,
    ClientUnavailable,
    Connect,
    Timeout,
    Transport,
    ResponseTooLarge,
    InvalidResponse,
    UnexpectedStatus,
    ApiRejected { retry_after: Option<u32> },
    ExistingWebhook,
}

struct Reply {
    status: StatusCode,
    envelope: Value,
}

impl Client {
    pub fn new(token: String) -> Result<Self, Failure> {
        if !super::valid_token(&token) {
            return Err(Failure::InvalidToken);
        }
        Ok(Self {
            http: build_http(REQUEST_TIMEOUT)?,
            endpoint: format!("https://api.telegram.org/bot{token}/"),
        })
    }

    pub async fn inspect(&self) -> Result<BotIdentity, Failure> {
        let me = self.call("getMe", json!({})).await?.result()?;
        let id = positive(me.get("id")).ok_or(Failure::InvalidResponse)?;
        if me["is_bot"].as_bool() != Some(true) {
            return Err(Failure::InvalidResponse);
        }
        let webhook = self.call("getWebhookInfo", json!({})).await?.result()?;
        match webhook["url"].as_str() {
            Some("") => Ok(BotIdentity { id }),
            Some(_) => Err(Failure::ExistingWebhook),
            None => Err(Failure::InvalidResponse),
        }
    }

    pub async fn updates(
        &self,
        offset: Option<i64>,
        timeout_seconds: u8,
    ) -> Result<Vec<Value>, Failure> {
        if offset.is_some_and(|offset| offset < 0) || timeout_seconds > 20 {
            return Err(Failure::InvalidRequest);
        }
        let mut request = json!({
            "limit": 20,
            "timeout": timeout_seconds,
            "allowed_updates": ["message", "callback_query"],
        });
        if let Some(offset) = offset {
            request["offset"] = json!(offset);
        }
        let result = self.call("getUpdates", request).await?.result()?;
        let Value::Array(updates) = result else {
            return Err(Failure::InvalidResponse);
        };
        if updates.len() > 20 || updates.iter().any(|update| !update.is_object()) {
            return Err(Failure::InvalidResponse);
        }
        Ok(updates)
    }

    pub async fn send_text(&self, chat_id: i64, text: &str, buttons: &[Button]) -> SendOutcome {
        if chat_id <= 0
            || !valid_text(text, 4096)
            || buttons.len() > 3
            || buttons
                .iter()
                .any(|button| !valid_text(&button.label, 64) || !valid_reference(&button.reference))
        {
            return SendOutcome::Rejected { retry_after: None };
        }
        let mut request = json!({
            "chat_id": chat_id,
            "text": text,
            "link_preview_options": { "is_disabled": true },
        });
        if !buttons.is_empty() {
            request["reply_markup"] = json!({"inline_keyboard": [buttons.iter().map(|button| {
                json!({"text": button.label, "callback_data": button.reference})
            }).collect::<Vec<_>>() ]});
        }
        match self.call("sendMessage", request).await {
            Err(Failure::Connect) => SendOutcome::Rejected { retry_after: None },
            Err(_) => SendOutcome::Unknown,
            Ok(reply) => match reply.result() {
                Err(Failure::ApiRejected { retry_after }) => SendOutcome::Rejected { retry_after },
                Err(_) => SendOutcome::Unknown,
                Ok(message) => {
                    let Some(message_id) = positive(message.get("message_id")) else {
                        return SendOutcome::Unknown;
                    };
                    if positive(message["chat"].get("id")) != Some(chat_id)
                        || message["chat"]["type"].as_str() != Some("private")
                    {
                        return SendOutcome::Unknown;
                    }
                    SendOutcome::Confirmed {
                        chat_id,
                        message_id,
                    }
                }
            },
        }
    }

    /// Only acknowledges the Telegram callback spinner, never native execution.
    pub async fn answer_callback(&self, query_id: &str) -> Result<(), Failure> {
        if query_id.is_empty() || query_id.len() > 1024 || query_id.chars().any(char::is_control) {
            return Err(Failure::InvalidRequest);
        }
        if self
            .call(
                "answerCallbackQuery",
                json!({"callback_query_id": query_id}),
            )
            .await?
            .result()?
            .as_bool()
            != Some(true)
        {
            return Err(Failure::InvalidResponse);
        }
        Ok(())
    }

    async fn call(&self, method: &str, body: Value) -> Result<Reply, Failure> {
        let encoded = serde_json::to_vec(&body).map_err(|_| Failure::InvalidRequest)?;
        let mut response = self
            .http
            .post(format!("{}{method}", self.endpoint))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(encoded)
            .send()
            .await
            .map_err(safe_failure)?;
        let status = response.status();
        // Never follow a redirect or trust a 5xx body as proof that no send happened.
        if status.is_redirection() || status.is_server_error() {
            return Err(Failure::UnexpectedStatus);
        }
        if response
            .content_length()
            .is_some_and(|size| size > MAX_BODY_BYTES as u64)
        {
            return Err(Failure::ResponseTooLarge);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(safe_failure)? {
            if chunk.len() > MAX_BODY_BYTES.saturating_sub(bytes.len()) {
                return Err(Failure::ResponseTooLarge);
            }
            bytes.extend_from_slice(&chunk);
        }
        let envelope: Value =
            serde_json::from_slice(&bytes).map_err(|_| Failure::InvalidResponse)?;
        if !envelope.is_object() {
            return Err(Failure::InvalidResponse);
        }
        Ok(Reply { status, envelope })
    }

    #[cfg(test)]
    pub(crate) fn local(address: std::net::SocketAddr, timeout: Duration) -> Result<Self, Failure> {
        if !address.ip().is_loopback() {
            return Err(Failure::InvalidRequest);
        }
        Ok(Self {
            http: build_http(timeout)?,
            endpoint: format!("http://{address}/botsynthetic-secret/"),
        })
    }
}

impl Reply {
    fn result(self) -> Result<Value, Failure> {
        match self.envelope["ok"].as_bool() {
            Some(true) if self.status.is_success() => self
                .envelope
                .as_object()
                .and_then(|object| object.get("result"))
                .cloned()
                .ok_or(Failure::InvalidResponse),
            Some(false) if self.status.is_success() || self.status.is_client_error() => {
                let code = self.envelope["error_code"]
                    .as_i64()
                    .ok_or(Failure::InvalidResponse)?;
                if !(400..500).contains(&code) {
                    return Err(Failure::InvalidResponse);
                }
                let retry_after = if code == 429 {
                    match self.envelope["parameters"].get("retry_after") {
                        None => None,
                        Some(value) => Some(
                            value
                                .as_u64()
                                .and_then(|n| u32::try_from(n).ok())
                                .ok_or(Failure::InvalidResponse)?,
                        ),
                    }
                } else {
                    None
                };
                Err(Failure::ApiRejected { retry_after })
            }
            _ => Err(Failure::InvalidResponse),
        }
    }
}

fn build_http(timeout: Duration) -> Result<reqwest::Client, Failure> {
    let builder = reqwest::Client::builder()
        .redirect(Policy::none())
        .retry(reqwest::retry::never())
        .timeout(timeout)
        .connect_timeout(timeout.min(Duration::from_secs(10)));
    // Synthetic tests must not inherit a developer's outbound proxy configuration.
    #[cfg(test)]
    let builder = builder.no_proxy();
    builder.build().map_err(|_| Failure::ClientUnavailable)
}

fn safe_failure(error: reqwest::Error) -> Failure {
    if error.is_connect() {
        Failure::Connect
    } else if error.is_timeout() {
        Failure::Timeout
    } else {
        Failure::Transport
    }
}

fn positive(value: Option<&Value>) -> Option<i64> {
    value.and_then(Value::as_i64).filter(|id| *id > 0)
}

fn valid_reference(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn valid_text(value: &str, max_utf16: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_utf16 * 4
        && value.encode_utf16().count() <= max_utf16
        && !value
            .chars()
            .any(|ch| ch.is_control() && ch != '\n' && ch != '\t')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        task::JoinHandle,
    };

    #[derive(Clone)]
    enum Response {
        Http(u16, String, String),
        ChunkedOversized,
        LostAck,
        Stall,
    }

    struct Mock {
        address: std::net::SocketAddr,
        calls: Arc<AtomicUsize>,
        requests: Arc<Mutex<Vec<Value>>>,
        server: JoinHandle<()>,
    }

    impl Drop for Mock {
        fn drop(&mut self) {
            self.server.abort();
        }
    }

    impl Mock {
        async fn start(responses: Vec<Response>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let calls = Arc::new(AtomicUsize::new(0));
            let requests = Arc::new(Mutex::new(Vec::new()));
            let count = calls.clone();
            let captured = requests.clone();
            let server = tokio::spawn(async move {
                loop {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    let index = count.fetch_add(1, Ordering::SeqCst);
                    let mut bytes = Vec::new();
                    let header_end = loop {
                        let mut chunk = [0; 4096];
                        let size = stream.read(&mut chunk).await.unwrap();
                        if size == 0 {
                            return;
                        }
                        bytes.extend_from_slice(&chunk[..size]);
                        if let Some(end) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                            break end + 4;
                        }
                        assert!(bytes.len() < 65536);
                    };
                    let headers = String::from_utf8_lossy(&bytes[..header_end]);
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap();
                    while bytes.len() < header_end + length {
                        let mut chunk = [0; 4096];
                        let size = stream.read(&mut chunk).await.unwrap();
                        if size == 0 {
                            return;
                        }
                        bytes.extend_from_slice(&chunk[..size]);
                    }
                    captured.lock().unwrap().push(
                        serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap(),
                    );
                    let response = responses
                        .get(index)
                        .unwrap_or_else(|| responses.last().unwrap());
                    match response {
                        Response::Http(status, extra, body) => {
                            let response = format!(
                                "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n{extra}\r\n{body}",
                                body.len()
                            );
                            let _ = stream.write_all(response.as_bytes()).await;
                        }
                        Response::LostAck => {}
                        Response::ChunkedOversized => {
                            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n").await;
                            let chunk = format!("10000\r\n{}\r\n", "x".repeat(65536));
                            for _ in 0..17 {
                                if stream.write_all(chunk.as_bytes()).await.is_err() {
                                    break;
                                }
                            }
                            let _ = stream.write_all(b"0\r\n\r\n").await;
                        }
                        Response::Stall => {
                            std::future::pending::<()>().await;
                        }
                    }
                }
            });
            Self {
                address,
                calls,
                requests,
                server,
            }
        }

        fn client(&self) -> Client {
            Client::local(self.address, Duration::from_secs(2)).unwrap()
        }
    }

    fn ok(result: Value) -> Response {
        Response::Http(
            200,
            String::new(),
            json!({"ok":true,"result":result}).to_string(),
        )
    }

    fn sent(chat: i64) -> Response {
        ok(json!({"message_id":11,"chat":{"id":chat,"type":"private"}}))
    }

    #[test]
    fn token_format_rejects_url_injection_and_has_no_error_payload() {
        for token in [
            "",
            "0:secret",
            "abc:secret",
            "1:secret/path",
            "1:secret?query",
            "1:secret\n",
            "1:x:y",
            "9223372036854775808:secret",
        ] {
            assert!(matches!(
                Client::new(token.to_owned()),
                Err(Failure::InvalidToken)
            ));
        }
        assert!(Client::new("123:synthetic_secret-123".to_owned()).is_ok());
        assert_eq!(format!("{:?}", Failure::InvalidToken), "InvalidToken");
    }

    #[tokio::test]
    async fn inspect_and_poll_validate_results_and_bounded_request() {
        let mock = Mock::start(vec![
            ok(json!({"id":123,"is_bot":true})),
            ok(json!({"url":""})),
            ok(json!([{"update_id":0}])),
        ])
        .await;
        let client = mock.client();
        assert_eq!(client.inspect().await.unwrap().id, 123);
        assert_eq!(client.updates(Some(0), 20).await.unwrap().len(), 1);
        {
            let requests = mock.requests.lock().unwrap();
            assert_eq!(
                requests[2],
                json!({"offset":0,"timeout":20,"limit":20,"allowed_updates":["message","callback_query"]})
            );
        }
        assert!(matches!(
            client.updates(Some(-1), 20).await,
            Err(Failure::InvalidRequest)
        ));
        assert!(matches!(
            client.updates(None, 21).await,
            Err(Failure::InvalidRequest)
        ));
        assert_eq!(mock.calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn webhook_is_not_deleted_and_invalid_bot_fails_closed() {
        let mock = Mock::start(vec![
            ok(json!({"id":123,"is_bot":true})),
            ok(json!({"url":"https://example.invalid/private"})),
        ])
        .await;
        assert!(matches!(
            mock.client().inspect().await,
            Err(Failure::ExistingWebhook)
        ));
        assert_eq!(mock.calls.load(Ordering::SeqCst), 2);
        let mock = Mock::start(vec![ok(json!({"id":123,"is_bot":false}))]).await;
        assert!(matches!(
            mock.client().inspect().await,
            Err(Failure::InvalidResponse)
        ));
        assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn send_is_plain_text_and_unicode_bounded() {
        let mock = Mock::start(vec![sent(7)]).await;
        let client = mock.client();
        let buttons = [Button {
            label: "允许".into(),
            reference: "opaque_123-abc".into(),
        }];
        assert!(matches!(
            client.send_text(7, &"😀".repeat(2048), &buttons).await,
            SendOutcome::Confirmed {
                chat_id: 7,
                message_id: 11
            }
        ));
        assert!(matches!(
            client.send_text(7, &"😀".repeat(2049), &[]).await,
            SendOutcome::Rejected { retry_after: None }
        ));
        let request = &mock.requests.lock().unwrap()[0];
        assert!(request.get("parse_mode").is_none());
        assert_eq!(request["link_preview_options"]["is_disabled"], true);
        assert_eq!(
            request["reply_markup"]["inline_keyboard"][0][0]["callback_data"],
            "opaque_123-abc"
        );
        assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn rejects_bad_buttons_without_sending() {
        let mock = Mock::start(vec![sent(7)]).await;
        for reference in ["", "execute command", "x/y", &"x".repeat(65)] {
            let buttons = [Button {
                label: "label".into(),
                reference: reference.into(),
            }];
            assert!(matches!(
                mock.client().send_text(7, "hello", &buttons).await,
                SendOutcome::Rejected { retry_after: None }
            ));
        }
        assert_eq!(mock.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn rejection_requires_explicit_envelope_and_server_errors_are_unknown() {
        let mock = Mock::start(vec![Response::Http(429, String::new(), json!({"ok":false,"error_code":429,"parameters":{"retry_after":3},"description":"private ignored"}).to_string())]).await;
        assert!(matches!(
            mock.client().send_text(7, "hello", &[]).await,
            SendOutcome::Rejected {
                retry_after: Some(3)
            }
        ));
        for response in [
            Response::Http(200, String::new(), "{}".into()),
            Response::Http(200, String::new(), "not JSON".into()),
            Response::Http(
                500,
                String::new(),
                json!({"ok":false,"error_code":429}).to_string(),
            ),
            sent(8),
        ] {
            let mock = Mock::start(vec![response]).await;
            assert!(matches!(
                mock.client().send_text(7, "hello", &[]).await,
                SendOutcome::Unknown
            ));
            assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
        }
        let mock = Mock::start(vec![Response::Http(
            200,
            String::new(),
            json!({"ok":false,"error_code":403}).to_string(),
        )])
        .await;
        assert!(matches!(
            mock.client().send_text(7, "hello", &[]).await,
            SendOutcome::Rejected { retry_after: None }
        ));
    }

    #[tokio::test]
    async fn lost_ack_and_timeout_are_unknown_without_replay() {
        for response in [Response::LostAck, Response::Stall] {
            let mock = Mock::start(vec![response]).await;
            let client = Client::local(mock.address, Duration::from_millis(100)).unwrap();
            assert!(matches!(
                client.send_text(7, "hello", &[]).await,
                SendOutcome::Unknown
            ));
            assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
        }
    }

    #[tokio::test]
    async fn redirect_is_not_followed_and_never_exposes_token_to_target() {
        let target = Mock::start(vec![sent(7)]).await;
        let mock = Mock::start(vec![Response::Http(
            307,
            format!("Location: http://{}/stolen\r\n", target.address),
            String::new(),
        )])
        .await;
        assert!(matches!(
            mock.client().send_text(7, "hello", &[]).await,
            SendOutcome::Unknown
        ));
        assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
        assert_eq!(target.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn oversized_response_and_update_batch_fail_closed() {
        let mock = Mock::start(vec![Response::ChunkedOversized]).await;
        assert!(matches!(
            mock.client().updates(None, 0).await,
            Err(Failure::ResponseTooLarge)
        ));
        let mock = Mock::start(vec![Response::Http(
            200,
            String::new(),
            "x".repeat(MAX_BODY_BYTES + 1),
        )])
        .await;
        assert!(matches!(
            mock.client().updates(None, 0).await,
            Err(Failure::ResponseTooLarge)
        ));
        let mock = Mock::start(vec![ok(json!(vec![json!({"update_id":1}); 21]))]).await;
        assert!(matches!(
            mock.client().updates(None, 0).await,
            Err(Failure::InvalidResponse)
        ));
    }

    #[tokio::test]
    async fn callback_ack_requires_true_and_sends_no_arbitrary_body() {
        let mock = Mock::start(vec![ok(json!(true)), ok(json!(false))]).await;
        let client = mock.client();
        assert!(client.answer_callback("synthetic-query").await.is_ok());
        assert_eq!(
            mock.requests.lock().unwrap()[0],
            json!({"callback_query_id":"synthetic-query"})
        );
        assert!(matches!(
            client.answer_callback("synthetic-query").await,
            Err(Failure::InvalidResponse)
        ));
    }
}
