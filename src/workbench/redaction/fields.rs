//! Recognize common field markers in incomplete JSON, code and assignments.
//! This is deliberately not a JavaScript interpreter or a general secret detector.

const NAMES: &[&str] = &[
    "authorization",
    "cookie",
    "password",
    "token",
    "api_key",
    "apikey",
    "api-key",
    "access_token",
    "accesstoken",
    "refresh_token",
    "refreshtoken",
    "client_secret",
    "clientsecret",
    "private_key",
    "privatekey",
    "encrypted_content",
    "encryptedcontent",
    "encrypted_function_args",
    "encryptedfunctionargs",
    "image_url",
    "imageurl",
    "image_base64",
    "imagebase64",
    "env",
];

#[derive(PartialEq)]
pub(super) enum FieldMatch {
    No,
    Prefix,
    Sensitive,
}

// Decode only the ASCII characters used in field names and delimiters. Keep
// original text in the caller; decoding here cannot produce executable content.
fn next(text: &str) -> Result<Option<(u8, usize)>, ()> {
    let bytes = text.as_bytes();
    let Some(&first) = bytes.first() else {
        return Ok(None);
    };
    if first != b'\\' {
        return Ok(Some((first, 1)));
    }
    let slash_count = bytes.iter().take_while(|&&byte| byte == b'\\').count();
    let Some(&kind) = bytes.get(slash_count) else {
        return Ok(None);
    };
    if matches!(kind, b'"' | b'\'') {
        return Ok(Some((kind, slash_count + 1)));
    }
    let digits = match kind {
        b'u' => 4,
        b'x' => 2,
        _ => return Err(()),
    };
    let start = slash_count + 1;
    let end = start + digits;
    let seen = &bytes[start..bytes.len().min(end)];
    if !seen.iter().all(u8::is_ascii_hexdigit) {
        return Err(());
    }
    if bytes.len() < end {
        return Ok(None);
    }
    let code =
        u16::from_str_radix(std::str::from_utf8(seen).map_err(|_| ())?, 16).map_err(|_| ())?;
    if code > 127 {
        return Err(());
    }
    Ok(Some((code as u8, end)))
}

pub(super) fn classify(text: &str) -> FieldMatch {
    use FieldMatch::*;
    let mut rest = text;
    let (first, used) = match next(rest) {
        Ok(Some(value)) => value,
        Ok(None) => return Prefix,
        Err(_) => return No,
    };
    let quote = matches!(first, b'"' | b'\'').then_some(first);
    if quote.is_some() {
        rest = &rest[used..];
    }
    let mut name = [0_u8; 32];
    let mut name_len = 0;
    let mut whitespace = false;
    loop {
        let (byte, used) = match next(rest) {
            Ok(Some(value)) => value,
            Ok(None) => return if text.len() > 256 { Sensitive } else { Prefix },
            Err(_) => return No,
        };
        if byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-') {
            if whitespace {
                return No;
            }
            if name_len == name.len() {
                return No;
            }
            name[name_len] = byte.to_ascii_lowercase();
            name_len += 1;
            if !NAMES
                .iter()
                .any(|candidate| candidate.as_bytes().starts_with(&name[..name_len]))
            {
                return No;
            }
        } else if NAMES
            .iter()
            .any(|candidate| candidate.as_bytes() == &name[..name_len])
        {
            if quote == Some(byte) || (quote.is_none() && matches!(byte, b':' | b'=')) {
                return Sensitive;
            }
            if byte.is_ascii_whitespace() && quote.is_none() {
                whitespace = true;
                if text.len() > 256 {
                    return Sensitive;
                }
            } else {
                return No;
            }
        } else {
            return No;
        }
        rest = &rest[used..];
    }
}
