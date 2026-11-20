#!/usr/bin/env bash
# Lockfile gates for sieve, run by the Test job in .github/workflows/ci.yml.
#
#   lockfiles.sh locked [ROOT]   every tracked Cargo.toml resolves with
#                                --locked: its lockfile is committed and
#                                current, nested workspaces included
#   lockfiles.sh audit [ROOT]    cargo audit over every tracked Cargo.lock;
#                                a vulnerability fails, informational
#                                warnings (unsound, unmaintained) are
#                                printed and do not
#   lockfiles.sh controls        planted defects that each gate must reject
#
# ROOT defaults to the current directory and must be a git work tree: only
# tracked files count, so a gitignored lockfile reads as missing.
# Exit 0 = the gate holds, 1 = it does not, 2 = usage.
set -euo pipefail

die() { echo "lockfiles: $*" >&2; exit 1; }

tracked() { git -C "$1" ls-files -- "$2" ":(glob)**/$2"; }

locked() {
  local root=${1:-.} m n=0 bad=0 err
  err=$(mktemp)
  # A dependency manifest of another ecosystem has no gate here yet; fail
  # instead of passing it unchecked.
  for m in package.json MODULE.bazel go.mod pyproject.toml; do
    if [ -n "$(tracked "$root" "$m")" ]; then
      die "$m is tracked but has no lockfile gate in scripts/lockfiles.sh"
    fi
  done
  while IFS= read -r m; do
    n=$((n + 1))
    if cargo metadata --locked --format-version 1 \
         --manifest-path "$root/$m" >/dev/null 2>"$err"; then
      echo "lockfiles: locked ok: $m"
    else
      echo "lockfiles: NOT locked: $m" >&2
      sed 's/^/  /' "$err" >&2
      bad=1
    fi
  done < <(tracked "$root" Cargo.toml)
  rm -f "$err"
  [ "$n" -gt 0 ] || die "no tracked Cargo.toml under $root: nothing was checked"
  [ "$bad" = 0 ] || die "a manifest does not resolve with --locked"
  echo "lockfiles: $n manifest(s) resolve with --locked"
}

audit() {
  local root=${1:-.} l n=0 bad=0
  while IFS= read -r l; do
    n=$((n + 1))
    if cargo audit --file "$root/$l"; then
      echo "lockfiles: audit ok: $l"
    else
      echo "lockfiles: AUDIT FAILED: $l" >&2
      bad=1
    fi
  done < <(tracked "$root" Cargo.lock)
  [ "$n" -gt 0 ] || die "no tracked Cargo.lock under $root: nothing was audited"
  [ "$bad" = 0 ] || die "an audited lockfile has findings"
  echo "lockfiles: $n lockfile(s) audited"
}

# --- planted controls --------------------------------------------------------

crate() { # DIR NAME DEP: a one-file crate with one registry dependency
  mkdir -p "$1/src"
  printf '[package]\nname = "%s"\nversion = "0.0.0"\nedition = "2021"\n\n[dependencies]\n%s\n' \
    "$2" "$3" >"$1/Cargo.toml"
  echo 'fn main() {}' >"$1/src/main.rs"
}

expect_fail() { # LABEL PATTERN CMD...: CMD must exit 1 and print PATTERN
  local label=$1 pat=$2 out rc=0
  shift 2
  out=$("$@" 2>&1) || rc=$?
  [ "$rc" = 1 ] || { echo "$out"; die "control $label: exit $rc, expected 1"; }
  grep -q -- "$pat" <<<"$out" || { echo "$out"; die "control $label: no '$pat' in output"; }
  echo "lockfiles: control $label rejected as expected"
}

controls() {
  local self t
  self=$(cd "$(dirname "$0")" && pwd)/$(basename "$0")
  t=$(mktemp -d)
  trap 'rm -rf "$t"' RETURN
  git init -q "$t"
  crate "$t" planted 'itoa = "=1.0.10"'
  (cd "$t" && cargo generate-lockfile -q && git add -A)

  # Positive control: the clean tree passes both gates.
  "$self" locked "$t" >/dev/null || die "control clean: locked gate rejected a clean tree"
  "$self" audit "$t" >/dev/null || die "control clean: audit rejected a clean tree"
  echo "lockfiles: control clean accepted"

  # 1. A dependency changed without updating the lock.
  crate "$t" planted 'itoa = "=1.0.11"'
  expect_fail stale-lock "NOT locked: Cargo.toml" "$self" locked "$t"
  crate "$t" planted 'itoa = "=1.0.10"'

  # 2. A nested workspace whose lockfile is not committed (gitignored).
  crate "$t/fuzz" fuzz 'itoa = "=1.0.10"'
  printf '\n[workspace]\n' >>"$t/fuzz/Cargo.toml"
  # As in a CI checkout: the ignored lockfile is not on disk.
  echo 'fuzz/Cargo.lock' >"$t/.gitignore"
  (cd "$t" && git add -A)
  expect_fail missing-nested-lock "NOT locked: fuzz/Cargo.toml" "$self" locked "$t"
  rm -rf "$t/fuzz" "$t/.gitignore"
  (cd "$t" && git add -A)

  # 3. A nested lockfile that pins a crate with a RustSec advisory
  #    (time 0.1.45: RUSTSEC-2020-0071). The root lock stays clean, so only
  #    the nested one can turn the audit red.
  mkdir -p "$t/sub"
  cat >"$t/sub/Cargo.lock" <<'EOF'
version = 3

[[package]]
name = "time"
version = "0.1.45"
source = "registry+https://github.com/rust-lang/crates.io-index"
EOF
  (cd "$t" && git add -A)
  expect_fail nested-advisory "RUSTSEC-2020-0071" "$self" audit "$t"
}

case ${1:-} in
  locked) locked "${2:-.}" ;;
  audit) audit "${2:-.}" ;;
  controls) controls ;;
  *) echo "usage: lockfiles.sh locked [ROOT] | audit [ROOT] | controls" >&2; exit 2 ;;
esac
