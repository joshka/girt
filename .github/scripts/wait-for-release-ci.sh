#!/usr/bin/env bash
# Only full push CI on this exact main SHA may authorize publishing.
set -euo pipefail

: "${GITHUB_REPOSITORY:?}"
: "${GITHUB_SHA:?}"

for ((attempt = 0; attempt < 180; attempt++)); do
  runs=$(gh api --method GET --paginate --slurp \
    "repos/$GITHUB_REPOSITORY/actions/workflows/validation.yml/runs" \
    -f head_sha="$GITHUB_SHA" -f event=push -f branch=main -f per_page=100)
  run=$(jq -ce --arg sha "$GITHUB_SHA" --arg repo "$GITHUB_REPOSITORY" '
    [.[].workflow_runs[] | select(
      .head_sha == $sha and .event == "push" and .head_branch == "main" and
      .path == ".github/workflows/validation.yml" and
      .head_repository.full_name == $repo
    )] | max_by(.id) // {}
  ' <<<"$runs")
  if [[ $(jq -r '.status' <<<"$run") == completed ]]; then
    if [[ $(jq -r '.conclusion' <<<"$run") == success ]]; then
      echo "Full main CI passed: $(jq -r '.html_url' <<<"$run")"
      exit 0
    fi
    echo "::error::Full main CI did not succeed: $run"
    exit 1
  fi
  echo "Waiting for full push CI on $GITHUB_SHA (attempt $((attempt + 1))/180)"
  sleep 30
done

echo "::error::Timed out waiting for full push CI on $GITHUB_SHA"
exit 1
