#!/bin/sh
set -eu

# Commit the version bump to the default branch through GitHub's API, and print
# the new commit's oid.
#
# Not `git commit && git push`. The branch ruleset requires signed commits, and a
# commit made by git on a runner has no key to sign with, so the push would be
# rejected -- or accepted only because the app happens to be exempt from the whole
# ruleset, which is not a property worth depending on. Commits created through
# createCommitOnBranch are signed by GitHub, so this satisfies the rule outright
# and leaves the app's bypass covering only the missing pull request.
#
# Inputs from the environment:
#
#   GITHUB_REPOSITORY  owner/name, set by Actions
#   GITHUB_TOKEN       a token for an identity allowed to push to the branch
#   AFI_RELEASE_TAG    the version being released, used in the commit subject
#   AFI_RELEASE_BRANCH the branch to commit on (default: main)
#
# Arguments: the files to include. They are read from the working tree as they
# are now, so the caller must have written them already.

: "${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is required}"
: "${AFI_RELEASE_TAG:?AFI_RELEASE_TAG is required}"
branch=${AFI_RELEASE_BRANCH:-main}

if [ $# -lt 1 ]; then
    printf 'usage: %s <file> [file...]\n' "$0" >&2
    exit 2
fi

# The API rejects the mutation if the branch has moved since this run checked it
# out, which is what stops a release from overwriting a merge that landed while
# it was working.
head_oid=$(git rev-parse HEAD)

additions=$(
    for file in "$@"; do
        [ -f "$file" ] || { printf 'no such file: %s\n' "$file" >&2; exit 1; }
        # -w0 is GNU-only, and tr keeps this working anywhere.
        jq -n --arg path "$file" \
              --arg contents "$(base64 < "$file" | tr -d '\n')" \
              '{path: $path, contents: $contents}'
    done | jq -s '.'
)

payload=$(
    jq -n \
        --arg repo "$GITHUB_REPOSITORY" \
        --arg branch "$branch" \
        --arg oid "$head_oid" \
        --arg headline "chore(release): $AFI_RELEASE_TAG" \
        --argjson additions "$additions" \
        '{
          query: "mutation($input: CreateCommitOnBranchInput!) { createCommitOnBranch(input: $input) { commit { oid } } }",
          variables: {
            input: {
              branch: { repositoryNameWithOwner: $repo, branchName: $branch },
              message: { headline: $headline },
              expectedHeadOid: $oid,
              fileChanges: { additions: $additions }
            }
          }
        }'
)

oid=$(
    printf '%s' "$payload" \
        | gh api graphql --input - \
            --jq '.data.createCommitOnBranch.commit.oid'
)

if [ -z "$oid" ] || [ "$oid" = "null" ]; then
    printf 'the API returned no commit oid\n' >&2
    exit 1
fi

printf '%s\n' "$oid"
