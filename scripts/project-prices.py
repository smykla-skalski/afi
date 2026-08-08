#!/usr/bin/env python3
"""Project models.dev's catalogue into the rate table afi vendors.

models.dev publishes one 3.6 MB `api.json` covering 181 providers and 6231
models, most of which afi can never reach and none of which it needs the
capability metadata for. This takes the intersection: the providers
`src/pricing/provider.rs` can resolve a source to, and the five token classes
`Pricing` knows how to bill.

The output is checked in. A build step that fetched this would break an
offline build, make two builds of one commit differ, and put data nobody
reviewed into the binary - so the network call happens here, in a scheduled
workflow that opens a pull request, and the diff is the review.

Determinism is the point of the formatting. Providers and models are sorted,
each model is one line, and `fetched` only moves when a rate does, so a refresh
that changed nothing is an empty diff rather than a date bump nobody can check.
"""

from __future__ import annotations

import datetime
import json
import sys
import urllib.request
from pathlib import Path

# --- the catalogue -----------------------------------------------------------
#
# Everything models.dev-shaped is in this block, mirroring
# `src/pricing/catalog/models_dev.rs`, which is the same projection in the
# language that reads the result back. Swapping catalogues is this block and
# that file; nothing else here or in the Rust knows the name.

SOURCE = "https://models.dev/api.json"

# afi's name for a provider, then this catalogue's name for it. Most match,
# which is exactly why the mapping is written down: an accident of agreement is
# not a contract, and reading afi's stored keys straight off somebody else's
# catalogue is how the two get welded together without anyone deciding to.
#
# The left column has to be `Provider::key` in `src/pricing/provider.rs`, and
# `the_vendored_table_and_the_host_table_agree` fails when it drifts.
PROVIDERS = {
    "anthropic": "anthropic",
    "bedrock": "amazon-bedrock",
    "cerebras": "cerebras",
    "deepseek": "deepseek",
    "fireworks": "fireworks-ai",
    "google": "google",
    "groq": "groq",
    "mistral": "mistral",
    "openai": "openai",
    "openrouter": "openrouter",
    "together": "togetherai",
    "xai": "xai",
    "zai": "zai",
    "zhipu": "zhipuai",
}

# --- afi's own format ---------------------------------------------------------

# The classes `pricing::RawRates` deserializes, and no others. models.dev also
# carries `tiers`, `context_over_200k`, `input_audio` and `output_audio`, which
# afi does not model - and `RawRates` is `deny_unknown_fields`, so passing one
# through would refuse the whole table at startup.
CLASSES = ["input", "output", "cache_read", "cache_write", "reasoning"]

ASSET = Path(__file__).resolve().parent.parent / "assets" / "prices.json"


def fetch(source: str) -> dict:
    """The catalogue, from models.dev or from a local copy of it.

    A path argument is how the determinism test runs this twice without asking
    models.dev twice, and how a refusal above can be reproduced from the file
    that caused it.
    """
    if not source.startswith("https://"):
        return json.loads(Path(source).read_text())
    # models.dev answers the default urllib agent with a 403.
    request = urllib.request.Request(source, headers={"User-Agent": "afi-price-projection"})
    with urllib.request.urlopen(request, timeout=60) as response:  # noqa: S310
        return json.load(response)


def project(catalogue: dict) -> dict[str, dict[str, dict[str, float]]]:
    """The providers afi can reach, priced in the classes afi bills."""
    out: dict[str, dict[str, dict[str, float]]] = {}
    for key, slug in sorted(PROVIDERS.items()):
        provider = catalogue.get(slug)
        if provider is None:
            sys.exit(f"models.dev no longer has provider {slug!r}")
        models: dict[str, dict[str, float]] = {}
        seen: dict[str, str] = {}
        for model_id, model in sorted(provider.get("models", {}).items()):
            cost = model.get("cost")
            # No input rate is no rate at all: `Rates::weighted` refuses to
            # price a class that was spent on and left unpriced, so half an
            # entry would suppress the figure for the whole run.
            if not isinstance(cost, dict) or "input" not in cost:
                continue
            # afi matches model ids case-insensitively after trimming, so two
            # spellings of one id would make the bill depend on which survived.
            # `Pricing::normalize` refuses that; so does this.
            normalized = model_id.strip().lower()
            if normalized in seen:
                sys.exit(
                    f"{slug} names {normalized!r} twice: {seen[normalized]!r} and {model_id!r}"
                )
            seen[normalized] = model_id
            models[model_id] = {c: cost[c] for c in CLASSES if c in cost}
        if models:
            out[key] = models
    return out


def render(providers: dict, fetched: str) -> str:
    """One line per model, so a rate change is a one-line diff."""
    lines = ['{', f'  "fetched": "{fetched}",', '  "providers": {']
    for i, (slug, models) in enumerate(sorted(providers.items())):
        lines.append(f'    "{slug}": {{')
        entries = sorted(models.items())
        for j, (model_id, rates) in enumerate(entries):
            body = ", ".join(f'"{c}": {json.dumps(rates[c])}' for c in CLASSES if c in rates)
            comma = "" if j == len(entries) - 1 else ","
            lines.append(f'      {json.dumps(model_id)}: {{{body}}}{comma}')
        lines.append("    }" + ("" if i == len(providers) - 1 else ","))
    lines += ["  }", "}", ""]
    return "\n".join(lines)


def main() -> None:
    providers = project(fetch(sys.argv[1] if len(sys.argv) > 1 else SOURCE))
    previous = json.loads(ASSET.read_text()) if ASSET.exists() else {}
    if previous.get("providers") == providers:
        print(f"{ASSET.name}: no rate moved; leaving it alone")
        return
    fetched = datetime.datetime.now(datetime.timezone.utc).date().isoformat()
    ASSET.parent.mkdir(parents=True, exist_ok=True)
    ASSET.write_text(render(providers, fetched))
    total = sum(len(m) for m in providers.values())
    print(f"{ASSET.name}: {len(providers)} providers, {total} models, fetched {fetched}")


if __name__ == "__main__":
    main()
