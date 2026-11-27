#!/usr/bin/env bash
# Planted control for the advisory kiln job: the native-vs-kiln diff must be
# able to fail. On a two-class fixture, kiln's verdict for a crate change
# must equal native's byte for byte AND differ from native's verdict for a
# .github change (the plant). Both outputs must be non-empty JSON verdicts,
# so two empty outputs (a crash on both sides) never compare equal.
#   kiln-control.sh NATIVE_BIN        (KILN, FUSED, IMPORT_MAP as for
#                                      kiln-as-wasmtime.sh)
# Exit 0 = the control holds, 1 = it does not.
set -euo pipefail
die() { echo "kiln-control: $*" >&2; exit 1; }
native=$1
case $native in /*) ;; *) native="$PWD/$native" ;; esac
here=$(cd "$(dirname "$0")" && pwd)
t=$(mktemp -d)
trap 'rm -rf "$t"' EXIT
mkdir -p "$t/a/src"
printf '[package]\nname = "a"\nversion = "0.1.0"\n' > "$t/a/Cargo.toml"
printf 'pub fn a() {}\n' > "$t/a/src/lib.rs"
printf '[class.a]\ncargo = ["a/Cargo.toml"]\n[class.b]\nhand = ["b/"]\n' > "$t/sieve.toml"
cd "$t"
"$here/kiln-as-wasmtime.sh" run --dir "$t::." "$FUSED" classify --config sieve.toml --paths a/src/lib.rs > kiln.json
"$native" classify --config sieve.toml --paths a/src/lib.rs > native.json
"$native" classify --config sieve.toml --paths .github/workflows/ci.yml > plant.json
for f in kiln.json native.json plant.json; do
  grep -q '"run_all"' "$f" || die "$f is not a verdict: $(head -c 200 "$f")"
done
cmp -s kiln.json native.json || die "kiln differs from native on the crate change: $(diff native.json kiln.json | head -20)"
if cmp -s kiln.json plant.json; then die "the plant was not caught: kiln's crate verdict equals native's .github verdict"; fi
echo "kiln-control: kiln equals native on the fixture, and the planted .github verdict differs"
