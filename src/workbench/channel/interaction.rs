//! Bounded native interaction projection. No user filesystem reads, raw payload
//! serialization, generic RPC results, or cancellation authority live here.
use serde_json::{Map, Value, json};
use std::collections::HashSet;
use uuid::Uuid;

const ID_LIMIT: usize = 128;
const TEXT_LIMIT: usize = 4096;
const FILE_LIMIT: usize = 8;

// Private protocol content deliberately has no Debug or Serialize implementation.
#[derive(Clone, PartialEq, Eq, Hash)]
pub enum RpcId {
    Number(i64),
    String(String),
}

impl RpcId {
    pub fn parse(value: &Value) -> Result<Self, Invalid> {
        if let Some(number) = value.as_i64() {
            Ok(Self::Number(number))
        } else {
            Ok(Self::String(string(value, ID_LIMIT, false)?.to_owned()))
        }
    }

    pub fn to_value(&self) -> Value {
        match self {
            Self::Number(number) => json!(number),
            Self::String(string) => json!(string),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Invalid {
    Shape,
    Limit,
    Control,
    Duplicate,
    Scope,
    Action,
}

#[derive(Clone)]
pub struct Request {
    pub id: RpcId,
    pub thread: Uuid,
    pub turn: String,
    pub item: String,
    pub kind: Kind,
}

#[derive(Clone)]
pub enum Kind {
    Command(Approval),
    File(Approval),
    Questions(Vec<Question>),
}

#[derive(Clone)]
pub struct Approval {
    text: String,
    accept: bool,
    decline: bool,
}

#[derive(Clone)]
pub struct Question {
    id: String,
    header: String,
    question: String,
    options: Vec<OptionText>,
}

#[derive(Clone)]
struct OptionText {
    label: String,
    description: String,
}

pub enum Decode {
    Ignored,
    Unsupported,
    Supported(Request),
}

#[derive(Clone, PartialEq, Eq)]
pub struct FileScope {
    pub thread: Uuid,
    pub turn: String,
    pub item: String,
    changes: Vec<FileChange>,
}

#[derive(Clone, PartialEq, Eq)]
struct FileChange {
    path: String,
    kind: FileKind,
}

#[derive(Clone, PartialEq, Eq)]
enum FileKind {
    Add,
    Delete,
    Update(Option<String>),
}

impl FileScope {
    /// Only a complete started item qualifies; patchUpdated is a preview.
    /// Diff is checked for its wire type and immediately discarded.
    pub fn from_started(value: &Value) -> Result<Option<Self>, Invalid> {
        if value["method"] != "item/started" {
            return Ok(None);
        }
        let params = object(&value["params"])?;
        let item = object(params.get("item").ok_or(Invalid::Shape)?)?;
        if item.get("type").and_then(Value::as_str) != Some("fileChange") {
            return Ok(None);
        }
        known(params, &["item", "threadId", "turnId", "startedAtMs"])?;
        known(item, &["type", "id", "changes", "status"])?;
        optional_integer(params, "startedAtMs")?;
        if item.get("status").and_then(Value::as_str) != Some("inProgress") {
            return Err(Invalid::Scope);
        }
        let thread = thread_id(params)?;
        let turn = required(params, "turnId", ID_LIMIT, false)?.to_owned();
        let id = required(item, "id", ID_LIMIT, false)?.to_owned();
        let changes = item
            .get("changes")
            .and_then(Value::as_array)
            .ok_or(Invalid::Shape)?;
        if changes.is_empty() || changes.len() > FILE_LIMIT {
            return Err(Invalid::Limit);
        }
        let mut paths = HashSet::new();
        let mut parsed = Vec::with_capacity(changes.len());
        for change in changes {
            let change = object(change)?;
            known(change, &["path", "kind", "diff"])?;
            if !change.get("diff").is_some_and(Value::is_string) {
                return Err(Invalid::Shape);
            }
            let file_path = path(required(change, "path", 512, false)?)?;
            if !paths.insert(file_path.clone()) {
                return Err(Invalid::Duplicate);
            }
            let kind = object(change.get("kind").ok_or(Invalid::Shape)?)?;
            let kind = match kind.get("type").and_then(Value::as_str) {
                Some("add") => {
                    known(kind, &["type"])?;
                    FileKind::Add
                }
                Some("delete") => {
                    known(kind, &["type"])?;
                    FileKind::Delete
                }
                Some("update") => {
                    known(kind, &["type", "move_path"])?;
                    let target = optional(kind, "move_path", 512, false)?
                        .map(path)
                        .transpose()?;
                    if let Some(target) = &target
                        && !paths.insert(target.clone())
                    {
                        return Err(Invalid::Duplicate);
                    }
                    FileKind::Update(target)
                }
                _ => return Err(Invalid::Shape),
            };
            parsed.push(FileChange {
                path: file_path,
                kind,
            });
        }
        let scope = Self {
            thread,
            turn,
            item: id,
            changes: parsed,
        };
        check_text(&scope.text())?;
        Ok(Some(scope))
    }

    fn text(&self) -> String {
        let mut text = String::from("File approval — these changes only:\n");
        for change in &self.changes {
            match &change.kind {
                FileKind::Add => text.push_str(&format!("Add: {}\n", change.path)),
                FileKind::Delete => text.push_str(&format!("Delete: {}\n", change.path)),
                FileKind::Update(None) => text.push_str(&format!("Update: {}\n", change.path)),
                FileKind::Update(Some(target)) => {
                    text.push_str(&format!("Update and move: {} → {target}\n", change.path))
                }
            }
        }
        text
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Accept,
    Decline,
    Choose { question: usize, option: usize },
}

pub enum NativeResponse {
    Approval(Decision),
    Answers(Answers),
}

pub enum Decision {
    Accept,
    Decline,
}

pub struct Answers(Vec<(String, String)>);

impl NativeResponse {
    pub fn envelope(&self, id: &RpcId) -> Value {
        let result = match self {
            Self::Approval(decision) => {
                json!({"decision": match decision { Decision::Accept => "accept", Decision::Decline => "decline" }})
            }
            Self::Answers(answers) => {
                let answers: Map<String, Value> = answers
                    .0
                    .iter()
                    .map(|(id, label)| (id.clone(), json!({"answers": [label]})))
                    .collect();
                json!({"answers": answers})
            }
        };
        json!({"id": id.to_value(), "result": result})
    }
}

impl Request {
    pub fn text(&self) -> String {
        match &self.kind {
            Kind::Command(approval) | Kind::File(approval) => approval.text.clone(),
            Kind::Questions(questions) => {
                let mut text =
                    String::from("Please answer every question using the numbered options.\n");
                for (index, question) in questions.iter().enumerate() {
                    text.push_str(&format!(
                        "\n{}. {}\n{}\n",
                        index + 1,
                        question.header,
                        question.question
                    ));
                    for (option, content) in question.options.iter().enumerate() {
                        text.push_str(&format!(
                            "{}.{} {} — {}\n",
                            index + 1,
                            option + 1,
                            content.label,
                            content.description
                        ));
                    }
                }
                text
            }
        }
    }

    pub fn actions(&self) -> Vec<Action> {
        match &self.kind {
            Kind::Command(approval) | Kind::File(approval) => [
                approval.accept.then_some(Action::Accept),
                approval.decline.then_some(Action::Decline),
            ]
            .into_iter()
            .flatten()
            .collect(),
            Kind::Questions(questions) => questions
                .iter()
                .enumerate()
                .flat_map(|(question, content)| {
                    (0..content.options.len())
                        .map(move |option| Action::Choose { question, option })
                })
                .collect(),
        }
    }

    /// `selected` contains earlier choices, excluding the current action. Each
    /// question may be selected only once; no partial answer reaches native RPC.
    pub fn response(
        &self,
        action: &Action,
        selected: &[(usize, usize)],
    ) -> Result<Option<NativeResponse>, Invalid> {
        if !self.actions().contains(action) {
            return Err(Invalid::Action);
        }
        match (&self.kind, action) {
            (Kind::Command(_) | Kind::File(_), Action::Accept | Action::Decline)
                if selected.is_empty() =>
            {
                Ok(Some(NativeResponse::Approval(
                    if *action == Action::Accept {
                        Decision::Accept
                    } else {
                        Decision::Decline
                    },
                )))
            }
            (Kind::Questions(questions), Action::Choose { question, option }) => {
                if selected.len() >= questions.len() {
                    return Err(Invalid::Action);
                }
                let mut choices = vec![None; questions.len()];
                for &(question, option) in selected
                    .iter()
                    .chain(std::iter::once(&(*question, *option)))
                {
                    let content = questions.get(question).ok_or(Invalid::Action)?;
                    if option >= content.options.len()
                        || choices[question].replace(option).is_some()
                    {
                        return Err(Invalid::Action);
                    }
                }
                if choices.iter().any(Option::is_none) {
                    return Ok(None);
                }
                let answers = questions
                    .iter()
                    .zip(choices)
                    .map(|(question, choice)| {
                        (
                            question.id.clone(),
                            question.options[choice.expect("all choices checked")]
                                .label
                                .clone(),
                        )
                    })
                    .collect();
                Ok(Some(NativeResponse::Answers(Answers(answers))))
            }
            _ => Err(Invalid::Action),
        }
    }
}

pub fn decode_request(value: &Value, file_scope: Option<&FileScope>) -> Result<Decode, Invalid> {
    let Some(method) = value.get("method").and_then(Value::as_str) else {
        return Ok(Decode::Ignored);
    };
    if value.get("id").is_none_or(|id| id.is_null()) {
        return Ok(Decode::Ignored);
    }
    if !matches!(
        method,
        "item/commandExecution/requestApproval"
            | "item/fileChange/requestApproval"
            | "item/tool/requestUserInput"
    ) {
        return Ok(Decode::Unsupported);
    }
    let envelope = object(value)?;
    known(envelope, &["id", "method", "params", "jsonrpc"])?;
    if envelope.get("jsonrpc").is_some_and(|value| value != "2.0") {
        return Err(Invalid::Shape);
    }
    let id = RpcId::parse(&value["id"])?;
    let params = object(&value["params"])?;
    let thread = thread_id(params)?;
    let turn = required(params, "turnId", ID_LIMIT, false)?.to_owned();
    let item = required(params, "itemId", ID_LIMIT, false)?.to_owned();
    let kind = match method {
        "item/commandExecution/requestApproval" => {
            known(
                params,
                &[
                    "threadId",
                    "turnId",
                    "itemId",
                    "kind",
                    "startedAtMs",
                    "approvalId",
                    "environmentId",
                    "reason",
                    "networkApprovalContext",
                    "command",
                    "cwd",
                    "commandActions",
                    "additionalPermissions",
                    "proposedExecpolicyAmendment",
                    "proposedNetworkPolicyAmendments",
                    "availableDecisions",
                ],
            )?;
            optional_integer(params, "startedAtMs")?;
            optional(params, "approvalId", ID_LIMIT, false)?;
            if params.get("kind").is_some_and(|value| value != "command")
                || (nonnull(params, "environmentId") && params["environmentId"] != "local")
                || ["networkApprovalContext", "additionalPermissions"]
                    .iter()
                    .any(|key| nonnull(params, key))
            {
                return Ok(Decode::Unsupported);
            }
            if nonnull(params, "proposedExecpolicyAmendment") {
                exec_amendment(&params["proposedExecpolicyAmendment"])?;
            }
            if nonnull(params, "proposedNetworkPolicyAmendments") {
                let amendments = params["proposedNetworkPolicyAmendments"]
                    .as_array()
                    .ok_or(Invalid::Shape)?;
                if amendments.len() > 8 {
                    return Err(Invalid::Limit);
                }
                for amendment in amendments {
                    network_amendment(amendment)?;
                }
            }
            let (mut accept, mut decline) = (true, true);
            if nonnull(params, "availableDecisions") {
                let decisions = params["availableDecisions"]
                    .as_array()
                    .ok_or(Invalid::Shape)?;
                let mut seen = HashSet::new();
                if decisions.len() > 6 {
                    return Err(Invalid::Limit);
                }
                for decision in decisions {
                    let name = if let Some(decision) = decision.as_str() {
                        if !matches!(
                            decision,
                            "accept" | "decline" | "acceptForSession" | "cancel"
                        ) {
                            return Ok(Decode::Unsupported);
                        }
                        decision
                    } else {
                        let decision = object(decision)?;
                        if decision.len() != 1 {
                            return Err(Invalid::Shape);
                        }
                        if let Some(amendment) = decision.get("acceptWithExecpolicyAmendment") {
                            let amendment = object(amendment)?;
                            known(amendment, &["execpolicy_amendment"])?;
                            exec_amendment(
                                amendment
                                    .get("execpolicy_amendment")
                                    .ok_or(Invalid::Shape)?,
                            )?;
                            "acceptWithExecpolicyAmendment"
                        } else if let Some(amendment) = decision.get("applyNetworkPolicyAmendment")
                        {
                            let amendment = object(amendment)?;
                            known(amendment, &["network_policy_amendment"])?;
                            network_amendment(
                                amendment
                                    .get("network_policy_amendment")
                                    .ok_or(Invalid::Shape)?,
                            )?;
                            "applyNetworkPolicyAmendment"
                        } else {
                            return Ok(Decode::Unsupported);
                        }
                    };
                    if !seen.insert(name) {
                        return Err(Invalid::Duplicate);
                    }
                }
                accept = seen.contains("accept");
                decline = seen.contains("decline");
                if !accept && !decline {
                    return Ok(Decode::Unsupported);
                }
            }
            let command = required(params, "command", 2400, true)?;
            let reason = optional(params, "reason", 512, true)?;
            let mut text = format!("Command approval — applies to this command only:\n{command}\n");
            if let Some(reason) = reason {
                text.push_str(&format!("Reason: {reason}\n"));
            }
            Kind::Command(Approval {
                text,
                accept,
                decline,
            })
        }
        "item/fileChange/requestApproval" => {
            known(
                params,
                &[
                    "threadId",
                    "turnId",
                    "itemId",
                    "startedAtMs",
                    "reason",
                    "grantRoot",
                ],
            )?;
            optional_integer(params, "startedAtMs")?;
            if nonnull(params, "grantRoot") {
                return Ok(Decode::Unsupported);
            }
            let Some(scope) = file_scope else {
                return Ok(Decode::Unsupported);
            };
            if scope.thread != thread || scope.turn != turn || scope.item != item {
                return Err(Invalid::Scope);
            }
            let mut text = scope.text();
            if let Some(reason) = optional(params, "reason", 512, true)? {
                text.push_str(&format!("Reason: {reason}\n"));
            }
            Kind::File(Approval {
                text,
                accept: true,
                decline: true,
            })
        }
        _ => {
            known(
                params,
                &[
                    "threadId",
                    "turnId",
                    "itemId",
                    "questions",
                    "isBlocking",
                    "autoResolutionMs",
                ],
            )?;
            if !boolean(params, "isBlocking", true)? || nonnull(params, "autoResolutionMs") {
                return Ok(Decode::Unsupported);
            }
            let questions = params
                .get("questions")
                .and_then(Value::as_array)
                .ok_or(Invalid::Shape)?;
            if questions.is_empty() || questions.len() > 3 {
                return Err(Invalid::Limit);
            }
            let mut ids = HashSet::new();
            let mut parsed = Vec::with_capacity(questions.len());
            for question in questions {
                let question = object(question)?;
                known(
                    question,
                    &["id", "header", "question", "isOther", "isSecret", "options"],
                )?;
                if boolean(question, "isSecret", false)? || boolean(question, "isOther", false)? {
                    return Ok(Decode::Unsupported);
                }
                let id = required(question, "id", ID_LIMIT, false)?.to_owned();
                if !ids.insert(id.clone()) {
                    return Err(Invalid::Duplicate);
                }
                let header = required(question, "header", 64, true)?.to_owned();
                let text = required(question, "question", 512, true)?.to_owned();
                if !nonnull(question, "options") {
                    return Ok(Decode::Unsupported);
                }
                let options = question["options"].as_array().ok_or(Invalid::Shape)?;
                if options.is_empty() {
                    return Ok(Decode::Unsupported);
                }
                if !(2..=4).contains(&options.len()) {
                    return Err(Invalid::Limit);
                }
                let mut labels = HashSet::new();
                let mut parsed_options = Vec::with_capacity(options.len());
                for option in options {
                    let option = object(option)?;
                    known(option, &["label", "description"])?;
                    let label = required(option, "label", 128, false)?.to_owned();
                    if !labels.insert(label.clone()) {
                        return Err(Invalid::Duplicate);
                    }
                    let description = required(option, "description", 160, true)?.to_owned();
                    parsed_options.push(OptionText { label, description });
                }
                parsed.push(Question {
                    id,
                    header,
                    question: text,
                    options: parsed_options,
                });
            }
            Kind::Questions(parsed)
        }
    };
    let request = Request {
        id,
        thread,
        turn,
        item,
        kind,
    };
    check_text(&request.text())?;
    Ok(Decode::Supported(request))
}

fn exec_amendment(value: &Value) -> Result<(), Invalid> {
    let args = value.as_array().ok_or(Invalid::Shape)?;
    if args.is_empty() || args.len() > 32 {
        return Err(Invalid::Limit);
    }
    for arg in args {
        string(arg, 256, false)?;
    }
    Ok(())
}
fn network_amendment(value: &Value) -> Result<(), Invalid> {
    let value = object(value)?;
    known(value, &["host", "action"])?;
    required(value, "host", 253, false)?;
    if !matches!(
        value.get("action").and_then(Value::as_str),
        Some("allow" | "deny")
    ) {
        return Err(Invalid::Shape);
    }
    Ok(())
}

fn object(value: &Value) -> Result<&Map<String, Value>, Invalid> {
    value.as_object().ok_or(Invalid::Shape)
}
fn known(value: &Map<String, Value>, fields: &[&str]) -> Result<(), Invalid> {
    if value.keys().all(|key| fields.contains(&key.as_str())) {
        Ok(())
    } else {
        Err(Invalid::Shape)
    }
}
fn string(value: &Value, limit: usize, multiline: bool) -> Result<&str, Invalid> {
    let value = value.as_str().ok_or(Invalid::Shape)?;
    if value.is_empty() || value.encode_utf16().count() > limit {
        return Err(Invalid::Limit);
    }
    if value.chars().any(|c| (c.is_control() && !(multiline && matches!(c, '\n' | '\t'))) || matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')) { return Err(Invalid::Control); }
    Ok(value)
}
fn required<'a>(
    value: &'a Map<String, Value>,
    key: &str,
    limit: usize,
    multiline: bool,
) -> Result<&'a str, Invalid> {
    string(value.get(key).ok_or(Invalid::Shape)?, limit, multiline)
}
fn optional<'a>(
    value: &'a Map<String, Value>,
    key: &str,
    limit: usize,
    multiline: bool,
) -> Result<Option<&'a str>, Invalid> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => string(value, limit, multiline).map(Some),
    }
}
fn boolean(value: &Map<String, Value>, key: &str, default: bool) -> Result<bool, Invalid> {
    match value.get(key) {
        None => Ok(default),
        Some(value) => value.as_bool().ok_or(Invalid::Shape),
    }
}
fn nonnull(value: &Map<String, Value>, key: &str) -> bool {
    value.get(key).is_some_and(|value| !value.is_null())
}
fn optional_integer(value: &Map<String, Value>, key: &str) -> Result<(), Invalid> {
    if value.get(key).is_some_and(|value| value.as_i64().is_none()) {
        Err(Invalid::Shape)
    } else {
        Ok(())
    }
}
fn thread_id(params: &Map<String, Value>) -> Result<Uuid, Invalid> {
    Uuid::parse_str(required(params, "threadId", 36, false)?).map_err(|_| Invalid::Shape)
}
fn check_text(value: &str) -> Result<(), Invalid> {
    if value.encode_utf16().count() > TEXT_LIMIT {
        Err(Invalid::Limit)
    } else {
        Ok(())
    }
}
fn path(value: &str) -> Result<String, Invalid> {
    // Reject lexical aliases without opening or resolving private user paths.
    if value
        .split('/')
        .enumerate()
        .any(|(index, part)| matches!(part, "." | "..") || (index > 0 && part.is_empty()))
    {
        return Err(Invalid::Scope);
    }
    Ok(value.to_owned())
}

#[cfg(test)]
mod tests;
