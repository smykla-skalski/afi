//! A run's failure state: whether anything failed, and what to report for it.
//!
//! Sticky across turns, because one process is one run. A piped session keeps
//! reading input after a turn dies, so the exit code and the summary have to
//! reflect the whole session rather than only its last turn.

use crate::model::TurnOutcome;
use crate::summary::RunError;

/// Whether any turn of a session failed, and how the first one did.
///
/// One field rather than a flag beside a reason: a failed run that could not say
/// why would be reported as a failure with a null `error`, which is the shape this
/// exists to prevent.
#[derive(Debug, Default)]
pub(crate) struct RunFailure {
    error: Option<RunError>,
}

impl RunFailure {
    /// Fold one turn's outcome in. Turns that did not fail change nothing.
    pub(crate) fn record(&mut self, outcome: &TurnOutcome) {
        if let Some(error) = outcome.error() {
            self.record_error(error);
        }
    }

    /// Record a failure raised outside a turn: an input the session could not
    /// read, or a turn with no source to run against.
    pub(crate) fn record_error(&mut self, error: RunError) {
        // First one wins. An auth failure repeats on every later turn, and the
        // reason the run went wrong is the first thing that went wrong.
        self.error.get_or_insert(error);
    }

    /// The failure as the summary reports it, or `None` for a clean run.
    pub(crate) fn error(&self) -> Option<&RunError> {
        self.error.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TURN_DONE;
    use crate::summary::ErrorKind;

    #[test]
    fn a_clean_session_reports_nothing() {
        let mut failure = RunFailure::default();
        failure.record(&TurnOutcome::new(TURN_DONE));
        assert!(failure.error().is_none());
    }

    #[test]
    fn the_first_failure_is_the_one_reported() {
        // A later provider hiccup must not relabel a run that died on a rejected
        // credential: retrying that one cannot help, and the field is what a
        // caller decides on.
        let mut failure = RunFailure::default();
        failure.record(&TurnOutcome::failed(RunError::new(
            "HTTP 401: authentication_error",
            ErrorKind::Auth,
        )));
        failure.record(&TurnOutcome::failed(RunError::new(
            "HTTP 429: rate_limit_error",
            ErrorKind::ProviderHttp,
        )));
        let error = failure.error().expect("the run failed");
        assert_eq!(error.kind, ErrorKind::Auth);
        assert_eq!(error.message, "HTTP 401: authentication_error");
    }
}
