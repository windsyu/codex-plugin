use serde_json::Value;

pub(super) fn search_expression(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

pub(super) fn parse_json(value: String) -> Value {
    serde_json::from_str(&value).unwrap_or(Value::Null)
}

pub(super) fn parse_optional_json(value: Option<String>) -> Value {
    value.map(parse_json).unwrap_or(Value::Null)
}
