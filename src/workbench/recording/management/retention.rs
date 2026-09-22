use crate::workbench::config::Config;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use uuid::Uuid;

pub(super) struct Scheduler {
    pub policy_at: Instant,
    pub due: Instant,
    pub revision: Option<String>,
    pub enabled: bool,
    pub days: u32,
    pub preview: Option<Uuid>,
    pub clock_blocked: bool,
    pub state: &'static str,
    pub last_error: Option<&'static str>,
    pub last_check: Option<String>,
    pub last_job: Option<Uuid>,
    pub skipped: std::collections::BTreeMap<String, u64>,
    pub scan_complete: Option<bool>,
    clock: (Instant, DateTime<Utc>),
}
impl Scheduler {
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            policy_at: now,
            due: now,
            revision: None,
            enabled: false,
            days: 90,
            preview: None,
            clock_blocked: false,
            state: "disabled",
            last_error: None,
            last_check: None,
            last_job: None,
            skipped: Default::default(),
            scan_complete: None,
            clock: (now, Utc::now()),
        }
    }
    pub fn observe_clock(&mut self, mono: Instant, wall: DateTime<Utc>) {
        let elapsed = mono.saturating_duration_since(self.clock.0).as_secs_f64();
        let wall_elapsed =
            wall.signed_duration_since(self.clock.1).num_milliseconds() as f64 / 1000.0;
        if (elapsed - wall_elapsed).abs() > 300.0 {
            self.clock_blocked = true;
            self.state = "clock_changed";
        }
        self.clock = (mono, wall);
    }
    pub fn policy(&mut self, value: Option<(Config, String)>) -> bool {
        let revision = value.as_ref().map(|(_, r)| r.clone());
        let changed = revision != self.revision;
        self.revision = revision;
        if let Some((config, _)) = value {
            self.enabled =
                config.history.cleanup.enabled && config.history.cleanup.retention.enabled;
            self.days = config.history.cleanup.retention.days;
        } else {
            self.enabled = false;
        }
        if changed {
            self.preview = None;
            self.last_error = None;
            self.due = Instant::now();
        }
        if !self.enabled {
            self.state = if self.revision.is_none() {
                "config_unavailable"
            } else {
                "disabled"
            };
        } else if self.clock_blocked {
            self.state = "clock_changed";
        } else if changed {
            self.state = "scheduled";
        }
        changed
    }
    pub fn complete(&mut self, id: Option<Uuid>) {
        self.preview = None;
        self.last_job = id;
        self.last_check = Some(Utc::now().to_rfc3339());
        self.due = Instant::now() + Duration::from_secs(3600);
        self.state = "waiting";
        self.last_error = None;
    }
    pub fn retry(&mut self, reason: &'static str) {
        self.preview = None;
        self.last_check = Some(Utc::now().to_rfc3339());
        self.last_error = Some(reason);
        self.due = Instant::now() + Duration::from_secs(60);
        self.state = "check_failed";
    }
    pub fn value(&self) -> Value {
        json!({"enabled":self.enabled,"days":self.days,"state":self.state,"lastError":self.last_error,"lastCheckAt":self.last_check,"lastJobId":self.last_job,"scanComplete":self.scan_complete,"skippedCounts":self.skipped,"nextCheckAt":(self.enabled&&!self.clock_blocked).then(||(Utc::now()+chrono::Duration::from_std(self.due.saturating_duration_since(Instant::now())).unwrap_or_default()).to_rfc3339())})
    }
}
