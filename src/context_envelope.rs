//! Shared recognition of observed, closed context envelopes. No I/O or attribution inference.

// These are closed, leading wrappers, not a keyword/XML heuristic. Ordinary
// prose, quoted examples, malformed or nested wrappers remain conversation.
pub(crate) fn closed_block<'a>(input: &'a str, tag: &str) -> Option<(&'a str, &'a str)> {
    let opening = format!("<{tag}>");
    let closing = format!("</{tag}>");
    let body = input.strip_prefix(&opening)?;
    let end = body.find(&closing)?;
    let inside = &body[..end];
    if inside.contains(&opening) {
        return None;
    }
    Some((inside, body[end + closing.len()..].trim_start()))
}

fn environment_body(body: &str) -> bool {
    let mut remaining = body.trim();
    let mut seen = std::collections::HashSet::new();
    while !remaining.is_empty() {
        let next = [
            "cwd",
            "shell",
            "current_date",
            "timezone",
            "filesystem",
            "subagents",
        ]
        .iter()
        .find_map(|tag| closed_block(remaining, tag).map(|(value, tail)| (*tag, value, tail)));
        let Some((tag, value, tail)) = next else {
            return false;
        };
        let valid = match tag {
            "filesystem" => filesystem_body(value),
            "subagents" => subagents_body(value),
            _ => plain_value(value),
        };
        if !seen.insert(tag) || !valid {
            return false;
        }
        remaining = tail;
    }
    seen.contains("cwd") && seen.contains("shell")
}

fn plain_value(value: &str) -> bool {
    !value.trim().is_empty() && !value.contains(['<', '>'])
}

// Recognize only the observed nested environment vocabulary. These small
// parsers reject unknown attributes, child elements, duplicate containers,
// unclosed tags and free text between elements; they are not general XML.
fn filesystem_body(body: &str) -> bool {
    let Some((roots, rest)) = closed_block(body.trim(), "workspace_roots") else {
        return false;
    };
    let mut roots = roots.trim();
    let mut root_count = 0;
    while !roots.is_empty() {
        let Some((path, rest)) = closed_block(roots, "root") else {
            return false;
        };
        if !plain_value(path) {
            return false;
        }
        root_count += 1;
        roots = rest;
    }
    let Some(profile) = rest.strip_prefix("<permission_profile type=\"managed\">") else {
        return false;
    };
    let Some(profile) = profile.trim_end().strip_suffix("</permission_profile>") else {
        return false;
    };
    let Some(entries) = profile
        .trim()
        .strip_prefix("<file_system type=\"restricted\">")
    else {
        return false;
    };
    let Some(mut entries) = entries
        .trim_end()
        .strip_suffix("</file_system>")
        .map(str::trim)
    else {
        return false;
    };
    while !entries.is_empty() {
        let Some(body) = entries
            .strip_prefix("<entry access=\"read\">")
            .or_else(|| entries.strip_prefix("<entry access=\"write\">"))
        else {
            return false;
        };
        let Some((inside, rest)) = body.split_once("</entry>") else {
            return false;
        };
        let value =
            closed_block(inside.trim(), "path").or_else(|| closed_block(inside.trim(), "special"));
        if !value.is_some_and(|(value, rest)| plain_value(value) && rest.is_empty()) {
            return false;
        }
        entries = rest.trim_start();
    }
    root_count > 0
}

fn subagents_body(body: &str) -> bool {
    let mut rest = body.trim();
    while !rest.is_empty() {
        let Some(agent) = rest.strip_prefix("<agent name=\"") else {
            return false;
        };
        let Some((name, tail)) = agent.split_once('"') else {
            return false;
        };
        if !name.starts_with('/') || !plain_value(name) {
            return false;
        }
        let Some(tail) = tail.trim_start().strip_prefix("/>") else {
            return false;
        };
        rest = tail.trim_start();
    }
    true
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EnvelopeKind {
    Memory,
    Environment,
    Notice,
}

pub(crate) struct Envelope<'a> {
    pub kind: EnvelopeKind,
    pub name: &'a str,
    pub text: &'a str,
    pub tail: &'a str,
}

pub(crate) fn leading(input: &str) -> Option<Envelope<'_>> {
    let remaining = input.trim_start();
    let (kind, name, tail) = if let Some(header) =
        remaining.strip_prefix("# AGENTS.md instructions for ")
    {
        let (path, body) = header.split_once('\n')?;
        if !path.trim().starts_with('/') {
            return None;
        }
        let (_, tail) = closed_block(body.trim_start(), "INSTRUCTIONS")?;
        (EnvelopeKind::Memory, path.trim(), tail)
    } else if let Some((body, tail)) = closed_block(remaining, "environment_context") {
        if !environment_body(body) {
            return None;
        }
        (EnvelopeKind::Environment, "environment_context", tail)
    } else if let Some((body, tail)) = closed_block(remaining, "external_codex_apps_open_page") {
        let value = serde_json::from_str::<serde_json::Value>(body).ok()?;
        if value != serde_json::json!({"page_id":null})
            || body
                .chars()
                .filter(|c| !c.is_ascii_whitespace())
                .collect::<String>()
                != "{\"page_id\":null}"
        {
            return None;
        }
        (EnvelopeKind::Notice, "external_codex_apps_open_page", tail)
    } else {
        return None;
    };
    Some(Envelope {
        kind,
        name,
        text: &input[..input.len() - tail.len()],
        tail,
    })
}

pub(crate) fn without_context(input: &str) -> &str {
    let mut remaining = input.trim();
    while let Some(envelope) = leading(remaining) {
        remaining = envelope.tail;
    }
    remaining
}
