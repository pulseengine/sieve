#!/usr/bin/env bash
# Run the corpus harness's wasmtime command line under kiln instead
# (advisory CI job `kiln (advisory)`). The harness calls
#   <runtime> run -S <opt> --dir <host>::<guest> ... [--env K=V]... <x.wasm> <sieve args>
# and kiln runs the meld-fused module ($FUSED) with meld's import map.
# kiln names a preopen after its host path (no host::guest mapping), so the
# tree preopened as "." becomes an absolute path: --root is added after the
# subcommand, and --config / --paths-file values are made absolute.
# kiln serves wasi:cli/exit exit-with-code by default, so -S is dropped.
set -euo pipefail
: "${KILN:?}" "${FUSED:?}" "${IMPORT_MAP:?}"
[ "${1:-}" = run ] && shift
kargs=(--wasi --component --import-map "$IMPORT_MAP")
root=""
while [ $# -gt 0 ]; do
  case $1 in
    -S) shift 2 ;;
    --dir) m=$2; host=${m%%::*}; guest=${m#*::}
           kargs+=(--wasi-fs "$host"); [ "$guest" = . ] && root=$host; shift 2 ;;
    --env) k=${2%%=*}; export "$k=${2#*=}"; kargs+=(--wasi-env "$k"); shift 2 ;;
    *.wasm) shift; break ;;
    *) echo "kiln-as-wasmtime: unexpected $1" >&2; exit 2 ;;
  esac
done
abs() { case $1 in /*) echo "$1" ;; *) echo "$root/$1" ;; esac; }
g=(); first=1
while [ $# -gt 0 ]; do
  case $1 in
    --config|--paths-file) g+=("$1" "$(abs "$2")"); shift 2 ;;
    *) g+=("$1"); if [ $first = 1 ] && [ -n "$root" ]; then g+=(--root "$root"); fi; shift ;;
  esac
  first=0
done
for a in "${g[@]}"; do kargs+=(--wasi-arg "$a"); done
exec "$KILN" "${kargs[@]}" "$FUSED"
