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
- the `afi-cli` crate on crates.io, published last by `scripts/publish-crate.sh`
- a Sigstore build-provenance attestation for every archive and package

[`scripts/release-targets.sh`](../scripts/release-targets.sh) is the definition
of that list. The build matrix reads it and so does the reconciliation gate, so
adding a target means editing one file.

## The order, and why it matters

```
plan     work out the version and commit the bump. No tag, no release.
build    compile, smoke-test and package all five targets.
release  tag, open a DRAFT release, attach and attest.
apt      push both Debian packages.
verify   assert the release and the apt repository agree.
publish  flip the draft, then publish the crate.
```

The crate goes last, on its own, because it is the only publication a release
cannot take back. Everything ahead of it is reversible: a release can be turned
back into a draft, a Debian package can be deleted from apt. A crate can only be
yanked, which stops new dependents resolving to it and leaves it downloadable
forever. So it is published once nothing else can still fail.

Nothing is visible to a user until `publish`. That is deliberate, and it is the
fix for how the first four releases went wrong:

- **v0.4.0** was published carrying one of the three platforms it shipped and
  gained the other two seven and a half hours later, because the release was
  created before the builds ran.
- **v0.3.0** was tagged and never got a release at all.
- **v0.2.0** attached an amd64 `.deb` to its release page and never pushed that
  package to apt, so `apt-get install afi=0.2.0-1` failed on amd64 while the
  release page said otherwise.

A failure anywhere before `publish` now leaves at most a bump commit on `main`,
and from `release` onwards a tag and a draft release. None of it is visible to
anyone without push access, and re-running the workflow resumes.

The bump is committed in `plan`, before the build, which is the one thing here
that writes outside the runner early. The alternative was to build the bumped
files as an overlay on top of the previous commit, and `build.rs` stamps a
modified working tree as `(dirty)`: every released binary would have reported
itself dirty and named a commit that is not the one the tag points at. A version
bump sitting on `main` is the cheaper of the two, and `plan-release.sh` already
treats "a manifest version with no tag" as its resume case.

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

A dry run does not commit the version bump, so its build jobs lay the bumped
files over the previous commit instead. That leaves the working tree modified
and the binaries stamped `(dirty)`, which is correct for something that ships
nothing and is why a real release commits first and builds a clean tree.

## Checking an archive without CI

The same build and the same check a release runs, on one target, locally:

```
mise run dist                                      # build and pack for the host
mise run smoke target/dist/afi-<target>.tar.gz <target> <version>
```

`dist` produces the layout `taiki-e/upload-rust-binary-action` produces in CI,
and `smoke` is the script the release runs against every archive. Worth doing
before touching anything under `scripts/` that a release depends on.

On a machine that cannot execute the target, `smoke` falls back to asserting the
object format and architecture from the file header and says so. A release sets
`AFI_SMOKE_REQUIRE=execute`, which turns that fallback into a failure, so no
archive is ever published having only been looked at.

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

**`plan`** — the gate failed, the supply-chain gate failed, or a credential is
missing, and nothing was written. If it got as far as the bump, `main` carries a
version with no tag; that is the resume case below, not a problem to clean up.

**`build`** — a bump commit is on `main` and one target failed to compile,
package, or run. `fail-fast` is off, so the log shows the state of all five, not
just the first. Nothing is public.

**`release`, `apt`, or `verify`** — a tag exists and a draft release exists.
Still nothing public. Fix the cause and re-run the workflow; it resumes:

- `plan-release.sh` notices the manifest is at a version with no tag and
  publishes it as it stands rather than recomputing.
- The commit step notices the bump is already on `main` and reuses that commit.
- The upload step passes `--clobber`, the apt push passes `--republish`, and
  `publish-crate.sh` is a no-op when that version is already on crates.io.

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

The state v0.3.0 was left in. The current pipeline cannot produce it, because it
builds everything before it tags, but a tag pushed by hand can, and the tags that
predate this pipeline already have.

`release.yml` cannot fix it: its resume path keys off the manifest version, and
by the time anyone notices, `main` has moved on.
[`backfill-release.yml`](../.github/workflows/backfill-release.yml) is the
recovery.

```
gh workflow run backfill-release.yml -f tag=v0.3.0
```

It checks out the tag, builds and smoke-tests every target from it, attests the
results, and publishes a release. Guards:

- It refuses a tag that already has a *published* release, so it can only fill a
  gap and never overwrite bytes someone has already downloaded.
- It refuses a tag whose manifest disagrees with the tag name.
- It builds from exactly that commit and does not move the tag, bump anything, or
  touch the changelog. The release notes are that version's existing changelog
  section, plus a note saying it was backfilled.
- The result is not marked latest, and nothing is pushed to apt: a backfilled
  version is by definition older than what apt already serves.

The packaging comes from the tag's own tree, so the `.deb` is the one that
version would have produced. Only the target list and the smoke test are taken
from the default branch, because a tag old enough to need this predates them.

Do a dry run first if the tag is old enough that you are unsure it still builds:

```
gh workflow run backfill-release.yml -f tag=v0.3.0 -f dry_run=true
```

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
| `CARGO_REGISTRY_TOKEN` | crates.io publishing, until trusted publishing replaces it. See below. |

The `plan` job checks all of them before anything is tagged, so a missing one
costs a failed run rather than a half-finished release.

[`.github/workflows/apt-credentials-check.yml`](../.github/workflows/apt-credentials-check.yml)
exercises the Cloudsmith credential without publishing. Run it if a release has
not happened in a while and you want to know the OIDC trust still resolves before
you find out during one.

## crates.io credentials

The publish job prefers [trusted publishing](https://crates.io/docs/trusted-publishing):
crates.io takes the job's OIDC identity and returns a token that lasts thirty
minutes and is revoked when the job ends. Same model as the Cloudsmith push, and
for the same reason: nothing long-lived sits in the repository secrets.

It cannot be turned on yet. A trusted publisher is registered against a crate you
already own, and `afi-cli` has never been published, so the first release falls
back to `CARGO_REGISTRY_TOKEN` and says so with a warning in the job log.

That token is scoped as narrowly as crates.io allows: crate pattern `afi-cli`,
endpoints `publish-new` and `publish-update` only, 365-day expiry. It cannot
touch any other crate, and `publish-new` is there solely so the first release can
create `afi-cli`.

Once the first release has published the crate:

1. Go to `https://crates.io/crates/afi-cli/settings/trusted-publishing`.
2. Add a GitHub publisher: owner `smykla-skalski`, repository `afi`, workflow
   `release.yml`. Leave the environment empty unless one is added later.
3. Run a release and confirm the log says `Authenticated to crates.io by trusted
   publishing.` rather than the fallback warning.
4. Revoke the token on crates.io and delete the `CARGO_REGISTRY_TOKEN` secret.

After step 4 the fallback has nothing to fall back to, so a broken trusted-publishing
setup fails the job instead of quietly reverting to a stored credential.

## Immutable releases

[GitHub immutable releases](https://docs.github.com/en/enterprise-cloud@latest/code-security/supply-chain-security/understanding-your-software-supply-chain/immutable-releases)
lock a release's assets and protect its tag at publication. The draft-then-publish
ordering above is what makes them possible: a published immutable release can
never gain another asset, so a pipeline that uploads after publishing cannot use
them.

Already on, as of 2026-08-07. It is a bare toggle with no request body -- passing
`-f enabled=true` is rejected with a 422 and the unhelpful `"enabled" is not a
permitted key`:

```
gh api -X PUT repos/smykla-skalski/afi/immutable-releases     # enable
gh api repos/smykla-skalski/afi/immutable-releases            # check
gh api -X DELETE repos/smykla-skalski/afi/immutable-releases  # disable
```

Existing releases stay mutable. Every new one is locked, which means a release
that turns out to need another asset cannot get one: cut the next version
instead. A draft is still mutable, so the pipeline's upload-then-publish
ordering is unaffected.

## Verifying a release

What a user should be told to run, and what you should run yourself after a
release:

```
gh attestation verify afi-x86_64-unknown-linux-musl.tar.gz --repo smykla-skalski/afi
```

The published `.sha256` files sit next to the archives and are uploaded by the
same job, so they prove a download arrived intact and nothing about where it came
from. The attestation is the one that answers that.
