//! Where published token rates come from, behind one interface.
//!
//! afi bills against its own [`Provider`] names and its own table format. Some
//! third party has to say what a token actually costs, and that party is not a
//! thing to build the accounting on directly: catalogues change shape, go away,
//! start charging, or turn out to be wrong about a provider that matters.
//!
//! So exactly one module in the tree knows the name, the address, and the wire
//! shape of whoever publishes the rates. Everything else - the refresh, the
//! cache, the layering, the host table - is written against [`Catalog`]. Adding
//! a second catalogue, or replacing this one, is a file in `catalog/` and a line
//! in [`active`]; nothing above has to be touched, and no key in
//! `assets/prices.json` moves.
//!
//! `scripts/project-prices.py` is the same projection, in the language the
//! vendoring step runs in, and carries the same warning at the top. The two are
//! held together by `the_vendored_table_and_the_provider_list_agree` rather than
//! by anyone remembering.

use std::collections::BTreeMap;
use std::fmt::Write;

use super::provider::Provider;

pub mod models_dev;

/// One model's rates, keyed by the class names `pricing::RATE_CLASSES` names.
///
/// The rate is the text the catalogue used rather than a float, because the
/// number is money and `rates::millionths` reads it from the digits. Parsing it
/// here and rendering it back would put a lossy step between the publisher and
/// the bill.
pub type ModelRates = BTreeMap<&'static str, String>;

/// What afi asked a catalogue for: every provider it can bill against, each
/// with the models that catalogue prices.
///
/// `BTreeMap` throughout so a projection is byte-identical run to run. A cache
/// that differed each time would make every comparison against it meaningless,
/// including the one the refresh throttle depends on.
pub type Projection = BTreeMap<Provider, BTreeMap<String, ModelRates>>;

/// A published catalogue of token rates.
///
/// The whole surface a replacement has to implement. Note what is *not* here:
/// nothing about caching, staleness, layering, or how afi stores a table. Those
/// are afi's, and a new catalogue does not get to have opinions about them.
pub trait Catalog: Send + Sync {
    /// What to call this catalogue in a message a person reads.
    fn name(&self) -> &'static str;

    /// Where the catalogue is fetched from.
    fn url(&self) -> &'static str;

    /// Narrow a fetched catalogue to the providers afi can bill against.
    ///
    /// Bytes rather than `&str`, because the caller has bytes off the wire and
    /// every deserializer here validates the UTF-8 itself - decoding first would
    /// be one more full pass and one more copy of several megabytes.
    ///
    /// `None` when the body is not this catalogue at all. An empty projection is
    /// a different answer and a legitimate one - a catalogue that priced nothing
    /// afi asked about - and it is the caller who decides that is not worth
    /// writing.
    fn project(&self, body: &[u8], wanted: &[Provider]) -> Option<Projection>;
}

/// The catalogue afi uses.
///
/// One line, on purpose. Swapping catalogues is meant to be this and a new file
/// in `catalog/`, with nothing above this module noticing.
#[must_use]
pub fn active() -> &'static dyn Catalog {
    &models_dev::ModelsDev
}

/// Render a projection as the table afi stores, sorted and one model per line.
///
/// Here rather than in a catalogue because the format is afi's: a catalogue says
/// what things cost, and afi says how it writes that down. Sharing this is what
/// lets the refresh and the vendoring step produce the same bytes.
#[must_use]
pub fn render(projection: &Projection, fetched: &str) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{{\n  \"fetched\": \"{fetched}\",\n  \"providers\": {{"
    );
    let last_provider = projection.len().saturating_sub(1);
    for (i, (provider, models)) in projection.iter().enumerate() {
        let _ = writeln!(out, "    \"{}\": {{", provider.key());
        let last_model = models.len().saturating_sub(1);
        for (j, (model, rates)) in models.iter().enumerate() {
            let body = rates
                .iter()
                .map(|(class, rate)| format!("\"{class}\": {rate}"))
                .collect::<Vec<_>>()
                .join(", ");
            let comma = if j == last_model { "" } else { "," };
            let _ = writeln!(out, "      {}: {{{body}}}{comma}", quoted(model));
        }
        out.push_str(if i == last_provider {
            "    }\n"
        } else {
            "    },\n"
        });
    }
    out.push_str("  }\n}\n");
    out
}

/// A model id as a JSON string. Ids are catalogue-supplied, so the escaping is
/// serde's rather than a pair of quotes and hope.
fn quoted(model: &str) -> String {
    serde_json::Value::String(model.to_string()).to_string()
}

#[cfg(test)]
mod tests;
