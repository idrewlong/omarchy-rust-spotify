#!/usr/bin/env bash
# After the release workflow has published manifest.json's version: checks
# each tarball's build attestation (built by this repo's release workflow
# from that tag) and pins their hashes in packaging/release.sha256, which
# is what install.sh trusts. Commit the result; that commit is the one to
# submit to the marketplace.
set -euo pipefail
cd "$(dirname "$0")/.."

repo=idrewlong/skinamp
version=$(jq -r .version manifest.json)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

gh release download "v$version" --repo "$repo" --dir "$work" --pattern '*.tar.gz'
for f in "$work"/*.tar.gz; do
  gh attestation verify "$f" --repo "$repo" \
    --signer-workflow "$repo/.github/workflows/release.yml" \
    --source-ref "refs/tags/v$version" >/dev/null
  echo "attested: ${f##*/}"
done
(cd "$work" && sha256sum -- *.tar.gz) > packaging/release.sha256
cat packaging/release.sha256
