use serde_json::{Value, json};

const SECRET_KEYS: &[&str] = &[
    "authorization",
    "cookie",
    "set-cookie",
    "api_key",
    "apikey",
    "access_token",
    "refresh_token",
    "oauth_token",
    "password",
    "secret",
    "client_secret",
];

pub fn redact(value: &Value) -> (Value, Value) {
    let mut output = value.clone();
    let mut pointers = Vec::new();
    visit(&mut output, "", &mut pointers);
    (
        output,
        json!({"ruleVersion":"known-secrets-v1","pointers":pointers}),
    )
}

fn visit(value: &mut Value, pointer: &str, pointers: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                let escaped = key.replace('~', "~0").replace('/', "~1");
                let child_pointer = format!("{pointer}/{escaped}");
                let normalized = key.to_ascii_lowercase().replace('-', "_");
                if SECRET_KEYS
                    .iter()
                    .any(|candidate| normalized == candidate.replace('-', "_"))
                {
                    *child = json!({"$redacted":true,"kind":"secret","rule":"known-secret-key-v1"});
                    pointers.push(child_pointer);
                } else if normalized == "env"
                    || normalized == "environment"
                    || normalized == "environment_variables"
                {
                    if let Value::Object(env) = child {
                        for env_value in env.values_mut() {
                            *env_value = json!({"$redacted":true,"kind":"environment_value","rule":"environment-default-deny-v1"});
                        }
                        pointers.push(child_pointer);
                    }
                } else {
                    visit(child, &child_pointer, pointers);
                }
            }
        }
        Value::Array(values) => {
            for (index, child) in values.iter_mut().enumerate() {
                visit(child, &format!("{pointer}/{index}"), pointers);
            }
        }
        Value::String(text)
            if text.starts_with("Bearer ")
                || text.starts_with("ghp_")
                || text.starts_with("sk-") =>
        {
            *value = json!({"$redacted":true,"kind":"credential","rule":"credential-prefix-v1"});
            pointers.push(pointer.to_string());
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_nested_secrets_and_environment_values() {
        let value = json!({"Authorization":"Bearer abc", "nested":{"api_key":"sk-test"}, "env":{"PATH":"/bin"}});
        let (redacted, audit) = redact(&value);
        assert_eq!(redacted["Authorization"]["$redacted"], true);
        assert_eq!(redacted["nested"]["api_key"]["$redacted"], true);
        assert_eq!(redacted["env"]["PATH"]["$redacted"], true);
        assert_eq!(audit["pointers"].as_array().unwrap().len(), 3);
    }
}
