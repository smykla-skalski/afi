//! The version of the summary shape, and the rule for moving it.
//!
//! Split from `summary.rs` so the number and the rule for changing it stay
//! together: a bump made without the rule in front of it is how a version key
//! stops meaning anything.

/// The shape of the object [`super::RunSummary::to_json`] renders.
///
/// A consumer has one question to answer before it can act on a field it did
/// not find: is this an afi too old to report it, or an afi that reports it and
/// a run that went wrong? Without a version the answer comes from probing for a
/// field some release added and inferring the build from whether it is there -
/// an inference that reads a renamed field as an absent one, that has to be
/// rewritten every time the shape grows, and that lives in the consumer as a
/// comment nobody can check. Absent now means an afi older than this key, the
/// one version-free shape there will ever be.
///
/// Every summary carries it: on stdout and in the file, from a run that
/// finished and from one refused before it started.
///
/// Bump it when a summary a working consumer could read stops being readable: a
/// key removed, a key renamed, a type changed, or a meaning that moved under a
/// name that stayed. Adding a key is none of those and does not bump - consumers
/// are expected to ignore what they do not know, and every field the summary has
/// grown so far arrived that way. `docs/reference.md` publishes the shape and
/// moves with the number; `summary::tests::version` pins the shape the number
/// currently stands for, so growing one without the other fails the build.
///
/// A number rather than a string, and meant to be read as "at least what I
/// know" rather than matched: a consumer that demands an exact version breaks on
/// an upgrade that only added fields it never reads. That forward compatibility
/// is what separates this from the session tag (`"schema": "afi-1"` - see
/// [`crate::sessions`]), which afi is the only reader of and can therefore gate
/// on exactly. This contract is read by CI that afi does not ship.
pub const SCHEMA_VERSION: u64 = 1;
