//! Deterministic lexical knowledge retrieval; vector retrieval remains behind its evidence gate.

use std::collections::{BTreeMap, BTreeSet};

use kaisa_domain::{ContentDigest, KnowledgeKind, RepoPath};

/// Standard BM25 term-frequency saturation parameter.
pub const K1: f64 = 1.2;
/// Standard BM25 document-length normalization parameter.
pub const B: f64 = 0.75;
/// Minimum token length in Unicode characters; single-character fragments are discarded.
pub const MIN_TOKEN_CHARS: usize = 2;
/// Multiplier for a term-matching item whose scope overlaps a task path at a directory boundary.
/// Applied once regardless of how many scope paths overlap.
pub const SCOPE_MATCH_BOOST: f64 = 1.5;

/// A lexical match, ranked by descending score and then ascending content digest.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    /// Content address of the matching knowledge item.
    pub id: ContentDigest,
    /// Positive BM25 score, including any scope boost.
    pub score: f64,
}

/// An in-memory BM25 index over lessons and other knowledge, with no vector dependencies.
///
/// Callers must select knowledge whose provenance permits retrieval. All kinds are ranked
/// alike; neither a kind nor a scope makes an item relevant without a shared text token.
#[derive(Debug, Clone, Default)]
pub struct LessonIndex {
    documents: BTreeMap<ContentDigest, Document>,
}

#[derive(Debug, Clone)]
struct Document {
    _kind: KnowledgeKind,
    terms: BTreeMap<String, usize>,
    length: usize,
    scope: Vec<RepoPath>,
}

impl LessonIndex {
    /// Creates an empty lexical index.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds an item, replacing any previous text, kind and scope for the same digest.
    /// Scope paths affect ranking only; they are not added to the searchable text.
    pub fn add(&mut self, id: ContentDigest, kind: KnowledgeKind, text: &str, scope: &[RepoPath]) {
        let tokens = tokenize(text);
        let length = tokens.len();
        let mut terms = BTreeMap::new();
        for token in tokens {
            *terms.entry(token).or_default() += 1;
        }
        self.documents.insert(
            id,
            Document {
                _kind: kind,
                terms,
                length,
                scope: scope.to_vec(),
            },
        );
    }

    /// Returns at most `limit` term matches in descending BM25 order, breaking ties by digest.
    /// Repeated query tokens count once. Empty queries, empty indexes and zero limits return
    /// no hits. Scope overlap means equality or a whole-directory prefix in either direction.
    #[must_use]
    pub fn search(&self, query: &str, scope_hint: &[RepoPath], limit: usize) -> Vec<Hit> {
        if limit == 0 || self.documents.is_empty() {
            return Vec::new();
        }
        let query_terms: BTreeSet<_> = tokenize(query).into_iter().collect();
        if query_terms.is_empty() {
            return Vec::new();
        }
        let document_count = self.documents.len() as f64;
        let total_length: usize = self
            .documents
            .values()
            .map(|document| document.length)
            .sum();
        if total_length == 0 {
            return Vec::new();
        }
        let average_length = total_length as f64 / document_count;
        let inverse_frequencies: Vec<_> = query_terms
            .iter()
            .map(|term| {
                let frequency = self
                    .documents
                    .values()
                    .filter(|document| document.terms.contains_key(term))
                    .count() as f64;
                // The positive Robertson IDF variant keeps even ubiquitous matches positive.
                let idf = (1.0 + (document_count - frequency + 0.5) / (frequency + 0.5)).ln();
                (term, idf)
            })
            .collect();
        let mut hits = Vec::new();
        for (id, document) in &self.documents {
            let normalization = K1 * (1.0 - B + B * document.length as f64 / average_length);
            let mut score = 0.0;
            for (term, idf) in &inverse_frequencies {
                if let Some(frequency) = document.terms.get(*term) {
                    let frequency = *frequency as f64;
                    score += idf * frequency * (K1 + 1.0) / (frequency + normalization);
                }
            }
            if score > 0.0 {
                if document
                    .scope
                    .iter()
                    .any(|path| scope_hint.iter().any(|hint| paths_overlap(path, hint)))
                {
                    score *= SCOPE_MATCH_BOOST;
                }
                hits.push(Hit {
                    id: id.clone(),
                    score,
                });
            }
        }
        hits.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.id.cmp(&right.id))
        });
        hits.truncate(limit);
        hits
    }
}

fn paths_overlap(left: &RepoPath, right: &RepoPath) -> bool {
    fn directory_prefix(parent: &str, child: &str) -> bool {
        child
            .strip_prefix(parent)
            .is_some_and(|suffix| suffix.starts_with('/'))
    }
    left == right
        || directory_prefix(left.as_str(), right.as_str())
        || directory_prefix(right.as_str(), left.as_str())
}

fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    for word in text.split(|character: char| !character.is_alphanumeric()) {
        let characters: Vec<_> = word.char_indices().collect();
        let mut start = 0;
        for position in 1..characters.len() {
            let (offset, current) = characters[position];
            let previous = characters[position - 1].1;
            let next_is_lowercase = characters
                .get(position + 1)
                .is_some_and(|(_, next)| next.is_lowercase());
            if current.is_uppercase()
                && (previous.is_lowercase()
                    || previous.is_numeric()
                    || (previous.is_uppercase() && next_is_lowercase))
            {
                push_token(&word[start..offset], &mut tokens);
                start = offset;
            }
        }
        push_token(&word[start..], &mut tokens);
    }
    tokens
}

fn push_token(part: &str, tokens: &mut Vec<String>) {
    if part.chars().count() >= MIN_TOKEN_CHARS {
        tokens.push(part.to_lowercase());
    }
}

#[cfg(test)]
mod tests {
    use kaisa_domain::{ContentDigest, DIGEST_HEX_LEN, KnowledgeKind, RepoPath};

    use super::{LessonIndex, MIN_TOKEN_CHARS, SCOPE_MATCH_BOOST, tokenize};

    const HIT_LIMIT: usize = 10;
    const FIRST_HIT: usize = 0;
    const SECOND_HIT: usize = 1;
    const SINGLE_HIT: usize = 1;
    const NO_HITS: usize = 0;
    const SESSION_MODULE: &str = "src/auth/session.rs";
    const SESSION_DIRECTORY: &str = "src/auth";

    fn digest(hex: char) -> ContentDigest {
        ContentDigest::try_new(&hex.to_string().repeat(DIGEST_HEX_LEN))
            .expect("a repeated lowercase hex digit is a content address")
    }

    fn path(text: &str) -> RepoPath {
        RepoPath::try_new(text).expect("the fixture stays inside the repository")
    }

    #[test]
    fn the_lesson_about_this_module_is_retrieved_for_a_task_on_it() {
        let mut lessons = LessonIndex::new();
        let session_lesson = digest('a');
        lessons.add(
            session_lesson.clone(),
            KnowledgeKind::Lesson,
            "Session refreshToken failures require token rotation",
            &[path(SESSION_DIRECTORY)],
        );
        lessons.add(
            digest('b'),
            KnowledgeKind::Lesson,
            "Invoice rounding uses decimal arithmetic",
            &[],
        );

        let hits = lessons.search(
            "repair session refresh_token rotation",
            &[path(SESSION_MODULE)],
            HIT_LIMIT,
        );

        assert_eq!(
            hits.len(),
            SINGLE_HIT,
            "only the session lesson shares task terms"
        );
        assert_eq!(
            hits[FIRST_HIT].id, session_lesson,
            "the task receives the module's relevant lesson"
        );
    }

    #[test]
    fn a_lesson_with_no_shared_term_is_never_returned() {
        let mut lessons = LessonIndex::new();
        lessons.add(
            digest('a'),
            KnowledgeKind::Lesson,
            "invoice rounding",
            &[path(SESSION_DIRECTORY)],
        );

        assert!(
            lessons
                .search("session token", &[path(SESSION_MODULE)], HIT_LIMIT)
                .is_empty(),
            "even a matching scope cannot manufacture lexical relevance"
        );
    }

    #[test]
    fn identifier_parts_match_across_naming_styles() {
        for identifier in [
            "refreshToken",
            "refresh_token",
            "RefreshToken",
            "REFRESH_TOKEN",
        ] {
            let mut lessons = LessonIndex::new();
            lessons.add(digest('a'), KnowledgeKind::Lesson, identifier, &[]);
            for query in ["refresh", "token", "refreshToken", "refresh_token"] {
                assert_eq!(
                    lessons.search(query, &[], HIT_LIMIT).len(),
                    SINGLE_HIT,
                    "{query} must find the shared identifier parts in {identifier}"
                );
            }
        }
        assert_eq!(
            tokenize("HTTPServer ÉcoleToken 한국어_검색"),
            vec!["http", "server", "école", "token", "한국어", "검색"],
            "acronyms and Unicode identifiers share the same boundary rules"
        );
    }

    #[test]
    fn the_same_query_always_ranks_the_same_way() {
        let mut lessons = LessonIndex::new();
        let mut reversed = LessonIndex::new();
        for hex in ['c', 'a', 'b'] {
            lessons.add(digest(hex), KnowledgeKind::Lesson, "session token", &[]);
        }
        for hex in ['b', 'a', 'c'] {
            reversed.add(digest(hex), KnowledgeKind::Lesson, "session token", &[]);
        }
        let expected: Vec<_> = ['a', 'b', 'c'].into_iter().map(digest).collect();
        let hits = lessons.search("token session", &[], HIT_LIMIT);

        assert_eq!(
            hits,
            lessons.search("token session", &[], HIT_LIMIT),
            "repeated searches preserve scores and order"
        );
        assert_eq!(
            hits,
            reversed.search("session token token", &[], HIT_LIMIT),
            "insertion and query term order cannot alter ranking"
        );
        assert_eq!(
            hits.iter().map(|hit| hit.id.clone()).collect::<Vec<_>>(),
            expected,
            "equal BM25 scores use ascending digest order"
        );
    }

    #[test]
    fn a_lesson_scoped_to_the_tasks_files_ranks_above_an_equal_unscoped_one() {
        for scope in [SESSION_DIRECTORY, SESSION_MODULE] {
            let mut lessons = LessonIndex::new();
            let scoped_lesson = digest('b');
            lessons.add(digest('a'), KnowledgeKind::Lesson, "session token", &[]);
            lessons.add(
                scoped_lesson.clone(),
                KnowledgeKind::Lesson,
                "session token",
                &[path(scope)],
            );

            let hits = lessons.search("session", &[path(SESSION_MODULE)], HIT_LIMIT);

            assert_eq!(
                hits[FIRST_HIT].id, scoped_lesson,
                "a file or directory scope outranks an equal unscoped match"
            );
            assert_eq!(
                hits[FIRST_HIT].score,
                hits[SECOND_HIT].score * SCOPE_MATCH_BOOST,
                "scope applies the documented multiplier exactly once"
            );
        }
    }

    #[test]
    fn a_path_prefix_inside_a_directory_name_does_not_boost_a_lesson() {
        let mut lessons = LessonIndex::new();
        lessons.add(digest('a'), KnowledgeKind::Lesson, "session token", &[]);
        lessons.add(
            digest('b'),
            KnowledgeKind::Lesson,
            "session token",
            &[path(SESSION_DIRECTORY)],
        );

        let hits = lessons.search(
            "session",
            &[path("src/authorization/session.rs")],
            HIT_LIMIT,
        );

        assert_eq!(
            hits[FIRST_HIT].score, hits[SECOND_HIT].score,
            "a shared string prefix is not an overlapping repository directory"
        );
    }

    #[test]
    fn replacing_a_lesson_removes_its_old_terms_and_scope() {
        let mut lessons = LessonIndex::new();
        let lesson = digest('a');
        lessons.add(
            lesson.clone(),
            KnowledgeKind::Lesson,
            "session token",
            &[path(SESSION_DIRECTORY)],
        );
        lessons.add(
            lesson.clone(),
            KnowledgeKind::Decision,
            "invoice decimal",
            &[],
        );

        assert!(
            lessons
                .search("session", &[path(SESSION_MODULE)], HIT_LIMIT)
                .is_empty(),
            "replacement must remove stale terms"
        );
        let hits = lessons.search("invoice", &[], HIT_LIMIT);
        assert_eq!(
            hits.len(),
            SINGLE_HIT,
            "replacement keeps exactly one item per digest"
        );
        assert_eq!(
            hits[FIRST_HIT].id, lesson,
            "decisions and lessons are both searchable knowledge"
        );
        assert_eq!(
            hits,
            lessons.search("invoice", &[path(SESSION_MODULE)], HIT_LIMIT),
            "replacement removes the former scope boost"
        );
    }

    #[test]
    fn empty_inputs_and_zero_limits_return_no_hits() {
        let mut lessons = LessonIndex::new();
        assert!(
            lessons.search("session", &[], HIT_LIMIT).is_empty(),
            "an empty index has no matches"
        );
        lessons.add(digest('a'), KnowledgeKind::Lesson, "", &[]);
        assert!(
            lessons.search("session", &[], HIT_LIMIT).is_empty(),
            "tokenless documents cannot match"
        );
        lessons.add(digest('b'), KnowledgeKind::Lesson, "session", &[]);
        for query in ["", "_-!", "a I"] {
            assert!(
                lessons.search(query, &[], HIT_LIMIT).is_empty(),
                "{query:?} has no searchable terms"
            );
        }
        assert!(
            lessons.search("session", &[], NO_HITS).is_empty(),
            "a zero limit suppresses all matches"
        );
        assert!(
            tokenize(&"a".repeat(MIN_TOKEN_CHARS - 1)).is_empty(),
            "tokens below the named character minimum are dropped"
        );
        assert_eq!(
            lessons.search("session", &[], SINGLE_HIT).len(),
            SINGLE_HIT,
            "a positive limit returns available matches"
        );
    }

    #[test]
    fn a_rare_query_term_ranks_above_an_equally_frequent_common_term() {
        let mut lessons = LessonIndex::new();
        let rare_lesson = digest('b');
        lessons.add(digest('a'), KnowledgeKind::Lesson, "session cache", &[]);
        lessons.add(
            rare_lesson.clone(),
            KnowledgeKind::Lesson,
            "rotation cache",
            &[],
        );
        lessons.add(digest('c'), KnowledgeKind::Lesson, "session cache", &[]);

        let hits = lessons.search("session rotation", &[], SINGLE_HIT);

        assert_eq!(
            hits.len(),
            SINGLE_HIT,
            "the limit truncates a larger set of lexical matches"
        );
        assert_eq!(
            hits[FIRST_HIT].id, rare_lesson,
            "inverse document frequency favors the rarer task term"
        );
    }

    #[test]
    fn a_shorter_lesson_ranks_above_a_longer_one_with_the_same_term_frequency() {
        let mut lessons = LessonIndex::new();
        let concise_lesson = digest('b');
        lessons.add(
            digest('a'),
            KnowledgeKind::Lesson,
            "session invoice rounding decimal arithmetic",
            &[],
        );
        lessons.add(
            concise_lesson.clone(),
            KnowledgeKind::Lesson,
            "session",
            &[],
        );

        let hits = lessons.search("session", &[], HIT_LIMIT);

        assert_eq!(
            hits[FIRST_HIT].id, concise_lesson,
            "BM25 length normalization favors the focused lesson"
        );
    }
}
