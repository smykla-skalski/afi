//! A client that will not spend once the run's budget has stopped it.
//!
//! The cap is enforced in the turn loop, which is where it has to be: only the
//! loop can consume a [`crate::cost::Verdict`] and act on the soft threshold by
//! putting the converge note into the conversation. But the loop is not the only
//! thing that can open a billed request - `/compress` is another, the
//! context-window probe is a third, and the next one has not been written yet.
//!
//! Gating each of those at its own call site works until somebody forgets. This
//! gates the one interface they all cross, so a new billed call is covered by
//! construction rather than by review. The loop still asks
//! [`crate::cost::checkpoint`] first and stops there; reaching here at all means
//! something bypassed it, which is exactly the case worth catching.
//!
//! Only the three methods that spend are gated. `list_models` and `get_props`
//! read metadata and cost nothing, and refusing them would break `/source` on a
//! run that had merely finished spending.

use async_trait::async_trait;
use serde_json::Value;

use super::{ChatClient, ChatCompletionStream, ClientError, StreamRequest};
use crate::config::Source;
use crate::cost;

/// Wraps any [`ChatClient`], refusing billed calls once the cap has fired.
#[derive(Debug, Clone, Default)]
pub struct Budgeted<C> {
    inner: C,
}

impl<C> Budgeted<C> {
    /// Put the run's budget in front of `inner`.
    pub const fn new(inner: C) -> Self {
        Self { inner }
    }
}

/// `Err` once the run has stopped spending, `Ok` while it may.
///
/// [`ClientError::Auth`] would be a lie and [`ClientError::Internal`] would call
/// a working cap a bug, so this is `Http`-free and classifies as
/// [`crate::summary::ErrorKind::Policy`]: the run was told what it may spend,
/// and honouring that is what refused the call. In practice the turn loop stops
/// first and returns success, so this text reaches a person only through a
/// command that spends outside the loop.
fn spendable() -> Result<(), ClientError> {
    if cost::may_spend() {
        return Ok(());
    }
    Err(ClientError::Budget(
        "this run has spent its budget and will not open another request".to_string(),
    ))
}

#[async_trait]
impl<C: ChatClient> ChatClient for Budgeted<C> {
    async fn chat_completions_stream(
        &self,
        req: StreamRequest<'_>,
    ) -> Result<ChatCompletionStream, ClientError> {
        spendable()?;
        self.inner.chat_completions_stream(req).await
    }

    async fn chat_completions(
        &self,
        source: &Source,
        model: &str,
        messages: &[Value],
        timeout: u64,
        extra_body: Option<&Value>,
    ) -> Result<String, ClientError> {
        spendable()?;
        self.inner
            .chat_completions(source, model, messages, timeout, extra_body)
            .await
    }

    async fn list_models(&self, source: &Source) -> Result<Value, ClientError> {
        self.inner.list_models(source).await
    }

    async fn get_props(&self, source: &Source) -> Result<Value, ClientError> {
        self.inner.get_props(source).await
    }

    async fn overrun_probe(&self, source: &Source, model: &str) -> Result<String, ClientError> {
        spendable()?;
        self.inner.overrun_probe(source, model).await
    }
}

#[cfg(test)]
mod tests;
