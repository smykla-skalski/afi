# Releasing

How a version of afi reaches users, what to do when it does not, and how to undo
one that should not have.

The mechanism lives in [`.github/workflows/release.yml`](../.github/workflows/release.yml),
whose header comments explain each step. This document is the operator's view:
what to run, and what to do when something breaks.

## What a release is

Four publications, from one run:

- a GitHub release, with an archive and a checksum for each of five targets
- two Debian packages, attached to that release and pushed to the apt repository
- the `afi-cli` crate on crates.io
- a Sigstore build-provenance attestation for every archive and package

[`scripts/release-targets.sh`](../scripts/release-targets.sh) is the definition
of that list. The build matrix reads it and so does the reconciliation gate, so
adding a target means editing one file.

## The order, and why it matters

```
plan     work out the version. Nothing outside the runner changes.
build    compile, smoke-test and package all five targets.
release  commit the bump, tag, open a DRAFT release, attach and attest.
apt      push both Debian packages.
verify   assert the release and the apt repository agree.
publish  flip the draft.
```

Nothing is visible to a user until `publish`. That is deliberate, and it is the
fix for how the first four releases went wrong:

- **v0.4.0** was published carrying one of three platforms and gained the other
  two seven and a half hours later, because the release was created before the
  builds ran.
- **v0.3.0** was tagged and never got a release at all.
- **v0.2.0** attached an amd64 `.deb` to its release page and never pushed that
  package to apt, so `apt-get install afi=0.2.0-1` failed on amd64 while the
  release page said otherwise.

A failure anywhere before `publish` now leaves a draft release, which nobody
without push access can see, and a tag. Re-running the workflow resumes.

## Cutting a release

Normally you do not. The scheduled run at 04:17 UTC releases whatever has
accumulated, and stops when nothing has. `release_commits` in
[`release-plz.toml`](../release-plz.toml) decides what counts: `feat`, `fix`,
`perf`, `refactor`. A day of `docs` and `chore` commits releases nothing.

To release off-schedule:

```
gh workflow run release.yml
```

Inputs, all optional:

| input     | effect |
|-----------|--------|
| `version` | Release exactly this version instead of working it out from the commits. No leading `v`. |
| `force`   | Release even when no commit since the last tag is releasable. |
| `dry_run` | Plan and build everything, publish nothing. |

A dry run is worth using before a change to the release machinery. It runs the
gate, works out the version, and builds, smoke-tests and packages all five
targets, then stops. Everything it proves is real; nothing it does is visible.

```
gh workflow run release.yml -f dry_run=true
```

## Prereleases

Ask for one by version:

```
gh workflow run release.yml -f version=0.5.0-rc.1
```

`git_release_type = "auto"` marks it a prerelease on GitHub, and the publish job
leaves `releases/latest` pointing at the last stable version.
[`scripts/deb-version.sh`](../scripts/deb-version.sh) converts the SemVer
prerelease into `0.5.0~rc.1-1`, which dpkg sorts *below* `0.5.0`, so a plain
`apt-get upgrade` will not pull a release candidate onto a stable machine.

## When a release fails

Find which job stopped, then:

**`plan`** — nothing happened. The gate failed, the supply-chain gate failed, or
a credential is missing. Fix and re-run.

**`build`** — nothing happened. One target failed to compile, package, or run.
`fail-fast` is off, so the log shows the state of all five, not just the first.

**`release`, `apt`, or `verify`** — a tag exists and a draft release exists.
Nothing is public. Fix the cause and re-run the workflow; it resumes:

- `plan-release.sh` notices the manifest is at a version with no tag and
  publishes it as it stands rather than recomputing.
- The commit step notices the bump is already on `main` and skips it.
- The upload step passes `--clobber`, and the apt push passes `--republish`.

Re-run rather than dispatching a fresh run, so the same version is finished
instead of a new one being started next to it.

**`verify`** — the release and the apt repository disagree. The output names
every missing file. This is the gate doing its job; the release stays a draft.

## Undoing a release

### The version is bad and it is already published

Releases are additive. Do not delete, publish a fix:

```
gh workflow run release.yml -f version=0.5.1
```

Deleting a published tag breaks everyone who already fetched it, and once
immutable releases are on (see below) GitHub will not let you.

### Pull a Debian package out of apt

Deleting is the only way to stop apt offering a version:

```
cloudsmith delete smykla-skalski/afi/afi_0.5.0-1_amd64.deb
```

Do both architectures, or you leave the split state v0.2.0 was in. Then publish a
fixed version, because a machine that already upgraded stays where it is.

### Yank a crate

```
cargo yank --version 0.5.0 afi-cli
```

A yank stops new dependents from resolving to it and leaves existing lockfiles
working. It is not a delete, and there is no delete.

### Un-publish a GitHub release

Turn it back into a draft rather than deleting it, so the tag and the assets
survive while it is out of sight:

```
gh release edit v0.5.0 --draft=true
```

## A tag with no release

The state v0.3.0 was left in. The current pipeline cannot produce it, but a tag
pushed by hand can.

Ask for the version explicitly. `plan-release.sh` refuses a version that is
already tagged, so first check what actually exists:

```
gh release view v0.3.0            # "release not found" means tag only
git ls-remote --tags upstream     # the tag is there
```

Build and attach by hand from the tag, or delete the tag if nobody has fetched
it. There is no automatic recovery, because the workflow's resume path keys off
the manifest version, and by then the manifest has moved on.

## Setup

Repository variables:

| variable | purpose |
|----------|---------|
| `CLOUDSMITH_WORKSPACE` | apt workspace |
| `CLOUDSMITH_REPOSITORY` | apt repository |
| `CLOUDSMITH_SERVICE_SLUG` | the service account the OIDC token is exchanged for |
| `RUNNER_LABEL` | optional, overrides the Linux runner |
| `MACOS_RUNNER_LABEL` | optional, overrides the macOS runner |

Repository secrets:

| secret | purpose |
|--------|---------|
| `SMYKLOT_APP_PRIVATE_KEY` | the app that can commit the version bump to `main` |
| `CARGO_REGISTRY_TOKEN` | crates.io publishing |

The `plan` job checks all of them before anything is tagged, so a missing one
costs a failed run rather than a half-finished release.

[`.github/workflows/apt-credentials-check.yml`](../.github/workflows/apt-credentials-check.yml)
exercises the Cloudsmith credential without publishing. Run it if a release has
not happened in a while and you want to know the OIDC trust still resolves before
you find out during one.

## Immutable releases

[GitHub immutable releases](https://docs.github.com/en/enterprise-cloud@latest/code-security/supply-chain-security/understanding-your-software-supply-chain/immutable-releases)
lock a release's assets and protect its tag at publication. The draft-then-publish
ordering above is what makes them possible: a published immutable release can
never gain another asset, so a pipeline that uploads after publishing cannot use
them.

Turn it on once this pipeline is on `main`:

```
gh api -X PUT repos/smykla-skalski/afi/immutable-releases -f enabled=true
```

Existing releases stay mutable. Every new one is locked.

## Verifying a release

What a user should be told to run, and what you should run yourself after a
release:

```
gh attestation verify afi-x86_64-unknown-linux-musl.tar.gz --repo smykla-skalski/afi
```

The published `.sha256` files sit next to the archives and are uploaded by the
same job, so they prove a download arrived intact and nothing about where it came
from. The attestation is the one that answers that.
