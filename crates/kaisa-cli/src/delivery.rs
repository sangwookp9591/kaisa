//! What `kaisa` tells the user, and its exit code, for a delivery decision (T22b; CI contract,
//! architecture §ADR-0026, user-flows F26).

use kaisa_daemon::Delivery;

/// Exit status of a run waiting for a person (CI contract code 3).
pub const EXIT_WAITING_FOR_HUMAN: u8 = 3;

/// One line for the user and the exit status for scripts.
#[must_use]
pub fn report_delivery(delivery: &Delivery) -> (String, u8) {
    match delivery {
        Delivery::Deliver { reduced: false } => (
            "verified: trusted checks passed and the review approved".to_owned(),
            crate::EXIT_SUCCESS,
        ),
        Delivery::Deliver { reduced: true } => (
            "verified (reduced: a check ran without OS confinement)".to_owned(),
            crate::EXIT_SUCCESS,
        ),
        Delivery::NeedsPerson(reason) => {
            (format!("waiting for you: {reason}"), EXIT_WAITING_FOR_HUMAN)
        }
        Delivery::Revise(reasons) => (
            format!(
                "not delivered: {} problem(s) remain after the revisions allowed",
                reasons.len()
            ),
            crate::EXIT_RUN_FAILED,
        ),
        Delivery::Refuse(reason) => (format!("not delivered: {reason}"), crate::EXIT_RUN_FAILED),
    }
}

#[cfg(test)]
mod tests {
    use kaisa_daemon::Delivery;

    use super::{EXIT_WAITING_FOR_HUMAN, report_delivery};
    use crate::{EXIT_RUN_FAILED, EXIT_SUCCESS};

    #[test]
    fn kaisa_reports_a_verified_result_with_exit_zero() {
        let (line, status) = report_delivery(&Delivery::Deliver { reduced: false });

        assert_eq!(
            status, EXIT_SUCCESS,
            "CI treats a verified delivery as success"
        );
        assert!(
            line.starts_with("verified"),
            "the user reads that it was verified: {line}"
        );
    }

    #[test]
    fn a_decision_waiting_for_a_person_exits_three_and_says_why() {
        let (line, status) = report_delivery(&Delivery::NeedsPerson(
            "the candidate changed what its checks run: package.json".to_owned(),
        ));

        assert_eq!(
            status, EXIT_WAITING_FOR_HUMAN,
            "CI must not treat it as done or failed"
        );
        assert!(line.contains("package.json"), "the reason is shown: {line}");
    }

    #[test]
    fn a_refused_candidate_exits_as_failed() {
        let (line, status) = report_delivery(&Delivery::Refuse(
            "evidence was produced on other content".to_owned(),
        ));

        assert_eq!(status, EXIT_RUN_FAILED, "nothing reached the user");
        assert!(
            line.starts_with("not delivered"),
            "the user is told plainly: {line}"
        );
    }

    #[test]
    fn a_reduced_delivery_is_still_success_but_says_so() {
        let (line, status) = report_delivery(&Delivery::Deliver { reduced: true });

        assert_eq!(status, EXIT_SUCCESS, "the result was verified");
        assert!(
            line.contains("reduced"),
            "the user learns the checks were not confined: {line}"
        );
    }
}
