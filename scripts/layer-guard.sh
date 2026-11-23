#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Keep the host libraries independent of their consumers. Cargo metadata
# includes every direct edge, even optional and foreign-target dependencies;
# transitive host dependencies are governed by Cargo.lock and cargo-deny.
# Usage: scripts/layer-guard.sh [--self-test] (needs cargo and jq)
# LAYER_CARGO may name a whitespace-separated supervisor wrapper command.
set -euo pipefail
cd "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

case "${1:-}" in
  ''|--self-test) ;;
  *) printf 'usage: scripts/layer-guard.sh [--self-test]\n' >&2; exit 2 ;;
esac

cargo_command=(cargo)
if [[ -n ${LAYER_CARGO:-} ]]; then
  read -r -a cargo_command <<< "$LAYER_CARGO"
fi
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
"${cargo_command[@]}" metadata --locked --no-deps --all-features --format-version 1 > "$work/metadata.json"

check_metadata() {
  local diagnostics
  diagnostics=$(jq -r '
    def registry:
      .source == "registry+https://github.com/rust-lang/crates.io-index"
      and .path == null;
    def contract:
      .name == "unemu-core-api" and .path == null
      and ((.source // "") | test("^git\\+https://github.com/unemu-org/unemu-core-api\\?rev=[0-9a-f]{40}$"));
    . as $metadata |
    [.packages[] | select(.id as $id | $metadata.workspace_members | index($id))] as $members |
    ($members[] | select(.name != "machine-view" and .name != "unemu-test-core") |
      "layer guard: no dependency policy for workspace crate \(.name)"),
    ($members[] as $package | $package.dependencies[] |
      select((
        contract
        or ($package.name == "machine-view" and (
          ((.name == "cpal" or .name == "winit") and registry)
          or ((.name as $name | (["objc2", "objc2-app-kit", "objc2-core-foundation", "objc2-core-graphics"] | index($name)) != null)
            and registry and .target == "cfg(target_os = \"macos\")")
          or (.name == "unemu-test-core" and .kind == "dev" and .source == null
            and .path == ($metadata.workspace_root + "/crates/test-core"))
        ))
      ) | not) |
      "layer guard: \($package.name) -> \(.name) (\(.kind // "normal"), \(.target // "all targets")) is outside the allowed dependency boundary")
  ' "$1") || return 2
  if [[ -n $diagnostics ]]; then
    printf '%s\n' "$diagnostics" >&2
    return 1
  fi
}

check_metadata "$work/metadata.json"
if [[ ${1:-} == --self-test ]]; then
  failures=0
  expect_failure() {
    local name="$1" filter="$2" expected="$3"
    jq "$filter" "$work/metadata.json" > "$work/mutant.json"
    if check_metadata "$work/mutant.json" > "$work/diagnostic" 2>&1; then
      printf 'FAIL  %s was accepted\n' "$name" >&2
      failures=$((failures + 1))
    elif ! grep -qF -- "$expected" "$work/diagnostic"; then
      printf 'FAIL  %s failed without the expected boundary diagnostic\n' "$name" >&2
      failures=$((failures + 1))
    else
      printf 'ok    %s rejected\n' "$name"
    fi
  }
  # Mutate metadata rather than resolving forbidden crates or building copies.
  for kind in normal dev build; do
    expect_failure "egui-$kind" ".packages[0].dependencies += [{name: \"egui\", kind: \"$kind\"}]" '-> egui'
  done
  expect_failure optional-target '.packages[0].dependencies += [{name: "unemu-pc-core", optional: true, target: "cfg(windows)"}]' '-> unemu-pc-core'
  expect_failure renamed '.packages[0].dependencies += [{name: "egui", rename: "cpal"}]' '-> egui'
  expect_failure host-in-test-core '(.packages[] | select(.name == "unemu-test-core").dependencies) += [{name: "winit", source: "registry+https://github.com/rust-lang/crates.io-index"}]' 'unemu-test-core -> winit'
  expect_failure fake-host '(.packages[].dependencies[] | select(.name == "cpal").source) = "git+https://github.com/example/cpal"' '-> cpal'
  expect_failure local-contract '(.packages[].dependencies[] | select(.name == "unemu-core-api")) |= (.source = null | .path = "local-contract")' '-> unemu-core-api'
  expect_failure moving-contract '(.packages[].dependencies[] | select(.name == "unemu-core-api").source) = "git+https://github.com/unemu-org/unemu-core-api?branch=main"' '-> unemu-core-api'
  expect_failure objc-on-all-hosts '(.packages[].dependencies[] | select(.name == "objc2").target) = null' '-> objc2'
  expect_failure production-test-core '(.packages[].dependencies[] | select(.name == "unemu-test-core").kind) = null' '-> unemu-test-core'
  expect_failure unknown-member '(.packages[] | select(.name == "unemu-test-core").name) = "new-crate"' 'no dependency policy'
  if (( failures )); then
    printf 'layer guard self-test: %d failure(s)\n' "$failures" >&2
    exit 1
  fi
  printf 'layer guard self-test: all 12 mutations rejected; clean graph accepted\n'
else
  printf 'layer guard: all direct dependencies stay within the host boundary\n'
fi
