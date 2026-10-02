// Turn verification (feature 0126). Computed here, per agent turn, at Stop,
// because the backend stores daily counters only and cannot reconstruct ordering:
// edit, then test, then pass is a different fact from test, then edit.
//
// Everything in TurnState is a step number, a hashed key, a bool or an enum, and
// what leaves is counts and the weakened-flag kinds.

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::store::{FlaggedChange, TurnEdit, TurnRun, TurnState};
use crate::types::CodingEvent;

// Path classes where an unverified change matters most. The scorers' own list is
// server side (eng finding 12); this one only decides `sensitiveUntested`, and it
// is the four the contract names.
pub const SENSITIVE_CLASSES: [&str; 4] = ["auth", "infra", "payments", "security"];

pub fn is_sensitive(path_class: Option<&str>) -> bool {
    path_class.is_some_and(|c| SENSITIVE_CLASSES.contains(&c))
}

impl TurnState {
    fn next_step(&mut self) -> i64 {
        self.step += 1;
        self.step
    }

    pub fn record_edit(&mut self, file: String, test: bool, sensitive: bool) {
        let step = self.next_step();
        self.edits.push(TurnEdit {
            step,
            file,
            test,
            sensitive,
        });
    }

    pub fn record_run(&mut self, outcome: &str, command: String) {
        let step = self.next_step();
        self.runs.push(TurnRun {
            step,
            outcome: outcome.to_string(),
            command,
        });
    }

    pub fn record_flags(&mut self, file: String, flags: Vec<String>, caught: bool) {
        if flags.is_empty() {
            return;
        }
        let step = self.next_step();
        self.flagged.push(FlaggedChange {
            step,
            file,
            flags,
            caught,
        });
    }

    /// A file the agent edited in this turn was reverted: every flagged change to
    /// it so far counts as caught.
    pub fn catch_file(&mut self, file: &str) {
        for flagged in self.flagged.iter_mut().filter(|f| f.file == file && !file.is_empty()) {
            flagged.caught = true;
        }
    }
}

const MAX_FLAGS: usize = 8;

/// The turn-verification fields, or None for a turn that made no accepted edit:
/// the contract sends one only per turn that changed something.
pub fn turn_verification(turn: &TurnState, mut event: CodingEvent) -> Option<CodingEvent> {
    let edits = &turn.edits;
    if edits.is_empty() {
        return None;
    }
    let named = || edits.iter().filter(|e| !e.file.is_empty());
    let source: HashSet<&str> = named().filter(|e| !e.test).map(|e| e.file.as_str()).collect();
    let tests: HashSet<&str> = named().filter(|e| e.test).map(|e| e.file.as_str()).collect();
    let first_source = named().filter(|e| !e.test).map(|e| e.step).min();
    let first_test = named().filter(|e| e.test).map(|e| e.step).min();
    let last_edit = edits.iter().map(|e| e.step).max().unwrap_or(0);
    let last_green = turn.runs.iter().filter(|r| r.outcome == "passed").map(|r| r.step).max();
    let ended_green = last_green.is_some_and(|green| green > last_edit);
    let after_green = match last_green {
        Some(green) => edits.iter().filter(|e| e.step > green).count(),
        None => edits.len(),
    };

    // Longest run of consecutive failed or errored runs of the same command. An
    // edit between them does not break it: edit, run, fail, edit, run, fail is
    // exactly the stuck loop this measures.
    let mut streak_max = 0;
    let mut streak = 0;
    let mut previous: Option<&str> = None;
    for run in &turn.runs {
        let failing = run.outcome == "failed" || run.outcome == "error";
        if failing && previous == Some(run.command.as_str()) && streak > 0 {
            streak += 1;
        } else if failing {
            streak = 1;
        } else {
            streak = 0;
        }
        previous = Some(run.command.as_str());
        streak_max = streak_max.max(streak);
    }

    let mut per_file: HashMap<&str, usize> = HashMap::new();
    for e in named() {
        *per_file.entry(e.file.as_str()).or_default() += 1;
    }
    let rework = per_file.values().filter(|n| **n >= 3).count();

    let flags: BTreeSet<&str> = turn
        .flagged
        .iter()
        .flat_map(|f| f.flags.iter().map(String::as_str))
        .collect();
    let caught = turn.flagged.iter().filter(|f| f.caught).count();

    event.edits_accepted = Some(edits.len() as i64);
    event.source_files_changed = Some(source.len() as i64);
    event.test_files_changed = Some(tests.len() as i64);
    event.test_first = Some(matches!((first_test, first_source), (Some(t), Some(s)) if t < s));
    event.test_runs = Some(turn.runs.len() as i64);
    event.ended_green = Some(ended_green);
    event.edits_after_last_green = Some(after_green as i64);
    event.failing_run_streak_max = Some(streak_max);
    event.rework_files = Some(rework as i64);
    event.sensitive_untested = Some(edits.iter().any(|e| e.sensitive) && !ended_green);
    event.weakened_flags = Some(flags.iter().take(MAX_FLAGS).map(|f| f.to_string()).collect());
    event.weakened_caught = Some(caught as i64);
    // Agreed with the backend for the Delegation bonus: subagent tool uses in
    // this turn, 0 when none.
    event.subagent_count = Some(turn.subagents);
    Some(event)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> CodingEvent {
        CodingEvent::new("tv:s:1".into(), "turn-verification", "t".into(), None, None)
    }

    #[test]
    fn a_turn_with_no_accepted_edit_sends_nothing() {
        let mut turn = TurnState::default();
        turn.record_run("passed", "k".into());
        assert_eq!(turn_verification(&turn, base()), None);
    }

    #[test]
    fn test_first_then_source_then_green_is_the_good_path() {
        let mut turn = TurnState::default();
        turn.record_edit("t1".into(), true, false);
        turn.record_run("failed", "npm test".into());
        turn.record_edit("s1".into(), false, false);
        turn.record_run("passed", "npm test".into());
        let e = turn_verification(&turn, base()).unwrap();
        assert_eq!(e.edits_accepted, Some(2));
        assert_eq!(e.source_files_changed, Some(1));
        assert_eq!(e.test_files_changed, Some(1));
        assert_eq!(e.test_first, Some(true));
        assert_eq!(e.test_runs, Some(2));
        assert_eq!(e.ended_green, Some(true));
        assert_eq!(e.edits_after_last_green, Some(0));
        assert_eq!(e.failing_run_streak_max, Some(1));
        assert_eq!(e.sensitive_untested, Some(false));
        assert_eq!(e.weakened_flags, Some(vec![]));
        assert_eq!(e.weakened_caught, Some(0));
        assert_eq!(e.subagent_count, Some(0));
    }

    #[test]
    fn an_edit_after_the_last_green_run_means_the_turn_did_not_end_green() {
        let mut turn = TurnState::default();
        turn.record_edit("s1".into(), false, true);
        turn.record_run("passed", "k".into());
        turn.record_edit("s1".into(), false, true);
        let e = turn_verification(&turn, base()).unwrap();
        assert_eq!(e.ended_green, Some(false));
        assert_eq!(e.edits_after_last_green, Some(1));
        assert_eq!(e.sensitive_untested, Some(true));
        assert_eq!(e.test_first, Some(false));
    }

    #[test]
    fn never_green_counts_every_edit_after_green() {
        let mut turn = TurnState::default();
        turn.record_edit("a".into(), false, false);
        turn.record_edit("b".into(), false, false);
        turn.record_run("unknown", "k".into());
        let e = turn_verification(&turn, base()).unwrap();
        assert_eq!(
            e.edits_after_last_green, e.edits_accepted,
            "equal means never green, per the backend"
        );
        assert_eq!(e.ended_green, Some(false));
    }

    #[test]
    fn a_stuck_loop_is_consecutive_failures_of_the_same_command() {
        let mut turn = TurnState::default();
        for _ in 0..3 {
            turn.record_edit("a".into(), false, false);
            turn.record_run("failed", "same".into());
        }
        turn.record_run("error", "other".into());
        turn.record_run("failed", "same".into());
        let e = turn_verification(&turn, base()).unwrap();
        assert_eq!(e.failing_run_streak_max, Some(3));
        assert_eq!(e.rework_files, Some(1), "edited three times");
    }

    #[test]
    fn a_passing_run_breaks_the_streak() {
        let mut turn = TurnState::default();
        turn.record_edit("a".into(), false, false);
        for outcome in ["failed", "failed", "passed", "failed", "error"] {
            turn.record_run(outcome, "same".into());
        }
        assert_eq!(
            turn_verification(&turn, base()).unwrap().failing_run_streak_max,
            Some(2)
        );
    }

    #[test]
    fn weakened_flags_are_deduped_and_caught_counts_flagged_changes_caught() {
        let mut turn = TurnState::default();
        turn.record_edit("t".into(), true, false);
        turn.record_flags("t".into(), vec!["skip-added".into()], false);
        turn.record_flags("u".into(), vec!["skip-added".into(), "assertion-removed".into()], false);
        turn.record_flags(String::new(), vec!["no-verify".into()], false);
        turn.catch_file("t");
        turn.subagents = 2;
        let e = turn_verification(&turn, base()).unwrap();
        assert_eq!(
            e.weakened_flags,
            Some(vec![
                "assertion-removed".into(),
                "no-verify".into(),
                "skip-added".into()
            ])
        );
        assert_eq!(e.weakened_caught, Some(1));
        assert_eq!(e.subagent_count, Some(2));
    }

    #[test]
    fn an_edit_with_no_file_in_the_repository_counts_as_an_edit_but_not_a_file() {
        let mut turn = TurnState::default();
        turn.record_edit(String::new(), false, false);
        let e = turn_verification(&turn, base()).unwrap();
        assert_eq!(e.edits_accepted, Some(1));
        assert_eq!(e.source_files_changed, Some(0));
    }
}
