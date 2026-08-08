//! That the wrapper refuses exactly the calls that spend, and nothing else.
//!
//! Driven through a counting stub rather than a real endpoint: what matters is
//! whether the inner client was *reached*, which is the whole promise. A test
//! that asserted on a returned error could pass while the request still went
//! out.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::Value;

use super::Budgeted;
use crate::config::budget::resolve_budget;
use crate::model::client::{ChatClient, ChatCompletionStream, ClientError, StreamRequest};
use crate::model::stream::NormalizedUsage;
use crate::model::usage_totals;
use crate::pricing::Pricing;
use crate::summary::ErrorKind;
use crate::{config, cost};

/// Counts every call that reached it. Every method errors, so nothing here
/// depends on a response shape.
#[derive(Default)]
struct Counting {
    reached: AtomicUsize,
}

impl Counting {
    fn count(&self) -> usize {
        self.reached.load(Ordering::Relaxed)
    }

    fn hit<T>(&self) -> Result<T, ClientError> {
        self.reached.fetch_add(1, Ordering::Relaxed);
        Err(ClientError::Connection("stub".to_string()))
    }
}

#[async_trait::async_trait]
impl ChatClient for Counting {
    async fn chat_completions_stream(
        &self,
        _req: StreamRequest<'_>,
    ) -> Result<ChatCompletionStream, ClientError> {
        self.hit()
    }

    async fn chat_completions(
        &self,
        _source: &config::Source,
        _model: &str,
        _messages: &[Value],
        _timeout: u64,
        _extra_body: Option<&Value>,
    ) -> Result<String, ClientError> {
        self.hit()
    }

    async fn list_models(&self, _source: &config::Source) -> Result<Value, ClientError> {
        self.hit()
    }

    async fn get_props(&self, _source: &config::Source) -> Result<Value, ClientError> {
        self.hit()
    }

    async fn overrun_probe(
        &self,
        _source: &config::Source,
        _model: &str,
    ) -> Result<String, ClientError> {
        self.hit()
    }
}

fn source() -> config::Source {
    config::Source::new(
        "local",
        "http://127.0.0.1:9/v1".to_string(),
        None,
        Some("m".to_string()),
        None,
        None,
    )
}

/// Every billed call on one client, so a method added to the trait and left
/// ungated shows up as a count that did not stay put.
async fn spend_everything(client: &Budgeted<Counting>) {
    let source = source();
    let _ = client.chat_completions(&source, "m", &[], 1, None).await;
    let _ = client.overrun_probe(&source, "m").await;
    let _ = client
        .chat_completions_stream(StreamRequest {
            source: &source,
            model: "m",
            messages: &[],
            tools: None,
            tool_choice: None,
            max_tokens: None,
            extra_body: None,
            recovery_sampling: false,
        })
        .await;
}

/// Reads metadata, which costs nothing and must keep working after a cap fires -
/// `/source` would otherwise stop listing models on a run that merely finished
/// spending.
async fn read_metadata(client: &Budgeted<Counting>) {
    let source = source();
    let _ = client.list_models(&source).await;
    let _ = client.get_props(&source).await;
}

/// One test owns the process-wide guard; a second would interleave with it under
/// the parallel runner.
#[tokio::test]
async fn a_stopped_run_reaches_the_endpoint_for_nothing_that_spends() {
    cost::reset();
    usage_totals::reset();

    let client = Budgeted::new(Counting::default());
    spend_everything(&client).await;
    assert_eq!(
        client.inner.count(),
        3,
        "with no budget every billed call goes out, as every run before this did"
    );

    // Arm a cap and drive it past its hard threshold the way a turn does.
    let budget = resolve_budget(Some("1"), &HashMap::new()).unwrap().unwrap();
    let rates = Pricing::parse(Some(r#"{"m": {"input": 1, "output": 1}}"#));
    cost::install(Some(budget), rates.as_ref());
    usage_totals::record(
        "local",
        None,
        "m",
        &NormalizedUsage {
            input_tokens: 9_000_000,
            ..NormalizedUsage::default()
        },
    );
    assert!(matches!(cost::checkpoint(), cost::Verdict::Hard(_)));

    let stopped = Budgeted::new(Counting::default());
    spend_everything(&stopped).await;
    assert_eq!(
        stopped.inner.count(),
        0,
        "a stopped run must not open a billed request through any of them"
    );

    read_metadata(&stopped).await;
    assert_eq!(
        stopped.inner.count(),
        2,
        "reading metadata costs nothing and must survive the cap"
    );

    cost::reset();
    usage_totals::reset();
}

#[test]
fn the_refusal_is_not_blamed_on_the_provider() {
    // A caller branching on `error_kind` must not retry this, and must not read
    // it as the endpoint having a bad minute: the run was told what it may spend
    // and honouring that is what refused the call.
    //
    // Built directly rather than through `spendable`, which reads the
    // process-wide guard that the test above owns.
    let error = ClientError::Budget("spent its budget".to_string());
    assert_eq!(error.kind(), ErrorKind::Policy);
    assert!(
        !matches!(error.kind(), ErrorKind::ProviderHttp),
        "a cap is not the endpoint having a bad minute, and must not be retried as one"
    );
}
