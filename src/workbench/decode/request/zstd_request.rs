//! HTTP request observation only: decode after the bounded wire body completes.
//! No state or decompressed bytes return to the forwarding path.
use super::FramingIssue;
use zstd::stream::raw::{DParameter, Decoder, Operation};

// A 2 MiB window plus zstd's bounded block buffers/context and our scratch block.
// Reserved in the aggregate observer budget while this synchronous decode runs.
const WORKSPACE: usize = 3 * 1024 * 1024;
const BLOCK: usize = 16 * 1024;

pub(super) fn decode(input: &[u8], limit: usize, budget: usize) -> Result<Vec<u8>, FramingIssue> {
    if input.is_empty() {
        return Err(FramingIssue::Incomplete);
    }
    // zstd's optional legacy decoder does not use the modern max-window guard.
    // Native requests use the current frame format; reject legacy/skippable magic.
    if !input.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
        return Err(FramingIssue::UnsupportedExtension);
    }
    let available = budget
        .checked_sub(WORKSPACE)
        .ok_or(FramingIssue::TooLarge)?;
    let limit = limit.min(available);
    let mut decoder = Decoder::new().map_err(|_| FramingIssue::UnsupportedExtension)?;
    decoder
        .set_parameter(DParameter::WindowLogMax(21))
        .map_err(|_| FramingIssue::UnsupportedExtension)?;
    let mut output = Vec::new();
    let mut consumed = 0;
    let mut block = [0u8; BLOCK];
    // Each iteration must consume input or produce output. Both totals are bounded.
    loop {
        let status = decoder
            .run_on_buffers(&input[consumed..], &mut block)
            .map_err(|_| FramingIssue::UnsupportedExtension)?;
        consumed += status.bytes_read;
        if output.len().saturating_add(status.bytes_written) > limit {
            return Err(FramingIssue::TooLarge);
        }
        output.reserve_exact(status.bytes_written);
        output.extend_from_slice(&block[..status.bytes_written]);
        if status.remaining == 0 {
            // Reject concatenation/trailing data: never publish a partial JSON body.
            return if consumed == input.len() {
                Ok(output)
            } else {
                Err(FramingIssue::UnsupportedExtension)
            };
        }
        if status.bytes_read == 0 && status.bytes_written == 0 {
            return Err(FramingIssue::Incomplete);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn encoded(body: &[u8]) -> Vec<u8> {
        zstd::stream::encode_all(body, 3).unwrap()
    }
    #[test]
    fn rejects_truncation_corruption_trailing_and_concatenated_frames() {
        let good = encoded(br#"{"model":"fixture"}"#);
        assert_eq!(
            decode(&good, 4096, WORKSPACE + 4096).unwrap(),
            br#"{"model":"fixture"}"#
        );
        for end in 0..good.len() {
            assert!(decode(&good[..end], 4096, WORKSPACE + 4096).is_err());
        }
        let mut trailing = good.clone();
        trailing.push(0);
        assert!(decode(&trailing, 4096, WORKSPACE + 4096).is_err());
        let mut joined = good.clone();
        joined.extend(&good);
        assert!(decode(&joined, 4096, WORKSPACE + 4096).is_err());
        assert!(decode(b"PRIVATE_BROKEN_DATA", 4096, WORKSPACE + 4096).is_err());
        for magic in [0xfd2fb51eu32, 0xfd2fb522, 0xfd2fb527, 0x184d2a50] {
            assert!(matches!(
                decode(&magic.to_le_bytes(), 4096, WORKSPACE + 4096),
                Err(FramingIssue::UnsupportedExtension)
            ));
        }
    }
    #[test]
    fn bounds_expansion_workspace_and_declared_window_before_publish() {
        let bomb = encoded(&vec![b' '; 128 * 1024]);
        assert!(matches!(
            decode(&bomb, 1024, WORKSPACE + 1024),
            Err(FramingIssue::TooLarge)
        ));
        assert!(matches!(
            decode(&bomb, 1024, WORKSPACE - 1),
            Err(FramingIssue::TooLarge)
        ));
        // Non-single-segment Zstd frame with a declared 2 GiB window.
        let oversized_window = [0x28, 0xb5, 0x2f, 0xfd, 0x00, 0xa8, 0x01, 0x00, 0x00];
        assert!(matches!(
            decode(&oversized_window, 1024, WORKSPACE + 1024),
            Err(FramingIssue::UnsupportedExtension)
        ));
    }
}
