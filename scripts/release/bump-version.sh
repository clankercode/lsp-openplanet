#!/usr/bin/env bash
# Bump Cargo.toml + all npm package.json versions together, then refresh
# Cargo.lock so `cargo build --locked` stays valid.
# Usage: scripts/release/bump-version.sh 0.3.0
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

if [[ $# -ne 1 ]]; then
  echo "Usage: $0 <semver>" >&2
  exit 2
fi
VERSION="$1"
if [[ ! "${VERSION}" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]]; then
  echo "Version must be semver (got: ${VERSION})" >&2
  exit 2
fi

# Cargo.toml
if grep -q '^version = "' Cargo.toml; then
  sed -i.bak -E "s/^version = \"[^\"]+\"/version = \"${VERSION}\"/" Cargo.toml
  rm -f Cargo.toml.bak
else
  echo "Could not find version in Cargo.toml" >&2
  exit 1
fi

# npm packages
shopt -s nullglob
for pkg in npm/*/package.json; do
  node -e "
    const fs=require('fs');
    const p=process.argv[1];
    const v=process.argv[2];
    const j=JSON.parse(fs.readFileSync(p,'utf8'));
    j.version=v;
    if (j.optionalDependencies) {
      for (const k of Object.keys(j.optionalDependencies)) j.optionalDependencies[k]=v;
    }
    fs.writeFileSync(p, JSON.stringify(j,null,2)+'\n');
    console.log('updated', p, '->', v);
  " "$pkg" "$VERSION"
done

# Keep Cargo.lock in lockstep with Cargo.toml package version (required for --locked CI).
echo "Refreshing Cargo.lock…"
cargo generate-lockfile
cargo metadata --locked --format-version 1 >/dev/null
echo "Cargo.lock OK for --locked builds"

echo
echo "Versions set to ${VERSION}."
echo "Next:"
echo "  1. Commit: git add Cargo.toml Cargo.lock npm CHANGELOG.md && git commit -m \"chore: release v${VERSION}\""
echo "  2. Tag:    git tag -a v${VERSION} -m \"v${VERSION}\""
echo "  3. Push:   git push origin HEAD && git push origin v${VERSION}"
echo "  4. Watch:  gh run watch  # release workflow"
