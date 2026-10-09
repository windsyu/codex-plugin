//! TUI subscription evidence only. The caller verifies capture epoch, continuity,
//! and liveness before every observation and use. Never reconstruct from history.
use super::native::Direction;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use uuid::Uuid;

const MAX_PENDING: usize = 64;
const MAX_TARGETS: usize = 32;
const MAX_IDS: usize = 256;
const MAX_ID_BYTES: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target {
    pub thread_id: Uuid,
    pub busy: bool,
}
#[derive(Default, Debug, PartialEq, Eq)]
pub struct Change {
    pub removed: Vec<Uuid>,
    pub all_invalidated: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    Malformed,
    ReusedId,
    Capacity,
    Poisoned,
}
// No Debug: request identifiers must not enter diagnostic output.
#[derive(Clone, PartialEq, Eq, Hash)]
enum RpcId {
    Text(String),
    Number(String),
}
struct Pending {
    expected: Option<Uuid>,
    // Only events observed after this request; never infer subscription from them.
    statuses: HashMap<Uuid, Result<Option<bool>, Failure>>,
}
#[derive(Default)]
pub struct Tracker {
    pending: HashMap<RpcId, Pending>,
    // Never evicted: an old response cannot match a reused identifier.
    used: HashSet<RpcId>,
    targets: BTreeMap<Uuid, bool>,
    poisoned: bool,
}
impl Tracker {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn targets(&self) -> Vec<Target> {
        self.targets
            .iter()
            .map(|(&thread_id, &busy)| Target { thread_id, busy })
            .collect()
    }
    /// Any error permanently invalidates this tracker. The caller must revoke
    /// its binding; a fresh tracker requires fresh complete transport evidence.
    pub fn observe(&mut self, direction: Direction, value: &Value) -> Result<Change, Failure> {
        if self.poisoned {
            return Err(Failure::Poisoned);
        }
        let result = self.apply(direction, value);
        if result.is_err() {
            self.pending.clear();
            self.targets.clear();
            self.poisoned = true;
        }
        result
    }
    fn apply(&mut self, direction: Direction, value: &Value) -> Result<Change, Failure> {
        let mut change = Change::default();
        let method = value.get("method").and_then(Value::as_str);
        if direction == Direction::ToServer {
            // Client responses to server requests cannot establish subscription.
            let Some(method) = method else {
                return Ok(change);
            };
            if let Some(id) = value.get("id").and_then(|v| rpc_id(v).ok())
                && self.used.contains(&id)
            {
                return Err(Failure::ReusedId);
            }
            if method == "thread/unsubscribe" {
                let thread = thread_id(&value["params"]["threadId"])?;
                self.pending.clear(); // invalidate all earlier subscription generations
                if self.targets.remove(&thread).is_some() {
                    change.removed.push(thread);
                }
                return Ok(change);
            }
            if !matches!(method, "thread/start" | "thread/resume" | "thread/fork") {
                return Ok(change);
            }
            if !value["params"].is_object()
                || value.get("result").is_some()
                || value.get("error").is_some()
            {
                return Err(Failure::Malformed);
            }
            let id = rpc_id(&value["id"])?;
            let expected = match method {
                "thread/start" => None,
                "thread/resume" => Some(thread_id(&value["params"]["threadId"])?),
                _ => {
                    thread_id(&value["params"]["threadId"])?;
                    None
                }
            };
            if self.pending.len() >= MAX_PENDING || self.used.len() >= MAX_IDS {
                return Err(Failure::Capacity);
            }
            self.used.insert(id.clone());
            self.pending.insert(
                id,
                Pending {
                    expected,
                    statuses: HashMap::new(),
                },
            );
            return Ok(change);
        }
        if let Some(method) = method {
            // Server requests are not responses, even with a colliding ID.
            if value.get("id").is_some() {
                return Ok(change);
            }
            if !matches!(method, "thread/closed" | "thread/status/changed") {
                return Ok(change);
            }
            let Ok(thread) = thread_id(&value["params"]["threadId"]) else {
                // No identifiable target: this notification cannot change authority.
                return Ok(change);
            };
            if method == "thread/closed" {
                self.pending.clear();
                if self.targets.remove(&thread).is_some() {
                    change.removed.push(thread);
                }
            } else {
                let observed = status(Some(&value["params"]["status"]));
                for pending in self.pending.values_mut() {
                    if pending.expected.is_some_and(|expected| expected != thread) {
                        continue;
                    }
                    if !pending.statuses.contains_key(&thread)
                        && pending.statuses.len() >= MAX_TARGETS
                    {
                        return Err(Failure::Capacity);
                    }
                    // Terminal/unknown evidence cannot be undone by a later idle
                    // notification while the same request still awaits its ACK.
                    let entry = pending.statuses.entry(thread).or_insert(observed);
                    if matches!(entry, Ok(Some(_))) {
                        *entry = observed;
                    }
                }
                if let std::collections::btree_map::Entry::Occupied(mut entry) =
                    self.targets.entry(thread)
                {
                    match observed? {
                        Some(busy) => {
                            *entry.get_mut() = busy;
                        }
                        None => {
                            entry.remove();
                            change.removed.push(thread);
                        }
                    }
                }
            }
            return Ok(change);
        }
        let Some(id) = value.get("id").and_then(|v| rpc_id(v).ok()) else {
            return Ok(change);
        };
        let Some(pending) = self.pending.remove(&id) else {
            return Ok(change);
        };
        match (value.get("result"), value.get("error")) {
            (None, Some(error))
                if error.is_object()
                    && error["code"].as_i64().is_some()
                    && error["message"].is_string() =>
            {
                return Ok(change);
            }
            (Some(result), None) if result.is_object() => {}
            _ => return Err(Failure::Malformed),
        }
        let thread = &value["result"]["thread"];
        let id = thread_id(&thread["id"])?;
        if pending.expected.is_some_and(|expected| expected != id) {
            return Err(Failure::Malformed);
        }
        // Public SessionSource schema: known ordinary sources are strings;
        // subAgent/custom are tagged objects. Absent/unknown sources never grant.
        if !matches!(
            thread.get("source").and_then(Value::as_str),
            Some("cli" | "vscode" | "exec" | "appServer")
        ) {
            if self.targets.remove(&id).is_some() {
                change.removed.push(id);
            }
            return Ok(change);
        }
        let response_status = status(thread.get("status"))?;
        let effective_status = if response_status.is_none() {
            None
        } else {
            pending
                .statuses
                .get(&id)
                .copied()
                .unwrap_or(Ok(response_status))?
        };
        let Some(busy) = effective_status else {
            self.pending.clear();
            if self.targets.remove(&id).is_some() {
                change.removed.push(id);
            }
            return Ok(change);
        };
        if !self.targets.contains_key(&id) && self.targets.len() >= MAX_TARGETS {
            return Err(Failure::Capacity);
        }
        self.targets.insert(id, busy);
        Ok(change)
    }
}
fn rpc_id(value: &Value) -> Result<RpcId, Failure> {
    match value {
        Value::String(s) if !s.is_empty() && s.len() <= MAX_ID_BYTES => Ok(RpcId::Text(s.clone())),
        Value::Number(n) if n.is_i64() || n.is_u64() => Ok(RpcId::Number(n.to_string())),
        _ => Err(Failure::Malformed),
    }
}
fn thread_id(value: &Value) -> Result<Uuid, Failure> {
    value
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok())
        .filter(|id| !id.is_nil())
        .ok_or(Failure::Malformed)
}
fn status(value: Option<&Value>) -> Result<Option<bool>, Failure> {
    let Some(value) = value else {
        return Ok(Some(true));
    };
    match value.get("type").and_then(Value::as_str) {
        Some("idle") => Ok(Some(false)),
        Some("notLoaded" | "systemError") => Ok(None),
        Some("active") => {
            let flags = value["activeFlags"].as_array().ok_or(Failure::Malformed)?;
            if flags
                .iter()
                .any(|f| !matches!(f.as_str(), Some("waitingOnApproval" | "waitingOnUserInput")))
            {
                return Err(Failure::Malformed);
            }
            Ok(Some(true))
        }
        _ => Err(Failure::Malformed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    const A: &str = "11111111-1111-4111-8111-111111111111";
    fn request(t: &mut Tracker, id: Value) -> Result<Change, Failure> {
        t.observe(
            Direction::ToServer,
            &json!({"id":id,"method":"thread/resume","params":{"threadId":A}}),
        )
    }
    fn response(id: Value) -> Value {
        json!({"id":id,"result":{"thread":{"id":A,"source":"cli","status":{"type":"idle"}}}})
    }
    fn qualify(t: &mut Tracker) {
        request(t, json!(1)).unwrap();
        t.observe(Direction::ToTui, &response(json!(1))).unwrap();
    }
    #[test]
    fn only_correlated_response_grants_and_ids_keep_type() {
        let mut t = Tracker::new();
        request(&mut t, json!(1)).unwrap();
        t.observe(Direction::ToTui, &response(json!("1"))).unwrap();
        assert!(t.targets().is_empty());
        t.observe(Direction::ToServer, &response(json!(1))).unwrap();
        assert!(t.targets().is_empty());
        let mut server_request = response(json!(1));
        server_request["method"] = json!("approval");
        t.observe(Direction::ToTui, &server_request).unwrap();
        assert!(t.targets().is_empty());
        t.observe(Direction::ToTui, &response(json!(1))).unwrap();
        assert!(!t.targets()[0].busy);
    }
    #[test]
    fn late_responses_after_unsubscribe_or_close_cannot_regrant() {
        for method in ["thread/unsubscribe", "thread/closed"] {
            let mut t = Tracker::new();
            qualify(&mut t);
            request(&mut t, json!(2)).unwrap();
            let direction = if method == "thread/unsubscribe" {
                Direction::ToServer
            } else {
                Direction::ToTui
            };
            let change = t
                .observe(direction, &json!({"method":method,"params":{"threadId":A}}))
                .unwrap();
            assert_eq!(change.removed.len(), 1);
            t.observe(Direction::ToTui, &response(json!(2))).unwrap();
            assert!(t.targets().is_empty());
            assert_eq!(request(&mut t, json!(2)), Err(Failure::ReusedId));
        }
    }
    #[test]
    fn broadcast_unknown_responses_and_unrelated_status_never_grant() {
        let mut t = Tracker::new();
        for v in [
            json!({"method":"thread/started","params":{"thread":{"id":A,"source":"cli"}}}),
            response(json!(9)),
            json!({"method":"thread/status/changed","params":{"threadId":A,"status":{"type":"future"}}}),
        ] {
            t.observe(Direction::ToTui, &v).unwrap();
        }
        assert!(t.targets().is_empty());
    }
    #[test]
    fn unknown_sources_never_grant_missing_status_is_busy() {
        for source in [
            Value::Null,
            json!("unknown"),
            json!({"subAgent":{"thread_spawn":{}}}),
            json!({"custom":"cli"}),
        ] {
            let mut t = Tracker::new();
            request(&mut t, json!(1)).unwrap();
            let mut v = response(json!(1));
            v["result"]["thread"]["source"] = source;
            t.observe(Direction::ToTui, &v).unwrap();
            assert!(t.targets().is_empty());
        }
        let mut t = Tracker::new();
        request(&mut t, json!(1)).unwrap();
        let mut v = response(json!(1));
        v["result"]["thread"]
            .as_object_mut()
            .unwrap()
            .remove("status");
        t.observe(Direction::ToTui, &v).unwrap();
        assert!(t.targets()[0].busy);
        t.observe(Direction::ToTui,&json!({"method":"thread/status/changed","params":{"threadId":A,"status":{"type":"idle"}}})).unwrap();
        assert!(!t.targets()[0].busy);
    }
    #[test]
    fn malformed_target_status_poison_is_permanent() {
        let mut t = Tracker::new();
        qualify(&mut t);
        assert_eq!(t.observe(Direction::ToTui,&json!({"method":"thread/status/changed","params":{"threadId":A,"status":{"type":"future"}}})),Err(Failure::Malformed));
        assert!(t.targets().is_empty());
        assert_eq!(request(&mut t, json!(2)), Err(Failure::Poisoned));
    }
    #[test]
    fn invalid_ids_and_malformed_matched_results_fail_closed() {
        for id in [json!(null), json!(1.5), json!("x".repeat(MAX_ID_BYTES + 1))] {
            assert_eq!(request(&mut Tracker::new(), id), Err(Failure::Malformed));
        }
        for id in [
            "bad",
            "00000000-0000-0000-0000-000000000000",
            "22222222-2222-4222-8222-222222222222",
        ] {
            let mut t = Tracker::new();
            request(&mut t, json!(1)).unwrap();
            let mut v = response(json!(1));
            v["result"]["thread"]["id"] = json!(id);
            assert_eq!(t.observe(Direction::ToTui, &v), Err(Failure::Malformed));
            assert!(t.targets().is_empty());
        }
    }
    #[test]
    fn pending_and_tombstones_are_bounded_without_eviction() {
        let mut t = Tracker::new();
        for id in 0..MAX_PENDING {
            request(&mut t, json!(id)).unwrap();
        }
        assert_eq!(request(&mut t, json!(MAX_PENDING)), Err(Failure::Capacity));
        let mut t = Tracker::new();
        for id in 0..MAX_IDS {
            request(&mut t, json!(id)).unwrap();
            t.observe(
                Direction::ToTui,
                &json!({"id":id,"error":{"code":-1,"message":"synthetic"}}),
            )
            .unwrap();
        }
        assert_eq!(request(&mut t, json!(MAX_IDS)), Err(Failure::Capacity));
    }
    #[test]
    fn ordinary_rpc_cannot_reuse_subscription_id() {
        let mut t = Tracker::new();
        qualify(&mut t);
        assert_eq!(
            t.observe(
                Direction::ToServer,
                &json!({"id":1,"method":"thread/read","params":{"threadId":A}})
            ),
            Err(Failure::ReusedId)
        );
        assert!(t.targets().is_empty());
    }
}

#[cfg(test)]
mod additional_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn start_and_fork_grant_returned_uuid_and_candidate_capacity_is_bounded() {
        let mut tracker = Tracker::new();
        for n in 1..=MAX_TARGETS + 1 {
            let target = Uuid::from_u128(n as u128);
            let method = if n % 2 == 0 {
                "thread/fork"
            } else {
                "thread/start"
            };
            tracker
                .observe(
                    Direction::ToServer,
                    &json!({"id":n,"method":method,"params":{"threadId":Uuid::from_u128(999)}}),
                )
                .unwrap();
            let result = tracker.observe(Direction::ToTui, &json!({"id":n,"result":{"thread":{"id":target,"source":"cli","status":{"type":"idle"}}}}));
            if n <= MAX_TARGETS {
                result.unwrap();
            } else {
                assert_eq!(result, Err(Failure::Capacity));
            }
        }
        assert!(tracker.targets().is_empty());
    }
    #[test]
    fn not_loaded_cancels_pending_resume_before_it_qualifies() {
        let mut tracker = Tracker::new();
        let target = Uuid::from_u128(1);
        tracker
            .observe(
                Direction::ToServer,
                &json!({"id":1,"method":"thread/resume","params":{"threadId":target}}),
            )
            .unwrap();
        tracker.observe(Direction::ToTui,&json!({"method":"thread/status/changed","params":{"threadId":target,"status":{"type":"notLoaded"}}})).unwrap();
        tracker.observe(Direction::ToTui,&json!({"id":1,"result":{"thread":{"id":target,"source":"cli","status":{"type":"idle"}}}})).unwrap();
        assert!(tracker.targets().is_empty());
    }
}

#[cfg(test)]
mod ordering_regressions {
    use super::*;
    use serde_json::json;
    #[test]
    fn lifecycle_before_unknown_thread_ack_overrides_snapshot() {
        for method in ["thread/start", "thread/fork"] {
            for kind in ["notLoaded", "systemError", "active", "idle"] {
                let mut tracker = Tracker::new();
                let id = Uuid::from_u128(17);
                tracker
                    .observe(
                        Direction::ToServer,
                        &json!({"id":1,"method":method,"params":{"threadId":Uuid::from_u128(18)}}),
                    )
                    .unwrap();
                tracker.observe(Direction::ToTui, &json!({"method":"thread/status/changed","params":{"threadId":id,"status":{"type":kind,"activeFlags":[]}}})).unwrap();
                let stale = if kind == "idle" { "active" } else { "idle" };
                tracker.observe(Direction::ToTui, &json!({"id":1,"result":{"thread":{"id":id,"source":"cli","status":{"type":stale,"activeFlags":[]}}}})).unwrap();
                match kind {
                    "notLoaded" | "systemError" => {
                        assert!(tracker.targets().is_empty(), "{method}: {kind}")
                    }
                    "active" => assert!(tracker.targets()[0].busy),
                    _ => assert!(!tracker.targets()[0].busy),
                }
            }
        }
    }
}

#[cfg(test)]
mod pending_status_tests {
    use super::*;
    use serde_json::json;
    fn begin(t: &mut Tracker, id: u64) {
        t.observe(
            Direction::ToServer,
            &json!({"id":id,"method":"thread/start","params":{}}),
        )
        .unwrap();
    }
    fn notify(t: &mut Tracker, id: Uuid, kind: &str) {
        t.observe(Direction::ToTui,&json!({"method":"thread/status/changed","params":{"threadId":id,"status":{"type":kind,"activeFlags":[]}}})).unwrap();
    }
    fn finish(t: &mut Tracker, request: u64, thread: Uuid) -> Result<Change, Failure> {
        t.observe(Direction::ToTui,&json!({"id":request,"result":{"thread":{"id":thread,"source":"cli","status":{"type":"active","activeFlags":[]}}}}))
    }
    #[test]
    fn unrelated_terminal_does_not_cancel_and_new_request_has_fresh_statuses() {
        let mut t = Tracker::new();
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        begin(&mut t, 1);
        notify(&mut t, b, "notLoaded");
        notify(&mut t, a, "idle");
        begin(&mut t, 2);
        finish(&mut t, 1, a).unwrap();
        assert!(!t.targets()[0].busy);
        finish(&mut t, 2, a).unwrap();
        assert!(t.targets()[0].busy);
    }
    #[test]
    fn terminal_then_idle_does_not_rehabilitate_pending_request() {
        let mut t = Tracker::new();
        let a = Uuid::from_u128(1);
        begin(&mut t, 1);
        notify(&mut t, a, "notLoaded");
        notify(&mut t, a, "idle");
        finish(&mut t, 1, a).unwrap();
        assert!(t.targets().is_empty());
    }
    #[test]
    fn malformed_unrelated_status_is_deferred_until_correlated() {
        let mut t = Tracker::new();
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        begin(&mut t, 1);
        notify(&mut t, b, "future");
        finish(&mut t, 1, a).unwrap();
        begin(&mut t, 2);
        notify(&mut t, b, "future");
        assert_eq!(finish(&mut t, 2, b), Err(Failure::Malformed));
        assert!(t.targets().is_empty());
    }
    #[test]
    fn terminal_ack_is_not_overridden_by_earlier_idle() {
        for kind in ["notLoaded", "systemError"] {
            let mut t = Tracker::new();
            let a = Uuid::from_u128(1);
            begin(&mut t, 1);
            notify(&mut t, a, "idle");
            t.observe(
                Direction::ToTui,
                &json!({"id":1,"result":{"thread":{"id":a,"source":"cli","status":{"type":kind}}}}),
            )
            .unwrap();
            assert!(t.targets().is_empty());
        }
    }
    #[test]
    fn status_evidence_is_bounded() {
        let mut t = Tracker::new();
        begin(&mut t, 1);
        for n in 1..=MAX_TARGETS {
            notify(&mut t, Uuid::from_u128(n as u128), "idle");
        }
        assert_eq!(t.observe(Direction::ToTui,&json!({"method":"thread/status/changed","params":{"threadId":Uuid::from_u128(999),"status":{"type":"idle"}}})),Err(Failure::Capacity));
    }
}
