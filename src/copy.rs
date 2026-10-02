// The terminal copy, from the `### Terminal copy` section of
// app-docs/designs/coding-agent-surface.md.
//
// Voice: instrument, not coach. Second person for the developer, "Flueny" for the
// product, never "we". No praise and no adjectives of judgement. ASCII only, no
// emoji and no box drawing, because terminals and CI logs vary. Colour is never
// the sole carrier of meaning, which here is simply that nothing emits colour.
//
// Two rules in that section contradict each other on two of the six strings: the
// exemplars for "Setup" and "Dry-run daily receipt" are 85 and 87 columns wide
// against a stated 80 column hard cap. The words are the contract, so they are
// kept verbatim and reflowed to 80 by `wrap`; the tests assert both halves, that
// no rendered line exceeds 80 and that the reflowed text still reads back word
// for word.

pub const COLUMNS: usize = 80;

pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            if line.is_empty() {
                line.push_str(word);
            } else if line.len() + 1 + word.len() <= width {
                line.push(' ');
                line.push_str(word);
            } else {
                lines.push(std::mem::take(&mut line));
                line.push_str(word);
            }
        }
        lines.push(line);
    }
    lines
}

pub fn wrap80(text: &str) -> String {
    wrap(text, COLUMNS).join("\n")
}

// 1. Setup, after SSO completes. The count comes from the handshake: printing 7
// when the server said 3 would be a number the product cannot substantiate.
pub fn setup_connected(dry_run: bool, dry_run_days: i64, app_url: &str) -> String {
    let first = if dry_run {
        format!(
            "Flueny is connected. Dry run is on for {dry_run_days} {}: nothing is scored, nothing is blocked.",
            plural(dry_run_days, "day")
        )
    } else {
        "Flueny is connected. Dry run is over: signal is scored, nothing is blocked.".to_string()
    };
    let second =
        format!("Your prompts and your code never leave this machine. See what is sent: {app_url}/coding/privacy");
    render(&[first, second])
}

// 2. Dry-run daily receipt (design decision 44), corrected by decision 58: one
// PostToolUse yields a tool-use event and, on an edit, an edit-decision event
// too, so the ratio framing is gone. What is left is what was looked at, what
// left, and what did not.
pub fn daily_receipt(tool_calls: i64, signals: i64, blocked: i64) -> String {
    render(&[
        format!(
            "Flueny observed {tool_calls} {} today and sent {signals} derived {}.",
            plural(tool_calls, "tool call"),
            plural(signals, "signal")
        ),
        format!(
            "It blocked {blocked} {}. No prompt, no code and no file content left this machine.",
            plural(blocked, "action")
        ),
        "See exactly what: flueny dry-run --today".to_string(),
    ])
}

// 3. PostToolUse nudge. M2, delivered as `additionalContext`. Nothing in M1 calls
// this: `intervention` is always null and the client must not render it yet.
pub fn nudge(finding: &str, standard: &str, url: &str) -> String {
    render(&[
        format!("Flueny: {finding} Your org's standard {standard} requires parameterized queries."),
        format!("Why: {url}"),
    ])
}

// 4. PreToolUse denial. M3, four parts in this order. Unused in M1 by design:
// `capabilities.canEnforce` is false for every agent and there is no gate.
pub fn denial(reason: &str, rule: &str, url: &str, token: &str) -> String {
    render(&[
        format!("Flueny blocked this command because {reason}"),
        format!("Rule: {rule}"),
        format!("Why: {url}"),
        format!("Proceed anyway: flueny allow --once {token}   (recorded)"),
    ])
}

// 5. Weekly summary. `/coding/signal` is authoritative and this is additive
// (design decision 46). The second sentence only exists when its number is not
// zero: "None of them touched tests" is a sentence about nothing happening.
pub fn weekly_summary(rejected: i64, decisions: i64, touched_tests: i64, app_url: &str) -> String {
    let first = format!(
        "You rejected {rejected} of {decisions} agent {} this week.",
        plural(decisions, "edit")
    );
    let tests = if touched_tests > 0 {
        format!(" {} of them touched tests.", capitalize(&number_word(touched_tests)))
    } else {
        String::new()
    };
    render(&[first + &tests, format!("Full breakdown: {app_url}/coding/signal")])
}

// 6. Capability unlock at SessionStart. M4 (design decision 47).
pub fn capability_unlock(path_class: &str) -> String {
    render(&[format!(
        "Flueny: agent writes to {path_class}/ are now available on this account."
    )])
}

// The one place a rendered string becomes terminal output. ASCII is enforced here
// rather than trusted, because the strings interpolate a URL and a path class
// that come from a server.
fn render(parts: &[String]) -> String {
    parts
        .iter()
        .flat_map(|part| wrap(&to_ascii(part), COLUMNS))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn to_ascii(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\u{2018}' | '\u{2019}' => out.push('\''),
            '\u{201c}' | '\u{201d}' => out.push('"'),
            '\u{2013}' | '\u{2014}' => out.push('-'),
            '\u{2026}' => out.push_str("..."),
            '\n' => out.push('\n'),
            c if (' '..='~').contains(&c) => out.push(c),
            _ => {}
        }
    }
    out
}

const WORDS: [&str; 10] = [
    "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine",
];

pub fn number_word(value: i64) -> String {
    usize::try_from(value)
        .ok()
        .and_then(|i| WORDS.get(i))
        .map(|w| w.to_string())
        .unwrap_or_else(|| value.to_string())
}

fn capitalize(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

pub fn plural(count: i64, noun: &str) -> String {
    if count == 1 {
        noun.to_string()
    } else {
        format!("{noun}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use regex_lite::Regex;

    // The voice rules from `### Terminal copy` in the design plan, as assertions.
    fn all() -> Vec<String> {
        vec![
            setup_connected(true, 7, "https://app.flueny.ai"),
            setup_connected(false, 0, "https://app.flueny.ai"),
            daily_receipt(41, 6, 0),
            nudge(
                "this concatenates user input into SQL.",
                "SEC-12",
                "https://app.flueny.ai/knowledge/sec-12",
            ),
            denial(
                "it writes outside the approved repository scope.",
                "Repository scope (org standard SEC-12)",
                "https://app.flueny.ai/knowledge/sec-12",
                "a3f21c",
            ),
            weekly_summary(3, 11, 2, "https://app.flueny.ai"),
            capability_unlock("infra"),
        ]
    }

    fn flat(text: &str) -> String {
        text.split('\n').collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn no_rendered_line_exceeds_80_columns() {
        for block in all() {
            for line in block.split('\n') {
                assert!(line.len() <= COLUMNS, "{} columns: {line}", line.len());
            }
        }
    }

    #[test]
    fn every_string_is_ascii_with_no_dashes_of_the_long_kind() {
        for block in all() {
            assert!(block.chars().all(|c| c == '\n' || (' '..='~').contains(&c)), "{block}");
            assert!(!block.contains('\u{2013}') && !block.contains('\u{2014}'), "{block}");
        }
    }

    #[test]
    fn no_praise_and_never_we() {
        let praise = Regex::new(
            r"(?i)\b(great|nice|excellent|good job|well done|awesome|impressive|amazing|perfect|strong|poor|bad)\b",
        )
        .unwrap();
        let we = Regex::new(r"(?i)\bwe\b|\bour\b|\bus\b").unwrap();
        for block in all() {
            assert!(!praise.is_match(&block), "{block}");
            assert!(!we.is_match(&block), "{block}");
        }
    }

    #[test]
    fn the_words_are_the_design_plan_exemplars_reflowed_not_rewritten() {
        assert_eq!(
            flat(&setup_connected(true, 7, "<url>")),
            "Flueny is connected. Dry run is on for 7 days: nothing is scored, nothing is blocked. \
             Your prompts and your code never leave this machine. See what is sent: <url>/coding/privacy"
        );
        // Design decision 58: the measured pair 27 and 50, not an invented one.
        assert_eq!(
            flat(&daily_receipt(27, 50, 0)),
            "Flueny observed 27 tool calls today and sent 50 derived signals. \
             It blocked 0 actions. No prompt, no code and no file content left this machine. \
             See exactly what: flueny dry-run --today"
        );
        assert_eq!(
            flat(&weekly_summary(3, 11, 2, "<url>")),
            "You rejected 3 of 11 agent edits this week. Two of them touched tests. Full breakdown: <url>/coding/signal"
        );
        assert_eq!(
            flat(&denial(
                "it writes outside the approved repository scope.",
                "Repository scope (org standard SEC-12)",
                "<url>/knowledge/sec-12",
                "a3f21c"
            )),
            "Flueny blocked this command because it writes outside the approved repository scope. \
             Rule: Repository scope (org standard SEC-12) Why: <url>/knowledge/sec-12 \
             Proceed anyway: flueny allow --once a3f21c (recorded)"
        );
        assert_eq!(
            flat(&capability_unlock("infra")),
            "Flueny: agent writes to infra/ are now available on this account."
        );
    }

    #[test]
    fn the_denial_is_four_parts_in_order() {
        let text = denial("x.", "R", "u", "t");
        let lines: Vec<&str> = text.split('\n').collect();
        assert_eq!(lines.len(), 4);
        assert!(lines[0].starts_with("Flueny blocked this command because"));
        assert!(lines[1].starts_with("Rule:"));
        assert!(lines[2].starts_with("Why:"));
        assert!(lines[3].starts_with("Proceed anyway:"));
    }

    #[test]
    fn a_number_the_product_cannot_substantiate_is_not_printed() {
        let none = weekly_summary(0, 11, 0, "<url>");
        assert!(!none.contains("touched tests"));
        assert!(none.contains("You rejected 0 of 11 agent edits this week."));
        assert!(setup_connected(true, 3, "<url>").contains("on for 3 days"));
        assert!(setup_connected(true, 1, "<url>").contains("on for 1 day:"));
        assert!(setup_connected(false, 0, "<url>").contains("Dry run is over"));
    }

    #[test]
    fn singulars_read_as_english() {
        assert!(flat(&daily_receipt(1, 1, 0)).contains("1 tool call today"));
        assert!(flat(&daily_receipt(1, 1, 0)).contains("sent 1 derived signal."));
        assert!(flat(&daily_receipt(1, 1, 1)).contains("blocked 1 action."));
    }
}
