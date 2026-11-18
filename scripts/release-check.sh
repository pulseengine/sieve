#!/usr/bin/env bash
# Release gates for sieve, run by .github/workflows/release.yml.
#
#   release-check.sh version-guard TAG       the tag names Cargo.toml's version
#   release-check.sh smoke BIN TAG           the packaged binary is that version,
#                                            meets the CLI baseline, and
#                                            classifies a fixture both ways
#
# Exit 0 = the gate holds, 1 = it does not, 2 = usage.
set -euo pipefail

die() { echo "release-check: $*" >&2; exit 1; }

crate_version() {
  # The first `version = "..."` is [package]'s: Cargo.toml starts with it.
  sed -n 's/^version = "\([^"]*\)"$/\1/p' "${CARGO_TOML:-Cargo.toml}" | head -1
}

version_guard() {
  local tag=$1 v
  v=$(crate_version)
  [ -n "$v" ] || die "no version in ${CARGO_TOML:-Cargo.toml}"
  [ "$tag" = "v$v" ] || die "tag $tag does not name Cargo.toml's version $v"
  echo "release-check: tag $tag matches Cargo.toml $v"
}

smoke() {
  local bin=$1 tag=$2 got rc
  case $bin in /*) ;; *) bin="$PWD/$bin" ;; esac
  got=$("$bin" --version) || die "$bin --version failed"
  [ "$(echo "$got" | head -1)" = "sieve ${tag#v}" ] || die "binary reports '$(echo "$got" | head -1)', tag is $tag"
  for s in sieve.classify.v1 sieve.graph.v1; do
    case "$got" in *"$s"*) ;; *) die "--version does not name $s" ;; esac
  done
  "$bin" --help >/dev/null || die "--help did not exit 0"
  rc=0; "$bin" --no-such-flag >/dev/null 2>&1 || rc=$?
  [ "$rc" = 2 ] || die "an unknown flag exited $rc, not 2"

  # A two-class fixture: a crate change selects its class only, a docs change
  # selects nothing, a .github change selects everything, a bad config exits 2.
  t=$(mktemp -d)
  trap 'rm -rf "${t:-}"' EXIT
  mkdir -p "$t/a/src"
  printf '[package]\nname = "a"\nversion = "0.1.0"\n' > "$t/a/Cargo.toml"
  printf 'pub fn a() {}\n' > "$t/a/src/lib.rs"
  printf '# r\n' > "$t/README.md"
  printf '[class.a]\ncargo = ["a/Cargo.toml"]\n[class.b]\nhand = ["b/"]\n' > "$t/sieve.toml"
  out=$(cd "$t" && "$bin" classify --config sieve.toml --paths a/src/lib.rs)
  echo "$out" | grep -q '"run_all": false' || die "a crate change ran everything: $out"
  echo "$out" | grep -q '"a": true' || die "a crate change did not select its class: $out"
  echo "$out" | grep -q '"b": false' || die "a crate change selected another class: $out"
  out=$(cd "$t" && "$bin" classify --config sieve.toml --paths README.md)
  echo "$out" | grep -q '"a": false' || die "a docs change selected a class: $out"
  out=$(cd "$t" && "$bin" classify --config sieve.toml --paths .github/workflows/ci.yml)
  echo "$out" | grep -q '"run_all": true' || die "a .github change did not run everything: $out"
  printf '[class.a]\nsrcs = []\n' > "$t/bad.toml"
  rc=0; (cd "$t" && "$bin" classify --config bad.toml --paths x >/dev/null 2>&1) || rc=$?
  [ "$rc" = 2 ] || die "an invalid config exited $rc, not 2"
  echo "release-check: $bin is sieve ${tag#v} and classifies the fixture both ways"
}

case "${1:-}" in
  version-guard) [ $# = 2 ] || exit 2; version_guard "$2" ;;
  smoke) [ $# = 3 ] || exit 2; smoke "$2" "$3" ;;
  *) echo "usage: release-check.sh version-guard TAG | smoke BIN TAG" >&2; exit 2 ;;
esac
