//! CLI 0.154.0 output omission markers. These are text, not authenticated
//! metadata: a matching marker warns about completeness, never execution state.

pub(crate) fn has_truncation_marker(text: &str) -> bool {
    let count = |value: &str| {
        !value.is_empty()
            && value.bytes().all(|byte| byte.is_ascii_digit())
            && value.bytes().any(|byte| byte != b'0')
    };
    if text.lines().any(|line| {
        line.strip_prefix("... ")
            .and_then(|value| value.strip_suffix(" bytes omitted ..."))
            .is_some_and(count)
    }) {
        return true;
    }
    // Token/character truncation can occur in the middle of a line.
    for (start, _) in text.match_indices('…') {
        let suffix = &text[start + '…'.len_utf8()..];
        let digits = suffix.bytes().take_while(u8::is_ascii_digit).count();
        if count(&suffix[..digits])
            && (suffix[digits..].starts_with(" tokens truncated…")
                || suffix[digits..].starts_with(" chars truncated…"))
        {
            return true;
        }
    }
    text.lines().next().is_some_and(|line| {
        line.strip_prefix("Warning: truncated output (original token count: ")
            .and_then(|value| value.strip_suffix(')'))
            .is_some_and(count)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_exact_native_markers_without_treating_ordinary_numbers_as_proof() {
        for output in [
            "head\n... 123 bytes omitted ...\ntail",
            "…10 tokens truncated…",
            "head…3 chars truncated…tail",
            "Warning: truncated output (original token count: 4)\n\nhead",
        ] {
            assert!(has_truncation_marker(output));
        }
        for output in [
            "Original token count: 123\nfull output",
            "0 bytes omitted",
            "... 0 bytes omitted ...",
            "…0 tokens truncated…",
            "prefix ... 4 bytes omitted ... suffix",
            "... -1 bytes omitted ...",
            "... NaN bytes omitted ...",
            "…3 token truncated…",
            "tokens truncated",
        ] {
            assert!(!has_truncation_marker(output));
        }
    }
}
