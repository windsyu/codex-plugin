use serde_json::{Map, Value, json};

const SECRET_KEYS: &[&str] = &[
    "authorization",
    "proxyauthorization",
    "cookie",
    "setcookie",
    "apikey",
    "accesstoken",
    "refreshtoken",
    "idtoken",
    "oauthtoken",
    "bearertoken",
    "password",
    "passphrase",
    "secret",
    "clientsecret",
    "clientassertion",
    "credential",
    "credentials",
    "privatekey",
    "sessionkey",
    "signature",
    "xamzsignature",
    "xgoogsignature",
];

const SENSITIVE_QUERY_KEYS: &[&str] = &[
    "token",
    "accesstoken",
    "refreshtoken",
    "idtoken",
    "apikey",
    "key",
    "signature",
    "sig",
    "xamzsignature",
    "xgoogsignature",
    "clientsecret",
    "code",
    "credential",
    "sas",
];

pub fn redact(value: &Value, fingerprint_key: &[u8; 32]) -> (Value, Value) {
    let mut output = value.clone();
    let mut changes = Vec::new();
    visit(&mut output, "", fingerprint_key, &mut changes, None);
    let pointers = changes
        .iter()
        .filter_map(|change: &Value| change.get("pointer").cloned())
        .collect::<Vec<_>>();
    (
        output,
        json!({"ruleVersion":"known-secrets-v2","pointers":pointers,"changes":changes}),
    )
}

fn visit(
    value: &mut Value,
    pointer: &str,
    fingerprint_key: &[u8; 32],
    changes: &mut Vec<Value>,
    media_type_hint: Option<&str>,
) {
    match value {
        Value::Object(map) => visit_object(map, pointer, fingerprint_key, changes),
        Value::Array(values) => {
            for (index, child) in values.iter_mut().enumerate() {
                visit(
                    child,
                    &format!("{pointer}/{index}"),
                    fingerprint_key,
                    changes,
                    media_type_hint,
                );
            }
        }
        Value::String(text) => {
            if credential_kind(text).is_some() {
                replace(
                    value,
                    pointer,
                    "credential",
                    "credential-pattern-v2",
                    changes,
                );
            } else if let Some(redacted_url) = redact_url(text) {
                *value = Value::String(redacted_url);
                record(changes, pointer, "url_query", "sensitive-url-query-v2");
            } else if let Some((media_type, estimated_bytes)) = media_payload(text, media_type_hint)
            {
                let fingerprint = blake3::keyed_hash(fingerprint_key, text.as_bytes())
                    .to_hex()
                    .to_string();
                *value = json!({
                    "$redacted":true,
                    "kind":"media_payload",
                    "rule":"base64-media-omit-v2",
                    "mediaType":media_type,
                    "estimatedBytes":estimated_bytes,
                    "fingerprint":fingerprint
                });
                record(changes, pointer, "media_payload", "base64-media-omit-v2");
            }
        }
        _ => {}
    }
}

fn visit_object(
    map: &mut Map<String, Value>,
    pointer: &str,
    fingerprint_key: &[u8; 32],
    changes: &mut Vec<Value>,
) {
    let media_type = map
        .iter()
        .find(|(key, _)| {
            matches!(
                normalize_name(key).as_str(),
                "mediatype" | "mimetype" | "contenttype"
            )
        })
        .and_then(|(_, value)| value.as_str())
        .map(str::to_string);
    for (key, child) in map.iter_mut() {
        let child_pointer = format!("{pointer}/{}", escape_pointer(key));
        let normalized = normalize_name(key);
        if SECRET_KEYS.iter().any(|candidate| normalized == *candidate)
            || (normalized == "token" && !child.is_null())
        {
            replace(
                child,
                &child_pointer,
                "secret",
                if normalized.contains("oauth") || normalized.ends_with("token") {
                    "oauth-token-v2"
                } else {
                    "known-secret-key-v2"
                },
                changes,
            );
        } else if matches!(
            normalized.as_str(),
            "env" | "environment" | "environmentvariables"
        ) {
            if let Value::Object(env) = child {
                for (env_key, env_value) in env.iter_mut() {
                    let env_pointer = format!("{child_pointer}/{}", escape_pointer(env_key));
                    replace(
                        env_value,
                        &env_pointer,
                        "environment_value",
                        "environment-default-deny-v2",
                        changes,
                    );
                }
            }
        } else if let Value::String(text) = child
            && is_media_field(&normalized, media_type.as_deref())
            && let Some((kind, estimated_bytes)) = media_payload(text, media_type.as_deref())
        {
            let fingerprint = blake3::keyed_hash(fingerprint_key, text.as_bytes())
                .to_hex()
                .to_string();
            *child = json!({
                "$redacted":true,"kind":"media_payload","rule":"base64-media-omit-v2",
                "mediaType":kind,"estimatedBytes":estimated_bytes,"fingerprint":fingerprint
            });
            record(
                changes,
                &child_pointer,
                "media_payload",
                "base64-media-omit-v2",
            );
        } else {
            visit(
                child,
                &child_pointer,
                fingerprint_key,
                changes,
                media_type.as_deref(),
            );
        }
    }
}

fn replace(value: &mut Value, pointer: &str, kind: &str, rule: &str, changes: &mut Vec<Value>) {
    *value = json!({"$redacted":true,"kind":kind,"rule":rule});
    record(changes, pointer, kind, rule);
}

fn record(changes: &mut Vec<Value>, pointer: &str, kind: &str, rule: &str) {
    changes.push(json!({"pointer":pointer,"kind":kind,"rule":rule}));
}

fn normalize_name(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn escape_pointer(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

fn credential_kind(text: &str) -> Option<&'static str> {
    let trimmed = text.trim();
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("bearer ") || lower.starts_with("basic ") {
        return Some("authorization");
    }
    if [
        "ghp_",
        "gho_",
        "ghu_",
        "ghs_",
        "ghr_",
        "github_pat_",
        "sk-",
        "xoxb-",
        "xoxp-",
        "xoxa-",
        "akia",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix))
    {
        return Some("credential_prefix");
    }
    let mut jwt_parts = trimmed.split('.');
    if jwt_parts.clone().count() == 3
        && jwt_parts.all(|part| {
            part.len() >= 16
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        })
    {
        return Some("jwt");
    }
    None
}

fn redact_url(text: &str) -> Option<String> {
    if !(text.starts_with("http://") || text.starts_with("https://")) {
        return None;
    }
    let (base, suffix) = text.split_once('?')?;
    let (query, fragment) = suffix
        .split_once('#')
        .map_or((suffix, None), |(query, fragment)| (query, Some(fragment)));
    let mut changed = false;
    let redacted = query
        .split('&')
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            let normalized = normalize_name(&percent_decode_key(key));
            if SENSITIVE_QUERY_KEYS
                .iter()
                .any(|candidate| normalized == *candidate)
            {
                changed = true;
                format!("{key}=%5BREDACTED%5D")
            } else if value.is_empty() {
                key.to_string()
            } else {
                pair.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("&");
    changed.then(|| {
        let fragment = fragment
            .map(|value| format!("#{value}"))
            .unwrap_or_default();
        format!("{base}?{redacted}{fragment}")
    })
}

fn percent_decode_key(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut output = String::with_capacity(value.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let (Some(high), Some(low)) = (hex(bytes[index + 1]), hex(bytes[index + 2]))
        {
            output.push((high * 16 + low) as char);
            index += 3;
        } else {
            output.push(if bytes[index] == b'+' {
                ' '
            } else {
                bytes[index] as char
            });
            index += 1;
        }
    }
    output
}

fn hex(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn is_media_field(normalized: &str, media_type: Option<&str>) -> bool {
    normalized.contains("base64")
        || normalized.contains("image")
        || normalized.contains("audio")
        || (matches!(normalized, "data" | "content" | "bytes")
            && media_type
                .is_some_and(|kind| kind.starts_with("image/") || kind.starts_with("audio/")))
}

fn media_payload(text: &str, media_type_hint: Option<&str>) -> Option<(String, usize)> {
    if let Some(rest) = text.strip_prefix("data:") {
        let (header, encoded) = rest.split_once(',')?;
        if !header.to_ascii_lowercase().contains(";base64") {
            return None;
        }
        let media_type = header
            .split(';')
            .next()
            .unwrap_or("application/octet-stream");
        if !(media_type.starts_with("image/") || media_type.starts_with("audio/")) {
            return None;
        }
        return Some((media_type.to_string(), decoded_size(encoded)));
    }
    let media_type = media_type_hint?;
    if !(media_type.starts_with("image/") || media_type.starts_with("audio/"))
        || !is_likely_base64(text)
    {
        return None;
    }
    Some((media_type.to_string(), decoded_size(text)))
}

fn is_likely_base64(value: &str) -> bool {
    value.len() >= 64
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'-' | b'_' | b'=')
        })
}

fn decoded_size(value: &str) -> usize {
    let padding = value.bytes().rev().take_while(|byte| *byte == b'=').count();
    value.len().saturating_mul(3) / 4 - padding.min(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_nested_secrets_urls_mcp_auth_and_media() {
        let media = format!("data:image/png;base64,{}", "A".repeat(128));
        let value = json!({
            "Authorization":"Bearer abc",
            "nested":{"api-key":"sk-test"},
            "env":{"PATH":"/bin"},
            "callback":"https://example.test/cb?state=ok&X-Amz-Signature=secret#done",
            "mcp":{"auth":{"bearerToken":"jwt-value"}},
            "image":media
        });
        let (redacted, audit) = redact(&value, &[7; 32]);
        assert_eq!(redacted["Authorization"]["$redacted"], true);
        assert_eq!(redacted["nested"]["api-key"]["$redacted"], true);
        assert_eq!(redacted["env"]["PATH"]["$redacted"], true);
        assert_eq!(
            redacted["callback"],
            "https://example.test/cb?state=ok&X-Amz-Signature=%5BREDACTED%5D#done"
        );
        assert_eq!(redacted["mcp"]["auth"]["bearerToken"]["$redacted"], true);
        assert_eq!(redacted["image"]["kind"], "media_payload");
        assert_eq!(audit["ruleVersion"], "known-secrets-v2");
        assert_eq!(audit["pointers"].as_array().unwrap().len(), 6);
    }

    #[test]
    fn preserves_non_secret_urls_and_plain_long_text() {
        let value = json!({
            "url":"https://example.test/?page=2&sort=asc",
            "content":"A normal sentence that is deliberately much longer than sixty four bytes but is not base64."
        });
        let (redacted, audit) = redact(&value, &[1; 32]);
        assert_eq!(redacted, value);
        assert!(audit["pointers"].as_array().unwrap().is_empty());
    }
}
