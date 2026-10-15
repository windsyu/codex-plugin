//! Private local lifecycle control. Never part of the remote owner API.
use super::*;
use serde_json::Value;

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Status,
    Shutdown,
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Request {
    pub instance_id: Uuid,
    pub token: String,
    pub action: Action,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub instance_id: Uuid,
    pub process_id: u32,
    pub service_label: Option<String>,
    pub stopping: bool,
    pub active_runs: usize,
    pub relay: Option<RelayStatus>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayStatus {
    pub state: String,
    pub registered: bool,
    pub enabled: bool,
    pub authentication_required: bool,
}
impl Status {
    pub fn registration_required(&self) -> bool {
        self.relay
            .as_ref()
            .is_some_and(|r| !r.registered || r.authentication_required)
    }
}

pub async fn control(entry: &BrowserEntry, action: Action) -> Result<Status> {
    let id = entry
        .instance_id
        .ok_or_else(|| anyhow::anyhow!("application entry v2 required"))?;
    let url = reqwest::Url::parse(&entry.url)?;
    let token = url
        .fragment()
        .and_then(|s| s.strip_prefix("pair="))
        .ok_or_else(|| anyhow::anyhow!("invalid private entry"))?
        .to_owned();
    let response = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .build()?
        .post(format!(
            "{}/workbench/v1/application/control",
            entry.address
        ))
        .header("Origin", &entry.address)
        .header("Content-Type", "application/json")
        .body(serde_json::to_vec(&Request {
            instance_id: id,
            token,
            action,
        })?)
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("local application control unavailable"))?;
    ensure!(
        response.status().is_success(),
        "local application control rejected ({})",
        response.status()
    );
    let bytes = response.bytes().await?;
    ensure!(
        bytes.len() <= 16 * 1024,
        "application control response too large"
    );
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("invalid control response"))?;
    let status: Status =
        serde_json::from_value(value).map_err(|_| anyhow::anyhow!("invalid control response"))?;
    ensure!(status.instance_id == id, "application instance changed");
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_missing_or_rejected_registration_needs_login() {
        let mut s = Status {
            instance_id: Uuid::new_v4(),
            process_id: 1,
            service_label: None,
            stopping: false,
            active_runs: 0,
            relay: None,
        };
        assert!(!s.registration_required());
        s.relay = Some(RelayStatus {
            state: "disabled".into(),
            registered: false,
            enabled: false,
            authentication_required: false,
        });
        assert!(s.registration_required());
        for state in ["disabled", "offline", "connecting", "connected"] {
            s.relay = Some(RelayStatus {
                state: state.into(),
                registered: true,
                enabled: state != "disabled",
                authentication_required: false,
            });
            assert!(!s.registration_required());
        }
        s.relay.as_mut().unwrap().authentication_required = true;
        assert!(s.registration_required());
    }
}
