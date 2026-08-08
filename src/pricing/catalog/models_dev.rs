//! models.dev, and the only place in the tree that knows it exists.
//!
//! It publishes one JSON document covering every provider afi can reach, free
//! and without a credential, and its cost object happens to use the same five
//! class names afi bills. That last part is luck rather than design, so it is
//! still translated here rather than passed through: a catalogue that renamed
//! `cache_read` tomorrow would be a change to this file and to nothing else.
//!
//! Everything models.dev-shaped stops at this module boundary - the address, the
//! wire shape, and its names for the providers afi calls [`Provider`].

use std::collections::{BTreeMap, HashMap};

use serde::Deserialize;
use serde_json::{Map, Value};

use super::{Catalog, ModelRates, Projection};
use crate::pricing::RATE_CLASSES;
use crate::pricing::provider::Provider;

pub struct ModelsDev;

/// models.dev's name for a provider afi has its own name for.
///
/// Most match, which is exactly why the mapping has to be written down: an
/// accident of agreement is not a contract, and reading `Provider::key` straight
/// off this catalogue would silently couple afi's stored table to somebody
/// else's naming.
fn slug(provider: Provider) -> &'static str {
    match provider {
        Provider::Anthropic => "anthropic",
        Provider::Bedrock => "amazon-bedrock",
        Provider::Cerebras => "cerebras",
        Provider::DeepSeek => "deepseek",
        Provider::Fireworks => "fireworks-ai",
        Provider::Google => "google",
        Provider::Groq => "groq",
        Provider::Mistral => "mistral",
        Provider::OpenAi => "openai",
        Provider::OpenRouter => "openrouter",
        Provider::Together => "togetherai",
        Provider::XAi => "xai",
        Provider::ZAi => "zai",
        Provider::Zhipu => "zhipuai",
    }
}

impl Catalog for ModelsDev {
    fn name(&self) -> &'static str {
        "models.dev"
    }

    fn url(&self) -> &'static str {
        "https://models.dev/api.json"
    }

    fn project(&self, body: &[u8], wanted: &[Provider]) -> Option<Projection> {
        let catalogue: Catalogue = serde_json::from_slice(body).ok()?;
        let mut out = Projection::new();
        for provider in wanted {
            // Absent is not an error here: this catalogue may simply never
            // have carried a provider afi can reach. Losing one it *did* carry
            // is the dangerous case, and only `refresh::run` can see that,
            // because only it knows the table being replaced.
            let Some(entry) = catalogue.get(slug(*provider)) else {
                continue;
            };
            let models: BTreeMap<String, ModelRates> = entry
                .models
                .iter()
                .filter_map(|(id, model)| Some((id.clone(), rates_of(model.cost.as_ref()?)?)))
                .collect();
            if !models.is_empty() {
                out.insert(*provider, models);
            }
        }
        Some(out)
    }
}

/// The document, keyed by this catalogue's provider names.
///
/// Deserialized directly rather than through a wrapper with `#[serde(flatten)]`:
/// flatten cannot stream, so it buffers all 3.6 MB into an intermediate value
/// tree before a single field is read - measured at 19-23 ms and +22 MB against
/// 7-10 ms and +5 MB without it.
type Catalogue = HashMap<String, CatalogueProvider>;

/// Just the shape the projection needs. models.dev carries far more per model -
/// context windows, modalities, release dates - and none of it prices a token,
/// so everything else is ignored without allocating.
#[derive(Debug, Deserialize)]
struct CatalogueProvider {
    #[serde(default)]
    models: HashMap<String, CatalogueModel>,
}

#[derive(Debug, Deserialize)]
struct CatalogueModel {
    cost: Option<Map<String, Value>>,
}

/// One model's cost object, narrowed to the classes afi bills.
///
/// `None` without an input rate: `Rates::weighted` refuses to price a class that
/// was spent on and left unpriced, so half an entry would suppress the figure
/// for the whole run rather than for that model.
///
/// models.dev also carries `tiers`, `context_over_200k`, `input_audio` and
/// `output_audio`. Dropping them is not tidiness - afi's `RawRates` is
/// `deny_unknown_fields`, so one written through would refuse the entire table
/// the next time afi read it back.
fn rates_of(cost: &Map<String, Value>) -> Option<ModelRates> {
    if !cost.get("input").is_some_and(Value::is_number) {
        return None;
    }
    Some(
        RATE_CLASSES
            .iter()
            .filter_map(|class| {
                let number = cost.get(*class)?.as_number()?;
                Some((*class, number.to_string()))
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests;
