use std::collections::BTreeMap;

use super::{ModelRates, Projection, active, render};
use crate::pricing::provider::Provider;
use crate::pricing::table::Table;

fn rates(pairs: &[(&'static str, &str)]) -> ModelRates {
    pairs
        .iter()
        .map(|(class, rate)| (*class, (*rate).to_string()))
        .collect()
}

fn projection() -> Projection {
    let mut out = Projection::new();
    let mut anthropic = BTreeMap::new();
    anthropic.insert(
        "claude-sonnet-5".to_string(),
        rates(&[("input", "3"), ("output", "15")]),
    );
    anthropic.insert("claude-opus-5".to_string(), rates(&[("input", "15")]));
    out.insert(Provider::Anthropic, anthropic);
    let mut bedrock = BTreeMap::new();
    bedrock.insert("zai.glm-5".to_string(), rates(&[("input", "0.6")]));
    out.insert(Provider::Bedrock, bedrock);
    out
}

#[test]
fn a_rendered_table_reads_back_as_the_one_afi_bills_from() {
    // The writer and the parser are written apart and have to agree. This is the
    // round trip that says the cache afi writes is a cache afi can read - and
    // the same shape the vendored file is checked in as.
    let body = render(&projection(), "2026-08-08");
    let table: Table = serde_json::from_str(&body).expect("the render must parse");
    assert_eq!(table.fetched, "2026-08-08");
    assert_eq!(table.providers["anthropic"].len(), 2);
    assert_eq!(table.providers["bedrock"].len(), 1);
}

#[test]
fn a_rendered_table_is_one_line_per_model() {
    // So a rate that moved is a one-line diff in the pull request the refresh
    // workflow opens, rather than something nobody can read as money.
    let body = render(&projection(), "2026-08-08");
    assert!(
        body.contains(r#"      "claude-opus-5": {"input": 15}"#),
        "{body}"
    );
    assert!(
        body.contains(r#"      "claude-sonnet-5": {"input": 3, "output": 15}"#),
        "{body}"
    );
}

#[test]
fn rendering_twice_writes_the_same_bytes() {
    // A cache that differed run to run would make every comparison against it
    // meaningless, including the one the refresh throttle depends on.
    assert_eq!(
        render(&projection(), "2026-08-08"),
        render(&projection(), "2026-08-08")
    );
}

#[test]
fn a_model_id_with_something_awkward_in_it_still_renders_as_json() {
    // Ids come from whoever publishes the catalogue, so the escaping is serde's
    // rather than a pair of quotes and hope.
    let mut models = BTreeMap::new();
    models.insert(r#"weird"id\with"#.to_string(), rates(&[("input", "1")]));
    let mut out = Projection::new();
    out.insert(Provider::OpenAi, models);
    let body = render(&out, "2026-08-08");
    let table: Table = serde_json::from_str(&body).expect("an awkward id must still parse");
    assert!(table.providers["openai"].contains_key(r#"weird"id\with"#));
}

#[test]
fn the_active_catalogue_says_who_it_is() {
    // The one place a catalogue is named. A swap is this assertion changing and
    // nothing above `catalog` noticing.
    let catalog = active();
    assert_eq!(catalog.url(), "https://models.dev/api.json");
}
