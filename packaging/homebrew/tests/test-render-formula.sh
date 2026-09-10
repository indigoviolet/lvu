#!/usr/bin/env bash
# Fixture tests for packaging/homebrew/render-formula.sh. Hermetic: only
# bash, awk, sed, grep and diff over in-repo fixtures. No network, no brew,
# no cargo. Run: bash packaging/homebrew/tests/test-render-formula.sh
set -euo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
renderer="$here/../render-formula.sh"
fixtures="$here/fixtures"
expected="$here/expected"
scratch=$(mktemp -d "${TMPDIR:-/tmp}/render-formula-test-XXXXXXXX")
trap 'rm -rf "$scratch"' EXIT INT TERM

pass=0; fail=0
check() { # check <name> <command...>: exit status decides
  if "$@" >/dev/null 2>&1; then pass=$((pass + 1)); echo "PASS: $1";
  else fail=$((fail + 1)); echo "FAIL: $1"; fi
}
check_output() { # check_output <name> <expected-file> <command...>
  # Compares BOTH the renderer exit status and its stdout: an output match
  # with a nonzero exit (or vice versa) is a failure, not a pass.
  local name="$1" golden="$2"; shift 2
  local rc=0
  "$@" >"$scratch/out.rb" 2>/dev/null || rc=$?
  if [ "$rc" -ne 0 ]; then
    fail=$((fail + 1)); echo "FAIL: $name (renderer exit $rc)"
    return 0
  fi
  if diff -q "$golden" "$scratch/out.rb" >/dev/null; then
    pass=$((pass + 1)); echo "PASS: $name"
  else
    fail=$((fail + 1)); echo "FAIL: $name (output differs)"
    diff -u "$golden" "$scratch/out.rb" | head -20
  fi
}
expect_fail_with() { # expect_fail_with <name> <message-fragment> <command...>
  local name="$1" fragment="$2"; shift 2
  local out rc=0
  out=$("$@" 2>&1) || rc=$?
  if [ "$rc" -ne 0 ] && printf '%s\n' "$out" | grep -q "$fragment"; then
    pass=$((pass + 1)); echo "PASS: $name"
  else
    fail=$((fail + 1)); echo "FAIL: $name (rc=$rc)"
  fi
}

# 1. Default render of the three supported archives: byte-exact golden.
check_output "default-3target" "$expected/expected-3target.rb" \
  "$renderer" 9.9.9 "$fixtures/SHA256SUMS-3target"

# 2. A four-archive SHA256SUMS still renders the three-target formula by
#    default: the Intel archive is ignored, never installed.
check_output "default-ignores-intel-archive" "$expected/expected-3target.rb" \
  "$renderer" 9.9.9 "$fixtures/SHA256SUMS-4target"

# 3. Explicit historical path: --with-intel-darwin restores all four blocks
#    with substituted checksums.
check_output "historical-intel-flag" "$expected/expected-4target-intel.rb" \
  "$renderer" 9.9.9 "$fixtures/SHA256SUMS-4target" --with-intel-darwin

# 4. Missing required target is a refusal, not a placeholder.
expect_fail_with "missing-required-refused" "has no lvu-9.9.9-aarch64-unknown-linux-musl" \
  "$renderer" 9.9.9 "$fixtures/SHA256SUMS-missing-required"

# 5. Explicit --allow-missing keeps working for a required target.
"$renderer" 9.9.9 "$fixtures/SHA256SUMS-missing-required" \
  --allow-missing aarch64-unknown-linux-musl > "$scratch/allowmissing.rb" 2>/dev/null
if grep -q "lvu-9.9.9-x86_64-unknown-linux-musl" "$scratch/allowmissing.rb" \
  && ! grep -q "aarch64-unknown-linux-musl" "$scratch/allowmissing.rb" \
  && ! grep -q "@@" "$scratch/allowmissing.rb"; then
  pass=$((pass + 1)); echo "PASS: allow-missing-required"
else
  fail=$((fail + 1)); echo "FAIL: allow-missing-required"
fi

# 6. Empty SHA256SUMS fails rather than rendering an empty formula.
printf '' > "$scratch/empty.SHA256SUMS"
expect_fail_with "empty-sums-refused" "SHA256SUMS is empty" \
  "$renderer" 9.9.9 "$scratch/empty.SHA256SUMS"

echo "result: $pass passed, $fail failed"
[ "$fail" = 0 ]
