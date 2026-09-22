//! Streaming text redaction. Raw JSON and credentials have no serialization path.

use std::sync::Arc;

use anyhow::{Result, bail};
use serde::Serialize;

mod fields;

const OMITTED: &str = "[已脱敏]";

#[derive(Clone, Default, Serialize)]
#[serde(transparent)]
pub struct SafeText(String);

impl SafeText {
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn bounded(&self, bytes: usize) -> Self {
        let mut end = self.0.len().min(bytes);
        while !self.0.is_char_boundary(end) {
            end -= 1;
        }
        Self(self.0[..end].to_owned())
    }
}

// No Debug: these are the actual values that must never enter a log or DTO.
pub struct RedactionPolicy {
    secrets: Vec<String>,
}

impl RedactionPolicy {
    pub fn new(mut secrets: Vec<String>) -> Result<Arc<Self>> {
        if secrets.len() > 32
            || secrets
                .iter()
                .any(|secret| secret.is_empty() || secret.len() > 8192)
        {
            bail!("invalid redaction secret budget");
        }
        secrets.sort_unstable_by_key(|value| std::cmp::Reverse(value.len()));
        secrets.dedup();
        Ok(Arc::new(Self { secrets }))
    }

    pub fn scrub(self: &Arc<Self>, text: &str) -> SafeText {
        let mut stream = TextRedactor::new(self.clone());
        let mut output = stream.push(text).0;
        output.push_str(stream.finish().as_str());
        SafeText(output)
    }

    pub fn scrub_tool(self: &Arc<Self>, text: &str) -> SafeText {
        let mut stream = TextRedactor::tool(self.clone());
        let mut output = stream.push(text).0;
        output.push_str(stream.finish().as_str());
        SafeText(output)
    }
}

#[derive(Clone, Copy)]
enum Mask {
    Credential,
    DataUri,
    Rest,
}

const PREFIXES: &[(&str, Mask)] = &[
    ("bearer ", Mask::Credential),
    ("basic ", Mask::Credential),
    ("sk-", Mask::Credential),
    ("ghp_", Mask::Credential),
    ("gho_", Mask::Credential),
    ("ghu_", Mask::Credential),
    ("ghs_", Mask::Credential),
    ("ghr_", Mask::Credential),
    ("github_pat_", Mask::Credential),
    ("xoxb-", Mask::Credential),
    ("xoxp-", Mask::Credential),
    ("akia", Mask::Credential),
    ("eyj", Mask::Credential),
    ("access_token=", Mask::Credential),
    ("refresh_token=", Mask::Credential),
    ("api_key=", Mask::Credential),
    ("token=", Mask::Credential),
    ("signature=", Mask::Credential),
    ("data:image/", Mask::DataUri),
    ("data:audio/", Mask::DataUri),
    ("-----begin ", Mask::Rest),
];

pub(crate) struct TextRedactor {
    policy: Arc<RedactionPolicy>,
    pending: String,
    masked: Option<Mask>,
    tool_fields: bool,
}

impl TextRedactor {
    pub fn new(policy: Arc<RedactionPolicy>) -> Self {
        Self {
            policy,
            pending: String::new(),
            masked: None,
            tool_fields: false,
        }
    }

    pub fn tool(policy: Arc<RedactionPolicy>) -> Self {
        Self {
            tool_fields: true,
            ..Self::new(policy)
        }
    }

    pub fn pending_bytes(&self) -> usize {
        self.pending.capacity()
    }

    pub fn push(&mut self, text: &str) -> SafeText {
        let mut output = String::new();
        for character in text.chars() {
            if let Some(mask) = self.masked {
                let keep_masking = match mask {
                    Mask::Rest => true,
                    Mask::Credential => {
                        character.is_ascii_alphanumeric() || "_-./+=%".contains(character)
                    }
                    Mask::DataUri => !character.is_whitespace() && !"\"'<>)]}".contains(character),
                };
                if keep_masking {
                    continue;
                }
                self.masked = None;
            }
            self.pending.push(character);
            self.drain(&mut output, false);
        }
        SafeText(output)
    }

    fn drain(&mut self, output: &mut String, finishing: bool) {
        while !self.pending.is_empty() {
            // Hold incomplete field names, including ASCII escapes, across
            // chunks. A recognized field omits the remaining preview because
            // its value can itself be an unfinished object or quoted string.
            if self.tool_fields {
                match fields::classify(&self.pending) {
                    fields::FieldMatch::Sensitive => {
                        self.pending.clear();
                        output.push_str(OMITTED);
                        self.masked = Some(Mask::Rest);
                        return;
                    }
                    fields::FieldMatch::Prefix if !finishing => return,
                    _ => {}
                }
            }
            let known_prefix = self.policy.secrets.iter().any(|secret| {
                secret.starts_with(&self.pending) && secret.len() > self.pending.len()
            });
            let generic_prefix = PREFIXES.iter().any(|(prefix, _)| {
                self.pending.len() < prefix.len()
                    && prefix.as_bytes()[..self.pending.len()]
                        .eq_ignore_ascii_case(self.pending.as_bytes())
            });
            if !finishing && (known_prefix || generic_prefix) {
                return;
            }
            if let Some(secret) = self
                .policy
                .secrets
                .iter()
                .find(|secret| self.pending.starts_with(secret.as_str()))
            {
                let size = secret.len();
                self.pending.drain(..size);
                output.push_str(OMITTED);
                continue;
            }
            if let Some((prefix, mask)) = PREFIXES.iter().find(|(prefix, _)| {
                self.pending
                    .as_bytes()
                    .get(..prefix.len())
                    .is_some_and(|start| start.eq_ignore_ascii_case(prefix.as_bytes()))
            }) {
                self.pending.drain(..prefix.len());
                output.push_str(OMITTED);
                self.masked = Some(*mask);
                // A generic prefix can have been held for a longer configured
                // secret. Reprocess its suffix under the newly active mask.
                let tail = std::mem::take(&mut self.pending);
                output.push_str(self.push(&tail).as_str());
                continue;
            }
            // An incomplete generic marker contains no credential payload.
            // A one-letter overlap with a configured secret is ordinary text;
            // retain privacy for a meaningful (8+ byte) configured prefix when
            // a stream ends or is interrupted. Full secrets of any length are
            // always handled above, including at every chunk boundary.
            if finishing && known_prefix && self.pending.len() >= 8 {
                output.push_str(OMITTED);
                self.pending.clear();
                return;
            }
            let character = self.pending.chars().next().unwrap();
            self.pending.drain(..character.len_utf8());
            output.push(character);
        }
    }

    pub fn finish(&mut self) -> SafeText {
        let mut output = String::new();
        self.drain(&mut output, true);
        self.masked = None;
        SafeText(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_field_names_are_protected_when_unquoted_escaped_or_split() {
        let policy = RedactionPolicy::new(vec![]).unwrap();
        for input in [
            r#"{password : 'synthetic-hidden-value'}"#,
            "API_KEY = synthetic-hidden-value",
            r#"{\"t\u006fken\":\"synthetic-hidden-value\"}"#,
            r#"{\"\u0061pi_key\":\"synthetic-hidden-value\"}"#,
            r#"{\"coo\x6bie\":\"synthetic-hidden-value\"}"#,
            r#"{\"headers\": \"{\\\"authorization\\\":\\\"synthetic-hidden-value\\\"}\"}"#,
            "{encrypted_content: 'synthetic-hidden-value'}",
            "{env: {CUSTOM_PRIVATE: 'synthetic-hidden-value'}}",
            "{accessToken: 'synthetic-hidden-value'}",
        ] {
            for split in 0..=input.len() {
                let mut stream = TextRedactor::tool(policy.clone());
                let mut result = stream.push(&input[..split]).0;
                assert!(!result.contains("synthetic-hidden-value"));
                result.push_str(stream.push(&input[split..]).as_str());
                result.push_str(stream.finish().as_str());
                assert!(
                    !result.contains("synthetic-hidden-value"),
                    "field marker was not redacted"
                );
                assert!(result.contains(OMITTED));
                assert_eq!(policy.scrub_tool(input).as_str(), result);
            }
        }
        for input in [
            "tokens used: 42",
            "tokenize(value)",
            "environment",
            "api_key_count=2",
            "{cmd: 'printf 中文'}",
            r#"{\"cmd\":\"printf hello\\n\"}"#,
        ] {
            assert_eq!(policy.scrub_tool(input).as_str(), input);
        }
    }

    #[test]
    fn ordinary_english_and_identifiers_keep_their_trailing_prefix_characters() {
        let policy = RedactionPolicy::new(vec!["fixture-secret-value".into()]).unwrap();
        for input in [
            "resp_one",
            "message",
            "basic",
            "token",
            "data",
            "brief",
            "中英 mixed text",
            "ey",
        ] {
            let mut stream = TextRedactor::new(policy.clone());
            let mut result = String::new();
            for character in input.chars() {
                result.push_str(stream.push(&character.to_string()).as_str());
            }
            result.push_str(stream.finish().as_str());
            assert_eq!(result, input);
            assert_eq!(policy.scrub(input).as_str(), input);
        }
        assert_eq!(policy.scrub("fixture-secret-").as_str(), OMITTED);
        assert_eq!(policy.scrub("sk-fixture").as_str(), OMITTED);
        assert_eq!(
            RedactionPolicy::new(vec!["xy".into()])
                .unwrap()
                .scrub("xy")
                .as_str(),
            OMITTED
        );
    }

    #[test]
    fn known_credentials_are_omitted_across_every_increment_boundary() {
        let policy = RedactionPolicy::new(vec![
            "fixture-sensitive-value".into(),
            "fixture-sensitive".into(),
        ])
        .unwrap();
        let input = "中文 fixture-sensitive-value 然后继续";
        for split in input
            .char_indices()
            .map(|(offset, _)| offset)
            .chain([input.len()])
        {
            let mut stream = TextRedactor::new(policy.clone());
            let mut result = stream.push(&input[..split]).0;
            result.push_str(stream.push(&input[split..]).as_str());
            result.push_str(stream.finish().as_str());
            assert_eq!(result, "中文 [已脱敏] 然后继续");
        }
    }

    #[test]
    fn common_auth_media_and_private_key_patterns_never_leak_their_suffixes() {
        let policy = RedactionPolicy::new(vec![]).unwrap();
        for (input, expected) in [
            (
                "header: Bearer abc.def+ghi=\n正常",
                "header: [已脱敏]\n正常",
            ),
            ("x sk-proj-fixture y", "x [已脱敏] y"),
            (
                "![图](data:image/png;base64,AAAA////==) 后续",
                "![图]([已脱敏]) 后续",
            ),
            ("?access_token=abc&ok=1", "?[已脱敏]&ok=1"),
            (
                "-----BEGIN PRIVATE KEY-----\nABCD\n-----END PRIVATE KEY-----",
                "[已脱敏]",
            ),
        ] {
            let mut stream = TextRedactor::new(policy.clone());
            let mut result = String::new();
            for character in input.chars() {
                result.push_str(stream.push(&character.to_string()).as_str());
            }
            result.push_str(stream.finish().as_str());
            assert_eq!(result, expected);
        }
    }

    #[test]
    fn ordinary_chinese_is_immediate_and_partial_credentials_stay_private_on_abort() {
        let policy = RedactionPolicy::new(vec!["a-long-fixture-credential".into()]).unwrap();
        let mut stream = TextRedactor::new(policy.clone());
        assert_eq!(stream.push("中文正文").as_str(), "中文正文");
        assert!(stream.push("a-long-fixture-").is_empty());
        assert!(stream.pending_bytes() < 8192);
        assert_eq!(stream.finish().as_str(), OMITTED);
        assert_eq!(
            policy.scrub("HTML <b>仅为文字</b>").as_str(),
            "HTML <b>仅为文字</b>"
        );
        assert!(RedactionPolicy::new(vec!["x".repeat(8193)]).is_err());
    }
}
