#![allow(clippy::unwrap_used)]

use super::*;
use serde_json::json;

const REPORT: &str = "\\section{Introduction}\nWe searched OpenAlex for studies. Harness ships a fact check \
                      step that scores 41 of 45 claims in 2026. This is a short one.\n\n\\section{Results}\n\
                      Rust builds finished 2.5 times faster than the C++ baseline in the same harness run.\n";

#[test]
fn extract_keeps_numbers_and_drops_method_sentences() {
    let claims = extract(REPORT, "does rust build faster than C++");
    let texts = claims.iter().map(|c| c.text.as_str()).collect::<Vec<_>>();
    assert_eq!(texts.len(), 2, "{texts:?}");
    assert!(texts[0].contains("41 of 45"));
    assert!(texts[1].contains("2.5 times faster"));
    assert!(claims.iter().all(|c| !c.text.starts_with("We ")));
    assert_eq!(claims[0].location, "Introduction, sentence 2");
    assert_eq!(claims[1].location, "Results, sentence 1");
}

#[test]
fn extract_drops_claims_unrelated_to_the_question() {
    let report = "The cafeteria opened a new wing last spring. Cache misses fell by 30 percent with the new \
                  index.";
    let claims = extract(report, "how does the index change cache misses");
    assert_eq!(claims.len(), 1, "{claims:?}");
    assert!(claims[0].text.contains("30 percent"));
}

#[test]
fn extract_caps_the_claim_list() {
    let line = "The measured value grew by 4 percent in every one of the sampled runs. ";
    assert_eq!(extract(&line.repeat(60), "value growth").len(), CLAIM_MAX);
}

#[test]
fn record_dedupes_claims_and_requires_a_source_for_support() {
    let value = json!([
        {"text": "Cache misses fell by 30 percent.", "location": "Results", "verdict": "supported",
         "sources": [{"url": "https://example.org/a", "title": "A"}],
         "queries": ["cache misses 30 percent", "cache misses 30 percent", "cache", "extra"]},
        {"text": "cache misses fell by 30 percent", "location": "Results", "verdict": "notSupported"},
        {"text": "The sky is green today.", "verdict": "supported"},
        {"text": "No verdict here.", "verdict": "made up"}
    ]);
    let claims = record(Some(&value));
    assert_eq!(claims.len(), 3);
    assert_eq!(claims[0].queries.len(), QUERY_MAX);
    assert!(claims[1].text.starts_with("The sky is green"));
    assert_eq!(claims[1].verdict, "unclear");
    assert_eq!(claims[2].verdict, "unclear");
    let summary = score(&claims);
    assert_eq!(
        (
            summary.total,
            summary.supported,
            summary.not_supported,
            summary.unclear
        ),
        (3, 1, 0, 2)
    );
}

#[test]
fn normalise_query_ignores_case_and_punctuation() {
    assert_eq!(
        normalise_query("Cache  misses, fell!"),
        normalise_query("cache misses fell")
    );
}
