#!/usr/bin/bash

release_pr=$(head -n1 <<< "${release_commit:-}" | jq -Rr 'split("[()]"; "")[1] // ""')
if [[ -z "$release_pr" ]]; then
  echo "::set-output name=notes_json::null"
  exit
fi

gh \
  pr --repo "$GITHUB_REPO" \
  view "$release_pr" \
  --json body \
  --jq '"::set-output name=notes_json::\((.body | split("### Release notes")[1] // "") | tojson)"'


