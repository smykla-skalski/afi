use super::{ALL, Provider, of_url};
use crate::config::Bedrock;
use crate::pricing::table::vendored;

#[test]
fn every_built_in_address_names_its_rate_card() {
    // The four sources afi registers by itself, plus the two the documentation
    // tells people to configure by hand. A typo in the host table here is a
    // source that silently stops being priced.
    let cases = [
        ("https://api.anthropic.com", Provider::Anthropic),
        ("https://api.together.xyz/v1", Provider::Together),
        ("https://openrouter.ai/api/v1", Provider::OpenRouter),
        ("https://api.z.ai/api/paas/v4", Provider::ZAi),
        ("https://api.openai.com/v1", Provider::OpenAi),
        ("https://open.bigmodel.cn/api/paas/v4", Provider::Zhipu),
    ];
    for (url, want) in cases {
        assert_eq!(of_url(url), Some(want), "{url}");
    }
}

#[test]
fn every_bedrock_region_is_one_rule() {
    // The endpoint carries the Region and the partition suffix, so both ends are
    // matched rather than enumerating dozens of hosts that differ only in the
    // part pricing does not depend on.
    for url in [
        "https://bedrock-runtime.us-east-1.amazonaws.com/v1",
        "https://bedrock-runtime.eu-central-1.amazonaws.com/v1",
        "https://bedrock-runtime.cn-north-1.amazonaws.com.cn/v1",
    ] {
        assert_eq!(of_url(url), Some(Provider::Bedrock), "{url}");
    }
}

#[test]
fn the_endpoint_afi_builds_for_itself_resolves_to_bedrock() {
    // Two spellings of one host shape, in two modules: `bedrock::ENDPOINT`
    // writes it and the host table reads it. Derived from the real builder
    // rather than hardcoded, so the two cannot drift - if AWS moves the host,
    // this fails instead of every Bedrock run silently losing its price and
    // every budgeted one silently refusing to start.
    for region in ["us-east-1", "eu-central-1", "cn-north-1"] {
        let bedrock = Bedrock {
            region: Some(region.to_string()),
            ..Bedrock::default()
        };
        let built = bedrock.base_url().expect("a Region builds an endpoint");
        assert_eq!(
            of_url(&built),
            Some(Provider::Bedrock),
            "afi builds {built} and then cannot price it"
        );
    }
}

#[test]
fn a_compliance_endpoint_is_still_bedrock() {
    // `AFI_BEDROCK_BASE_URL` is documented, and this is what a FIPS-bound
    // deployment points it at. Same service, same bill.
    assert_eq!(
        of_url("https://bedrock-runtime-fips.us-east-1.amazonaws.com/v1"),
        Some(Provider::Bedrock)
    );
}

#[test]
fn a_bedrock_lookalike_is_not_billed_as_bedrock() {
    // A host anyone can register that merely begins or ends the same way is not
    // AWS, and billing it at AWS's rates would be a wrong number stated
    // confidently.
    for url in [
        "https://bedrock-runtime-proxy.example.com/v1",
        "https://bedrock-runtime.example.com/v1",
        "https://bedrock-runtime.us-east-1.amazonaws.com.evil.test/v1",
        "https://not-bedrock-runtime.us-east-1.amazonaws.com/v1",
    ] {
        assert_eq!(of_url(url), None, "{url}");
    }
}

#[test]
fn an_address_afi_has_no_rates_for_is_priced_by_nothing() {
    // The local case, and the reason the answer is `None` rather than a guess:
    // a llama.cpp on localhost costs nothing afi could know, and billing it at
    // some other provider's rates would be worse than reporting no figure.
    for url in [
        "http://localhost:8080/v1",
        "http://127.0.0.1:8080/v1",
        "https://llm.internal.example.com/v1",
        "",
    ] {
        assert_eq!(of_url(url), None, "{url}");
    }
}

#[test]
fn the_address_is_read_past_credentials_a_port_and_a_path() {
    // Everything after the authority is exactly what the host table must
    // ignore, and a base url carrying a port or an inline credential is an
    // ordinary thing to have configured.
    assert_eq!(
        of_url("https://API.OpenAI.com:443/v1"),
        Some(Provider::OpenAi)
    );
    assert_eq!(
        of_url("https://user@api.openai.com/v1"),
        Some(Provider::OpenAi)
    );
    assert_eq!(of_url("api.openai.com/v1"), Some(Provider::OpenAi));
}

#[test]
fn a_provider_key_round_trips() {
    // The keys are what `assets/prices.json` and the cache are written with, so
    // a rename that lost one would silently unprice every model under it.
    for provider in ALL {
        assert_eq!(Provider::from_key(provider.key()), Some(provider));
    }
    assert_eq!(Provider::from_key("a-provider-from-the-future"), None);
}

#[test]
fn afi_does_not_borrow_the_catalogues_names() {
    // The decoupling, asserted rather than assumed. Three of these differ from
    // what the catalogue calls the same provider, and the stored table uses afi's
    // name - so swapping catalogues moves no key in `assets/prices.json`.
    assert_eq!(Provider::Bedrock.key(), "bedrock");
    assert_eq!(Provider::Together.key(), "together");
    assert_eq!(Provider::Zhipu.key(), "zhipu");
    assert_eq!(Provider::Fireworks.key(), "fireworks");
}

#[test]
fn the_vendored_table_and_the_provider_list_agree() {
    // Two lists that mean the same thing: the providers afi can resolve a source
    // to. A provider afi knows but the file does not carry is a source afi
    // claims to price and cannot; one the file carries but afi cannot reach is
    // weight in the binary nothing reads.
    let vendored = vendored();
    let mut carried: Vec<&str> = vendored.providers.keys().map(String::as_str).collect();
    carried.sort_unstable();
    let known: Vec<&str> = ALL.iter().map(|p| p.key()).collect();
    assert_eq!(
        carried, known,
        "assets/prices.json and `provider::ALL` name different providers; \
         update PROVIDERS in scripts/project-prices.py or `ALL` beside it"
    );
}
