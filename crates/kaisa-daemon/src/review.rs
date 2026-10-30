//! Validating a reviewer's verdict (T21c, ADR-0030). A verdict arrives as JSON from an agent;
//! anything that does not validate is a failed review, never an approval.

use std::fmt;

use kaisa_domain::{Finding, RepoPath, ReviewVerdict, Severity};
use serde_json::Value;

/// Why a verdict was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerdictError {
    /// Not JSON, or a required field is missing or of the wrong type.
    Malformed(&'static str),
    /// It reviewed another candidate.
    WrongCandidate,
    /// It says "approve" yet lists a blocking finding.
    Contradictory,
}

impl fmt::Display for VerdictError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(what) => write!(f, "malformed verdict: {what}"),
            Self::WrongCandidate => f.write_str("the verdict is for another candidate"),
            Self::Contradictory => f.write_str("the verdict approves despite a blocking finding"),
        }
    }
}

impl std::error::Error for VerdictError {}

/// Parses `text` as a verdict on `tree`:
/// `{"candidate_tree": "...", "approve": bool, "findings": [{"severity": "blocking"|"advisory",
/// "path": "...", "line": n?, "message": "..."}]}`.
///
/// # Errors
///
/// Returns [`VerdictError`] for anything that is not a well-formed, consistent verdict on `tree`.
pub fn parse_verdict(text: &str, tree: &str) -> Result<ReviewVerdict, VerdictError> {
    let value: Value =
        serde_json::from_str(text).map_err(|_| VerdictError::Malformed("not JSON"))?;
    let candidate_tree: &str = value["candidate_tree"]
        .as_str()
        .ok_or(VerdictError::Malformed("candidate_tree"))?;
    if candidate_tree != tree {
        return Err(VerdictError::WrongCandidate);
    }
    let approve: bool = value["approve"]
        .as_bool()
        .ok_or(VerdictError::Malformed("approve"))?;
    let mut findings: Vec<Finding> = Vec::new();
    for item in value["findings"]
        .as_array()
        .ok_or(VerdictError::Malformed("findings"))?
    {
        let severity: Severity = item["severity"]
            .as_str()
            .and_then(Severity::from_code)
            .ok_or(VerdictError::Malformed("severity"))?;
        let path: RepoPath = item["path"]
            .as_str()
            .and_then(|path| RepoPath::try_new(path).ok())
            .ok_or(VerdictError::Malformed("path"))?;
        let line: Option<u32> = match &item["line"] {
            Value::Null => None,
            other => Some(
                other
                    .as_u64()
                    .and_then(|line| u32::try_from(line).ok())
                    .ok_or(VerdictError::Malformed("line"))?,
            ),
        };
        let message: String = item["message"]
            .as_str()
            .ok_or(VerdictError::Malformed("message"))?
            .to_owned();
        findings.push(Finding {
            severity,
            path,
            line,
            message,
        });
    }
    let verdict = ReviewVerdict {
        candidate_tree: candidate_tree.to_owned(),
        findings,
    };
    if approve != verdict.approves(tree) {
        return Err(VerdictError::Contradictory);
    }
    Ok(verdict)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{VerdictError, parse_verdict};

    /// The tree of the candidate under review.
    const REVIEWED_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
    /// Another candidate's tree.
    const OTHER_TREE: &str = "9daeafb9864cf43055ae93beb0afd6c7d144bfa4";

    fn a_verdict(tree: &str, approve: bool, severity: &str) -> String {
        json!({
            "candidate_tree": tree,
            "approve": approve,
            "findings": [{
                "severity": severity,
                "path": "src/auth/session.rs",
                "line": 12,
                "message": "the refresh token is never rotated",
            }],
        })
        .to_string()
    }

    #[test]
    fn a_malformed_review_verdict_never_counts_as_approval() {
        let prose = "Looks good to me, ship it!";

        let parsed = parse_verdict(prose, REVIEWED_TREE);

        assert_eq!(
            parsed,
            Err(VerdictError::Malformed("not JSON")),
            "free text is not a verdict, whatever it says"
        );
    }

    #[test]
    fn a_verdict_on_another_candidate_is_refused() {
        let stale = a_verdict(OTHER_TREE, true, "advisory");

        let parsed = parse_verdict(&stale, REVIEWED_TREE);

        assert_eq!(
            parsed,
            Err(VerdictError::WrongCandidate),
            "a review of other code says nothing about this candidate"
        );
    }

    #[test]
    fn an_approval_with_a_blocking_finding_is_refused() {
        let contradictory = a_verdict(REVIEWED_TREE, true, "blocking");

        let parsed = parse_verdict(&contradictory, REVIEWED_TREE);

        assert_eq!(
            parsed,
            Err(VerdictError::Contradictory),
            "an approval must not hide a blocking finding"
        );
    }

    #[test]
    fn a_blocking_verdict_is_parsed_with_the_finding_a_revision_must_fix() {
        let blocking = a_verdict(REVIEWED_TREE, false, "blocking");

        let verdict = parse_verdict(&blocking, REVIEWED_TREE).expect("a valid verdict");

        assert_eq!(
            (verdict.approves(REVIEWED_TREE), verdict.blocking().len()),
            (false, 1),
            "the blocking finding is kept for the revision pack"
        );
    }

    #[test]
    fn an_advisory_only_verdict_approves_its_candidate() {
        let advisory = a_verdict(REVIEWED_TREE, true, "advisory");

        let verdict = parse_verdict(&advisory, REVIEWED_TREE).expect("a valid verdict");

        assert!(
            verdict.approves(REVIEWED_TREE) && !verdict.approves(OTHER_TREE),
            "an approval holds for its own candidate tree only"
        );
    }
}
