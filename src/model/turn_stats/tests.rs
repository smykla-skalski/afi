use super::*;
use crate::risk::ApprovalChoice;
use crate::term::OutputEvent;
use tokio_util::sync::CancellationToken;

/// Collects the messages the handler emits, so a test can assert the sentence a
/// run reports rather than only its status.
#[derive(Default)]
struct TestUi {
    messages: Vec<String>,
}

impl UserInterface for TestUi {
    fn emit(&mut self, event: OutputEvent) {
        if let OutputEvent::Message { text, .. } = event {
            self.messages.push(text);
        }
    }

    fn start_activity(&mut self, _label: &str) -> CancellationToken {
        CancellationToken::new()
    }

    fn stop_activity(&mut self) {}

    fn approve(&mut self, _prompt: &str) -> ApprovalChoice {
        ApprovalChoice::Yes
    }
}

fn config(retry_limit: u32) -> ModelConfig {
    ModelConfig {
        reasoning_only_retry_limit: retry_limit,
        ..ModelConfig::default()
    }
}

/// The stall handler with no history to nudge, which is all these cases need.
fn stall(cut_count: u32, forced_final: bool, retry_limit: u32) -> (TurnOutcome, Vec<String>) {
    let mut messages = Vec::new();
    let mut ui = TestUi::default();
    let outcome = handle_reasoning_stall(
        &mut messages,
        &config(retry_limit),
        cut_count,
        40_000,
        &[],
        forced_final,
        &mut ui,
    );
    (outcome, ui.messages)
}

#[test]
fn a_forced_final_lost_to_reasoning_fails_the_turn() {
    // The model spent 40k characters in its scratchpad and answered nothing. This
    // reported TURN_DONE, so a run with nothing to say exited 0.
    let (outcome, messages) = stall(0, true, 3);
    let error = outcome.error().expect("the turn must fail");
    assert_eq!(error.kind, ErrorKind::NoAnswer);
    assert!(error.message.contains("FORCED FINAL FAILED"), "{error:?}");
    // The same sentence, so the log and the summary agree.
    assert_eq!(messages, vec![error.message]);
}

#[test]
fn giving_up_on_the_stall_rescue_fails_the_turn() {
    let (outcome, _) = stall(3, false, 3);
    let error = outcome.error().expect("the turn must fail");
    assert_eq!(error.kind, ErrorKind::NoAnswer);
    assert!(error.message.contains("RESCUE FAILED"), "{}", error.message);
}

#[test]
fn a_stall_with_rescues_left_is_not_a_failure() {
    // Both of these go back to the model, so neither may fail the run: the nudge
    // and the forced final are the rescue working as intended.
    for cut_count in [0, 2] {
        let (outcome, _) = stall(cut_count, false, 3);
        assert_eq!(outcome.status, TURN_FORCE_FINAL, "cut {cut_count}");
        assert!(!outcome.is_failure(), "cut {cut_count}");
        assert!(outcome.error().is_none(), "cut {cut_count}");
    }
}
