//! Bounded Telegram input normalization. Synthetic coverage is not live Telegram validation.
//! Update deduplication, binding lifetime, and native execution belong to the task actor.

use serde_json::Value;
use uuid::Uuid;

pub mod client;

pub(super) const TOKEN_LIMIT: usize = 256;

pub(super) fn valid_token(token: &str) -> bool {
    token.len() <= TOKEN_LIMIT
        && token.split_once(':').is_some_and(|(id, secret)| {
            !id.is_empty()
                && id.bytes().all(|byte| byte.is_ascii_digit())
                && id.parse::<i64>().is_ok_and(|id| id > 0)
                && !secret.is_empty()
                && secret
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        })
}

pub struct TelegramBinding {
    pub bot_id: i64,
    pub user_id: i64,
    pub chat_id: i64,
    pub generation: Uuid,
}

// Deliberately no Debug: incoming text and callback identifiers are private payloads.
pub enum Inbound {
    Text {
        update_id: i64,
        message_id: i64,
        text: String,
    },
    /// A reply is classified separately so a stale or malformed answer can
    /// never fall back to executable task text.
    QuestionReply {
        update_id: i64,
        message_id: i64,
        prompt_message_id: i64,
        text: String,
    },
    Callback {
        update_id: i64,
        query_id: String,
        message_id: i64,
        reference: String,
    },
    Ignore(Ignore),
    Reject(Reject),
}

#[derive(Debug, PartialEq, Eq)]
pub enum Ignore {
    UnsupportedUpdate,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Reject {
    Malformed,
    UnauthorizedUser,
    UnauthorizedChat,
    NotPrivate,
    UnsupportedMessage,
    InvalidText,
    InvalidEntity,
    InvalidCallback,
    InvalidReply,
}

pub fn authorize(update: &Value, binding: &TelegramBinding) -> Inbound {
    match authorize_inner(update, binding) {
        Ok(input) => input,
        Err(reason) => Inbound::Reject(reason),
    }
}

fn positive(value: Option<&Value>) -> Option<i64> {
    value.and_then(Value::as_i64).filter(|id| *id > 0)
}

fn identity(chat: &Value, sender: &Value, binding: &TelegramBinding) -> Result<(), Reject> {
    if chat["type"].as_str() != Some("private") {
        return Err(Reject::NotPrivate);
    }
    if positive(chat.get("id")) != Some(binding.chat_id) || binding.chat_id <= 0 {
        return Err(Reject::UnauthorizedChat);
    }
    if positive(sender.get("id")) != Some(binding.user_id)
        || binding.user_id <= 0
        || sender["is_bot"].as_bool() != Some(false)
    {
        return Err(Reject::UnauthorizedUser);
    }
    Ok(())
}

fn authorize_inner(update: &Value, binding: &TelegramBinding) -> Result<Inbound, Reject> {
    let object = update.as_object().ok_or(Reject::Malformed)?;
    let update_id = object
        .get("update_id")
        .and_then(Value::as_i64)
        .filter(|id| *id >= 0)
        .ok_or(Reject::Malformed)?;
    // Exactly one update variant: do not silently prefer text over a second variant.
    let mut variants = object.iter().filter(|(key, _)| key.as_str() != "update_id");
    let Some((kind, payload)) = variants.next() else {
        return Err(Reject::Malformed);
    };
    if variants.next().is_some() {
        return Err(Reject::Malformed);
    }
    match kind.as_str() {
        "message" => {
            identity(&payload["chat"], &payload["from"], binding)?;
            let message = payload.as_object().ok_or(Reject::Malformed)?;
            // Closed text-message shape. Known harmless metadata is accepted; new content
            // variants require review before they can become executable input.
            if message.keys().any(|key| {
                !matches!(
                    key.as_str(),
                    "message_id"
                        | "from"
                        | "chat"
                        | "date"
                        | "text"
                        | "entities"
                        | "forward_origin"
                        | "is_automatic_forward"
                        | "reply_to_message"
                        | "external_reply"
                        | "quote"
                        | "edit_date"
                        | "has_protected_content"
                        | "is_from_offline"
                        | "via_bot"
                        | "reply_markup"
                        | "effect_id"
                )
            }) {
                return Err(Reject::UnsupportedMessage);
            }
            let message_id = positive(message.get("message_id")).ok_or(Reject::Malformed)?;
            let text = message
                .get("text")
                .and_then(Value::as_str)
                .ok_or(Reject::UnsupportedMessage)?;
            if text.is_empty()
                || text.len() > 16384
                || text.encode_utf16().count() > 4096
                || text.chars().any(|ch| ch.is_control() && ch != '\n')
            {
                return Err(Reject::InvalidText);
            }
            validate_entities(text, message.get("entities"))?;
            if let Some(original) = message.get("reply_to_message") {
                if [
                    "forward_origin",
                    "is_automatic_forward",
                    "external_reply",
                    "quote",
                    "edit_date",
                    "via_bot",
                ]
                .iter()
                .any(|key| message.contains_key(*key))
                {
                    return Err(Reject::InvalidReply);
                }
                let original = original.as_object().ok_or(Reject::InvalidReply)?;
                let prompt_message_id =
                    positive(original.get("message_id")).ok_or(Reject::InvalidReply)?;
                let chat = original.get("chat").ok_or(Reject::InvalidReply)?;
                let sender = original.get("from").ok_or(Reject::InvalidReply)?;
                if chat["type"] != "private"
                    || positive(chat.get("id")) != Some(binding.chat_id)
                    || positive(sender.get("id")) != Some(binding.bot_id)
                    || binding.bot_id <= 0
                    || sender["is_bot"] != true
                    || positive(original.get("date")).is_none()
                    || [
                        "forward_origin",
                        "is_automatic_forward",
                        "external_reply",
                        "via_bot",
                    ]
                    .iter()
                    .any(|key| original.contains_key(*key))
                {
                    return Err(Reject::InvalidReply);
                }
                // Do not retain the quoted bot body, markup, or nested entities.
                return Ok(Inbound::QuestionReply {
                    update_id,
                    message_id,
                    prompt_message_id,
                    text: text.to_owned(),
                });
            }
            Ok(Inbound::Text {
                update_id,
                message_id,
                text: text.to_owned(),
            })
        }
        "callback_query" => {
            let callback = payload.as_object().ok_or(Reject::InvalidCallback)?;
            if callback.contains_key("inline_message_id")
                || callback.contains_key("game_short_name")
            {
                return Err(Reject::InvalidCallback);
            }
            let message = callback.get("message").ok_or(Reject::InvalidCallback)?;
            identity(&message["chat"], &payload["from"], binding)?;
            // date == 0 is an InaccessibleMessage, never an actionable interaction.
            if positive(message.get("date")).is_none()
                || positive(message["from"].get("id")) != Some(binding.bot_id)
                || binding.bot_id <= 0
                || message["from"]["is_bot"].as_bool() != Some(true)
            {
                return Err(Reject::InvalidCallback);
            }
            let message_id = positive(message.get("message_id")).ok_or(Reject::InvalidCallback)?;
            let query_id = callback
                .get("id")
                .and_then(Value::as_str)
                .ok_or(Reject::InvalidCallback)?;
            let reference = callback
                .get("data")
                .and_then(Value::as_str)
                .ok_or(Reject::InvalidCallback)?;
            if query_id.is_empty()
                || query_id.len() > 1024
                || query_id.chars().any(char::is_control)
                || reference.is_empty()
                || reference.len() > 64
                || !reference
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            {
                return Err(Reject::InvalidCallback);
            }
            // This is only an authenticated opaque reference. The actor must validate
            // generation, task, message, expiry, allowed action, and single consumption.
            Ok(Inbound::Callback {
                update_id,
                query_id: query_id.to_owned(),
                message_id,
                reference: reference.to_owned(),
            })
        }
        _ => Ok(Inbound::Ignore(Ignore::UnsupportedUpdate)),
    }
}

fn validate_entities(text: &str, entities: Option<&Value>) -> Result<(), Reject> {
    let Some(entities) = entities else {
        return Ok(());
    };
    let entities = entities.as_array().ok_or(Reject::InvalidEntity)?;
    if entities.len() > 4096 {
        return Err(Reject::InvalidEntity);
    }
    let mut boundaries = vec![true];
    for ch in text.chars() {
        if ch.len_utf16() == 2 {
            boundaries.push(false);
        }
        boundaries.push(true);
    }
    for entity in entities {
        let offset = entity["offset"]
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .ok_or(Reject::InvalidEntity)?;
        let length = entity["length"]
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| *n > 0)
            .ok_or(Reject::InvalidEntity)?;
        let end = offset.checked_add(length).ok_or(Reject::InvalidEntity)?;
        if boundaries.get(offset) != Some(&true)
            || boundaries.get(end) != Some(&true)
            || !matches!(
                entity["type"].as_str(),
                Some(
                    "mention"
                        | "hashtag"
                        | "cashtag"
                        | "bot_command"
                        | "url"
                        | "email"
                        | "phone_number"
                        | "bold"
                        | "italic"
                        | "underline"
                        | "strikethrough"
                        | "spoiler"
                        | "blockquote"
                        | "expandable_blockquote"
                        | "code"
                        | "pre"
                        | "text_link"
                        | "text_mention"
                        | "custom_emoji"
                        | "date_time"
                )
            )
        {
            return Err(Reject::InvalidEntity);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn binding() -> TelegramBinding {
        TelegramBinding {
            bot_id: 9,
            user_id: 1_i64 << 40,
            chat_id: 1_i64 << 40,
            generation: Uuid::nil(),
        }
    }
    fn message(text: &str) -> Value {
        json!({"update_id":1,"message":{"message_id":2,"date":3,"from":{"id":1_i64<<40,"is_bot":false},"chat":{"id":1_i64<<40,"type":"private"},"text":text}})
    }
    fn rejected(value: &Value, reason: Reject) {
        match authorize(value, &binding()) {
            Inbound::Reject(actual) => assert_eq!(actual, reason),
            _ => panic!("expected rejection"),
        }
    }

    #[test]
    fn exact_identity_required_and_forward_origin_is_not_authority() {
        let mut value = message("hello");
        assert!(matches!(
            authorize(&value, &binding()),
            Inbound::Text { .. }
        ));
        value["message"]["from"]["id"] = json!(7);
        value["message"]["forward_origin"] = json!({"sender_user":{"id":1_i64<<40}});
        rejected(&value, Reject::UnauthorizedUser);
        value = message("hello");
        value["message"]["from"]["is_bot"] = json!(true);
        rejected(&value, Reject::UnauthorizedUser);
        value = message("hello");
        value["message"]["chat"]["id"] = json!(7);
        rejected(&value, Reject::UnauthorizedChat);
        value = message("hello");
        value["message"]["chat"]["type"] = json!("group");
        rejected(&value, Reject::NotPrivate);
        value = message("hello");
        value["message"].as_object_mut().unwrap().remove("from");
        rejected(&value, Reject::UnauthorizedUser);
    }

    #[test]
    fn unsupported_payloads_do_not_become_tasks() {
        for kind in ["edited_message", "business_message", "future_message"] {
            let value = json!({"update_id":1,kind:message("hello")["message"].clone()});
            assert!(matches!(
                authorize(&value, &binding()),
                Inbound::Ignore(Ignore::UnsupportedUpdate)
            ));
        }
        let mut value = message("hello");
        value["message"]["caption"] = json!("caption");
        rejected(&value, Reject::UnsupportedMessage);
        value = message("hello");
        value["message"]["future_content"] = json!({});
        rejected(&value, Reject::UnsupportedMessage);
        value = message("hello");
        value["callback_query"] = json!({});
        rejected(&value, Reject::Malformed);
    }

    #[test]
    fn update_ids_are_nonnegative_integers_and_are_not_an_authority() {
        let mut value = message("hello");
        value["update_id"] = json!(0);
        assert!(matches!(
            authorize(&value, &binding()),
            Inbound::Text { update_id: 0, .. }
        ));
        for id in [json!(-1), json!("1"), Value::Null] {
            value["update_id"] = id;
            rejected(&value, Reject::Malformed);
        }
    }

    #[test]
    fn text_and_entities_use_utf16_scalar_boundaries() {
        let mut value = message("a😀b\n");
        value["message"]["entities"] = json!([{"type":"bold","offset":1,"length":2}]);
        assert!(matches!(
            authorize(&value, &binding()),
            Inbound::Text { .. }
        ));
        for (offset, length) in [(1, 1), (2, 1), (4, 2), (0, 0)] {
            value["message"]["entities"] = json!([{"type":"bold","offset":offset,"length":length}]);
            rejected(&value, Reject::InvalidEntity);
        }
        rejected(&message("\u{1b}[A"), Reject::InvalidText);
        rejected(&message("hello\r"), Reject::InvalidText);
        rejected(&message(&"😀".repeat(2049)), Reject::InvalidText);
        assert!(matches!(
            authorize(&message(&"😀".repeat(2048)), &binding()),
            Inbound::Text { .. }
        ));
    }

    #[test]
    fn callbacks_require_accessible_bot_message_and_opaque_reference() {
        let mut value = json!({"update_id":1,"callback_query":{"id":"query","from":{"id":1_i64<<40,"is_bot":false},"data":"ref_123-abc","message":{"message_id":2,"date":3,"from":{"id":9,"is_bot":true},"chat":{"id":1_i64<<40,"type":"private"}}}});
        assert!(matches!(
            authorize(&value, &binding()),
            Inbound::Callback { .. }
        ));
        value["callback_query"]["message"]["date"] = json!(0);
        rejected(&value, Reject::InvalidCallback);
        value["callback_query"]["message"]["date"] = json!(3);
        value["callback_query"]["data"] = json!("approve command");
        rejected(&value, Reject::InvalidCallback);
        value["callback_query"]["data"] = json!("x".repeat(65));
        rejected(&value, Reject::InvalidCallback);
        value["callback_query"]["data"] = json!("ref");
        value["callback_query"]["from"]["id"] = json!(7);
        rejected(&value, Reject::UnauthorizedUser);
    }

    #[test]
    fn question_replies_require_exact_accessible_bot_message_metadata() {
        let mut value = message("synthetic answer");
        value["message"]["reply_to_message"] = json!({"message_id":45,"date":3,
            "from":{"id":9,"is_bot":true},"chat":{"id":1_i64<<40,"type":"private"},
            "text":"SYNTHETIC_QUOTED_BODY_NOT_RETAINED"});
        assert!(matches!(
            authorize(&value, &binding()),
            Inbound::QuestionReply {
                prompt_message_id: 45,
                message_id: 2,
                ..
            }
        ));
        for field in ["message_id", "date", "from", "chat"] {
            let mut bad = value.clone();
            bad["message"]["reply_to_message"]
                .as_object_mut()
                .unwrap()
                .remove(field);
            rejected(&bad, Reject::InvalidReply);
        }
        for (field, replacement) in [
            ("message_id", json!(0)),
            ("date", json!(0)),
            ("from", json!({"id":8,"is_bot":true})),
            ("from", json!({"id":9,"is_bot":false})),
            ("chat", json!({"id":1_i64<<40,"type":"group"})),
            ("chat", json!({"id":8,"type":"private"})),
        ] {
            let mut bad = value.clone();
            bad["message"]["reply_to_message"][field] = replacement;
            rejected(&bad, Reject::InvalidReply);
        }
        for field in [
            "forward_origin",
            "is_automatic_forward",
            "external_reply",
            "quote",
            "edit_date",
            "via_bot",
        ] {
            let mut bad = value.clone();
            bad["message"][field] = json!(true);
            rejected(&bad, Reject::InvalidReply);
        }
        value["message"]["reply_to_message"] = Value::Null;
        rejected(&value, Reject::InvalidReply);
    }
}
