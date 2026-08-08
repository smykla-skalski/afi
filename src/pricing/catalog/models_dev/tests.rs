use super::{ModelsDev, slug};
use crate::pricing::catalog::Catalog;
use crate::pricing::provider::{ALL, Provider};

/// A slice of models.dev, in the shape the real one has - including the keys
/// afi does not model and the half-priced entries it has to drop.
const CATALOGUE: &str = r#"{
  "anthropic": {"models": {
    "claude-sonnet-5": {"cost": {"input": 3, "output": 15, "cache_read": 0.3, "cache_write": 3.75}},
    "claude-unpriced": {"cost": {"output": 15}},
    "claude-free": {}
  }},
  "openai": {"models": {
    "gpt-5": {"cost": {"input": 1.25, "output": 10, "cache_read": 0.125, "tiers": [{"input": 2}]}}
  }},
  "amazon-bedrock": {"models": {
    "zai.glm-5": {"cost": {"input": 0.6, "output": 2.2}}
  }},
  "some-provider-afi-cannot-reach": {"models": {
    "whatever": {"cost": {"input": 999, "output": 999}}
  }}
}"#;

#[test]
fn the_projection_keeps_only_what_afi_can_bill() {
    let projected = ModelsDev
        .project(CATALOGUE, &ALL)
        .expect("the slice must project");
    assert_eq!(
        projected.keys().copied().collect::<Vec<_>>(),
        vec![Provider::Anthropic, Provider::Bedrock, Provider::OpenAi],
        "a provider no source resolves to is weight nothing can read"
    );

    let anthropic = &projected[&Provider::Anthropic];
    assert_eq!(anthropic["claude-sonnet-5"]["input"], "3");
    assert_eq!(anthropic["claude-sonnet-5"]["cache_write"], "3.75");
    assert!(
        !anthropic.contains_key("claude-unpriced"),
        "no input rate is no rate at all - half an entry suppresses the whole run's figure"
    );
    assert!(!anthropic.contains_key("claude-free"), "no cost, no entry");
}

#[test]
fn a_class_afi_does_not_model_is_dropped_rather_than_carried() {
    // `RawRates` is `deny_unknown_fields`, so a `tiers` key written into the
    // cache would refuse the whole table the next time afi read it back - the
    // rates for every model, gone, over a key afi never wanted.
    let projected = ModelsDev.project(CATALOGUE, &ALL).expect("must project");
    let kept: Vec<&str> = projected[&Provider::OpenAi]["gpt-5"]
        .keys()
        .copied()
        .collect();
    assert_eq!(kept, ["cache_read", "input", "output"]);
}

#[test]
fn the_catalogue_is_asked_for_afi_s_providers_under_its_own_names() {
    // The whole point of the mapping. afi calls it `bedrock`, this catalogue
    // calls it `amazon-bedrock`, and the stored table uses afi's name - so
    // replacing the catalogue never moves a key in `assets/prices.json`.
    assert_eq!(slug(Provider::Bedrock), "amazon-bedrock");
    assert_eq!(Provider::Bedrock.key(), "bedrock");
    let projected = ModelsDev.project(CATALOGUE, &ALL).expect("must project");
    assert!(projected.contains_key(&Provider::Bedrock));
}

#[test]
fn a_body_that_is_not_this_catalogue_projects_nothing() {
    assert!(ModelsDev.project("not json", &ALL).is_none());
    // Valid JSON of the wrong shape reads as a catalogue that priced nothing,
    // which the caller refuses to write over a working table.
    assert_eq!(
        ModelsDev
            .project(r#"{"anthropic": {}}"#, &ALL)
            .map(|p| p.len()),
        Some(0)
    );
}

#[test]
fn asking_for_fewer_providers_returns_fewer() {
    let projected = ModelsDev
        .project(CATALOGUE, &[Provider::OpenAi])
        .expect("must project");
    assert_eq!(
        projected.keys().copied().collect::<Vec<_>>(),
        vec![Provider::OpenAi]
    );
}

#[test]
fn every_provider_afi_bills_has_a_name_here() {
    // A `Provider` this catalogue cannot name would be silently unpriced, and
    // the `match` in `slug` is what makes adding one a compile error instead.
    for provider in ALL {
        assert!(!slug(provider).is_empty(), "{provider:?}");
    }
}
