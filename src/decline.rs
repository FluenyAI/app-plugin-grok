// The markers a tool result carries when the developer declined or interrupted.
//
// Every marker here is APOSTROPHE FREE on purpose. Claude Code writes "the user
// doesn't want to proceed with this tool use", and inside a JSONL transcript that
// apostrophe may be a plain byte, a curly quote, or an escape depending on who
// wrote the line. The transcript sweep matches on raw bytes and never decodes the
// value it is matching (design decision 57), so it cannot normalize escapes, and
// a marker that spans the apostrophe would match one spelling out of three.

pub const DECLINE_MARKERS: [&str; 7] = [
    "want to proceed with this tool use",
    "want to take this action",
    "the user does not want to",
    "user rejected",
    "user denied",
    "request interrupted by user",
    "tool use was rejected",
];

// Bounded on purpose, and bounded tightly. A decline marker is not somewhere in a
// decline, it is at the front of one. Measured over 4228 real tool results, a
// window this size found every decline and nothing else, where 4000 found six
// results that only mentioned one. A miss here costs nothing, because the
// transcript sweep finds it at Stop under the same event id; a false rejection
// costs a number that is wrong in the flattering direction.
pub const SCAN_LIMIT: usize = 256;

/// The string path, used by the hooks, where the payload is already a parsed
/// object handed to this process by the host.
pub fn looks_declined(response: Option<&serde_json::Value>) -> bool {
    let Some(response) = response else { return false };
    let text = match response {
        serde_json::Value::Null => return false,
        serde_json::Value::String(s) => s.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    };
    let head: String = text.chars().take(SCAN_LIMIT).collect::<String>().to_lowercase();
    DECLINE_MARKERS.iter().any(|marker| head.contains(marker))
}

/// The byte path, used by the transcript sweep. Case-insensitive ASCII comparison
/// performed directly on the buffer: no slice is ever decoded into a string, so
/// "was this declined" is answered without reading the tool result.
pub fn bytes_look_declined(buf: &[u8], start: usize, end: usize) -> bool {
    let stop = end.min(start.saturating_add(SCAN_LIMIT)).min(buf.len());
    if start >= stop {
        return false;
    }
    let window = &buf[start..stop];
    DECLINE_MARKERS.iter().any(|marker| {
        let needle = marker.as_bytes();
        window.len() >= needle.len()
            && window
                .windows(needle.len())
                .any(|w| w.iter().zip(needle).all(|(a, b)| a.to_ascii_lowercase() == *b))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_decline_at_the_front_is_found_in_either_form() {
        let text = "The user doesn't want to proceed with this tool use.";
        assert!(looks_declined(Some(&json!(text))));
        assert!(bytes_look_declined(text.as_bytes(), 0, text.len()));
    }

    #[test]
    fn a_decline_quoted_far_into_a_result_is_not_one() {
        let text = format!("{}user rejected", "-".repeat(600));
        assert!(!looks_declined(Some(&json!(text))));
        assert!(!bytes_look_declined(text.as_bytes(), 0, text.len()));
        assert!(!looks_declined(None));
        assert!(!looks_declined(Some(&serde_json::Value::Null)));
    }
}
