#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "$0")/../.." && pwd)
fixture=$(mktemp -d "${TMPDIR:-/tmp}/lvu-sol-stage-profile-XXXXXX")
marker="$fixture/.lvu-test-reproducer"
touch "$marker"

cleanup() {
    [ -f "$marker" ] || { echo "refusing to remove unmarked fixture $fixture" >&2; return 1; }
    case "$fixture" in
        "${TMPDIR:-/tmp}"/lvu-sol-stage-profile-*) rm -rf -- "$fixture" ;;
        *) echo "refusing to remove unexpected fixture $fixture" >&2; return 1 ;;
    esac
}
trap cleanup EXIT

mkdir -p "$fixture/bin"
printf '%s\n' \
    '#!/usr/bin/env bash' \
    'printf "%s\n" "$@" > "$CARGO_ARGS_LOG"' \
    'exit 97' > "$fixture/bin/cargo"
chmod +x "$fixture/bin/cargo"

probe_profile() {
    profile=$1
    expected=$2
    log="$fixture/$profile.args"
    set +e
    PATH="$fixture/bin:$PATH" CARGO_ARGS_LOG="$log" \
        bash "$repo_root/packaging/stage.sh" \
        --profile "$profile" --target test-target --out "$fixture/$profile-stage" \
        >/dev/null 2>"$fixture/$profile.stderr"
    status=$?
    set -e
    [ "$status" -eq 97 ] || {
        echo "stage profile $profile exited $status instead of reaching cargo" >&2
        cat "$fixture/$profile.stderr" >&2
        return 1
    }
    diff -u "$expected" "$log"
}

printf '%s\n' build -p lvu-app --locked --target test-target > "$fixture/dev.expected"
printf '%s\n' build -p lvu-app --locked --target test-target --release > "$fixture/release.expected"
probe_profile dev "$fixture/dev.expected"
probe_profile release "$fixture/release.expected"
echo "stage profile arguments preserve empty dev and nonempty release cases"
