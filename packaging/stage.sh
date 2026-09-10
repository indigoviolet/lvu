#!/usr/bin/env bash
# Build and stage a relocatable lvu archive, then prove the staged tree
# resolves its own resources.
#
# The staged layout is exactly what crates/lvu-app/src/resources.rs looks for:
#
#   lvu-<version>-<target>/
#     bin/lvu                        the real application (crate binary lvu-app)
#     libexec/lvu/python/            pinned Polars expression helper project
#     libexec/lvu/bridge/            built bridge with production node_modules
#     share/doc/lvu/                 README and distribution notes
#
# Verification runs the staged binary from an unrelated working directory and
# again through a symlink, and checks that resolution reports the staged tree
# rather than the checkout that produced the binary.
set -euo pipefail

usage() {
    cat <<'USAGE'
usage: packaging/stage.sh [options]

  --out DIR         staging root (default: target/packaging)
  --target TRIPLE   target triple label (default: the host triple)
  --profile NAME    cargo profile: release (default) or dev
  --skip-bridge     stage without the bridge; 🧠 assistance stays unavailable
  --archive         also write a .tar.gz and a .sha256 next to the staged tree
  --help            show this message
USAGE
}

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
out=""
target=""
profile="release"
skip_bridge=0
archive=0

while [ $# -gt 0 ]; do
    case "$1" in
        --out) out="$2"; shift 2 ;;
        --target) target="$2"; shift 2 ;;
        --profile) profile="$2"; shift 2 ;;
        --skip-bridge) skip_bridge=1; shift ;;
        --archive) archive=1; shift ;;
        --help) usage; exit 0 ;;
        *) echo "stage.sh: unknown option $1" >&2; usage >&2; exit 2 ;;
    esac
done

run() { echo "+ $*" >&2; "$@"; }

version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$repo_root/crates/lvu-app/Cargo.toml" | head -1)
[ -n "$version" ] || { echo "stage.sh: cannot read the lvu-app version" >&2; exit 1; }
[ -n "$target" ] || target=$(rustc -vV | sed -n 's/^host: //p')
[ -n "$out" ] || out="$repo_root/target/packaging"

case "$profile" in
    release) set -- --release; profile_dir="release" ;;
    dev|debug) set --; profile_dir="debug" ;;
    *) echo "stage.sh: unsupported profile $profile" >&2; exit 2 ;;
esac

name="lvu-$version-$target"
staged="$out/$name"
rm -rf "$staged"
mkdir -p "$staged/bin" "$staged/libexec/lvu" "$staged/share/doc/lvu"

echo "==> building lvu-app ($profile) for $target" >&2
# The triple is passed to cargo rather than used only as a filename, so an
# archive cannot be named for a platform it was not built for. Cargo puts an
# explicitly targeted artifact under <target-dir>/<triple>/<profile>/.
run cargo build -p lvu-app --locked --target "$target" "$@"
# The workspace target directory can be shared with other checkouts, so the
# freshly linked artifact is copied immediately rather than referenced later.
binary="${CARGO_TARGET_DIR:-$repo_root/target}/$target/$profile_dir/lvu-app"
[ -x "$binary" ] || { echo "stage.sh: missing built binary $binary" >&2; exit 1; }
# The command is `lvu`; the workspace's `lvu` executable is a UI demo and is
# deliberately never packaged.
cp "$binary" "$staged/bin/lvu"
chmod 0755 "$staged/bin/lvu"

echo "==> staging the Python expression helper" >&2
mkdir -p "$staged/libexec/lvu/python"
cp "$repo_root/python/pyproject.toml" "$repo_root/python/uv.lock" "$staged/libexec/lvu/python/"
cp -R "$repo_root/python/lvu_expr_helper" "$staged/libexec/lvu/python/"
find "$staged/libexec/lvu/python" -name '__pycache__' -type d -prune -exec rm -rf {} +
# The packaged helper is never built in place: `uv run --project` makes
# setuptools write build/ and .egg-info into the directory the package manager
# owns. The lockfile is exported to hashed requirements instead, and the helper
# runs as a plain package on PYTHONPATH. resources.rs selects that form by the
# presence of this file.
run uv export --project "$repo_root/python" --no-dev --no-emit-project \
    --format requirements-txt -o "$staged/libexec/lvu/python/requirements.txt"

if [ "$skip_bridge" -eq 0 ]; then
    echo "==> staging the bridge with production dependencies" >&2
    [ -f "$repo_root/bridge/dist/cli.js" ] || {
        echo "stage.sh: bridge/dist/cli.js is missing; run 'mise run check:bridge' first" >&2
        exit 1
    }
    mkdir -p "$staged/libexec/lvu/bridge"
    cp "$repo_root/bridge/package.json" "$repo_root/bridge/package-lock.json" \
        "$staged/libexec/lvu/bridge/"
    cp -R "$repo_root/bridge/dist" "$staged/libexec/lvu/bridge/"
    # Locked production dependencies only; the archive carries no test tooling.
    run npm --prefix "$staged/libexec/lvu/bridge" ci --omit=dev --ignore-scripts
else
    echo "==> skipping the bridge (--skip-bridge)" >&2
fi

cp "$repo_root/README.md" "$staged/share/doc/lvu/"
cp "$repo_root/docs/distribution.md" "$staged/share/doc/lvu/"
# The crates declare "MIT OR Apache-2.0", so the archive carries both texts.
# They sit at the archive root, where a packager expects to find them, and are
# repeated under share/doc/lvu so an installed prefix keeps them beside the
# documentation. A missing text here is a licensing defect, not a warning.
for license in LICENSE LICENSE-MIT LICENSE-APACHE; do
    [ -f "$repo_root/$license" ] || {
        echo "stage.sh: missing $license; the crate declares MIT OR Apache-2.0" >&2
        exit 1
    }
    cp "$repo_root/$license" "$staged/$license"
    cp "$repo_root/$license" "$staged/share/doc/lvu/$license"
done

echo "==> verifying the staged tree" >&2
probe_root="$out/.verify"
rm -rf "$probe_root"
mkdir -p "$probe_root/elsewhere" "$probe_root/link" "$probe_root/empty"
ln -s "$staged/bin/lvu" "$probe_root/link/lvu"

fail() { echo "stage.sh: VERIFY FAILED: $*" >&2; exit 1; }

# 0. Every check below runs the staged binary. A build for a platform this
#    machine cannot execute must say so rather than look like a broken archive.
status=0
"$staged/bin/lvu" --help >/dev/null 2>&1 || status=$?
if [ "$status" -eq 126 ] || [ "$status" -eq 127 ]; then
    fail "the staged $target binary cannot be executed on this machine; stage.sh verifies an archive by running it, so build each target on a machine that can run it"
fi

# 1. --help from an unrelated working directory.
( cd "$probe_root/elsewhere" && "$staged/bin/lvu" --help ) >"$probe_root/help.txt"
# The packaged command is `lvu`; help that advertises the crate binary name
# `lvu-app` tells an installed user to run something that is not on their PATH.
grep -q '^Usage: lvu \[OPTIONS\]' "$probe_root/help.txt" || fail "--help did not print 'Usage: lvu [OPTIONS] ...'"
if grep -q 'lvu-app' "$probe_root/help.txt"; then fail "--help still names the crate binary lvu-app"; fi

# 2. resource resolution directly and through the symlink, from a cwd that is
#    unrelated to both the checkout and the staged tree.
for invocation in "$staged/bin/lvu" "$probe_root/link/lvu"; do
    report=$( cd "$probe_root/elsewhere" && "$invocation" --resources )
    echo "--- $invocation" >&2
    echo "$report" >&2
    echo "$report" | grep -q "origin: installed beside the executable" \
        || fail "$invocation did not resolve resources from the staged tree"
    case "$report" in
        *"$repo_root/python"*|*"$repo_root/bridge"*)
            fail "$invocation resolved back into the build checkout" ;;
    esac
    echo "$report" | grep -q "Python expression helper: found" \
        || fail "$invocation did not find the staged Python helper"
    if [ "$skip_bridge" -eq 0 ]; then
        echo "$report" | grep -q "agent bridge: found" \
            || fail "$invocation did not find the staged bridge"
    fi
done

# 3. A pinned but empty resource root must degrade with an actionable
#    diagnostic instead of silently using another location.
report=$( cd "$probe_root/elsewhere" \
    && LVU_RESOURCE_ROOT="$probe_root/empty" "$staged/bin/lvu" --resources )
echo "--- pinned empty resource root" >&2
echo "$report" >&2
echo "$report" | grep -q "Python expression helper: missing" \
    || fail "an empty pinned resource root still reported a helper"
echo "$report" | grep -q "no other location was tried" \
    || fail "a pinned override fell through to another location"

# 4. The staged helper must actually compile a Polars expression, and must not
#    write anything into the install prefix while doing it. uv provisions the
#    interpreter and locked dependencies; skipped when uv is unavailable.
if command -v uv >/dev/null 2>&1; then
    echo "--- staged helper compilation" >&2
    before=$(find "$staged/libexec/lvu/python" | sort)
    helper_command=$( "$staged/bin/lvu" --resources \
        | sed -n '/Python expression helper: found/,/^$/p' \
        | sed -n 's/^  command: //p' )
    [ -n "$helper_command" ] || fail "no helper command was reported"
    request='{"schema_version":1,"request_id":"stage","operation":"compile","kind":"filter","expression":"pl.col(\"raw\").str.contains(\"boot\")"}'
    # The helper's stderr is kept rather than discarded. Dropping it turned any
    # failure here into a bare non-zero exit with nothing to act on, which is
    # exactly the moment an operator needs to know what the subprocess said.
    response=$( cd "$probe_root/elsewhere" \
        && printf '%s\n' "$request" | eval "$helper_command" 2>"$probe_root/helper.err" | head -1 ) || true
    echo "$response" | cut -c1-160 >&2
    case "$response" in
        *'"ok":true'*) ;;
        *)
            echo "--- staged helper stderr ---" >&2
            cat "$probe_root/helper.err" >&2 || true
            fail "the staged helper did not compile a Polars expression" ;;
    esac
    after=$(find "$staged/libexec/lvu/python" | sort)
    [ "$before" = "$after" ] || fail "the staged helper wrote into the install prefix"
else
    echo "--- uv is unavailable; skipped the staged helper compilation" >&2
fi

# 5. The staged bridge must load its own bundled production dependencies and
#    run. What it must NOT require is a Paseo provider: `node dist/cli.js`
#    connects to ws://127.0.0.1:6767 at startup, so asserting a capabilities
#    response asserted that a Paseo daemon was listening on the build machine.
#    That passed on a development host and failed every release runner with
#    "Transport closed (code 1006)", which is a fact about the machine, not
#    about the archive. The archive is responsible for carrying a complete,
#    loadable bridge; whether an agent provider is reachable is not its
#    business, and lvu already reports assistance as unavailable when it is not.
if [ "$skip_bridge" -eq 0 ] && command -v node >/dev/null 2>&1; then
    echo "--- staged bridge bundled dependencies" >&2
    ( cd "$staged/libexec/lvu/bridge" \
        && node --input-type=module \
            -e 'await import("@getpaseo/client"); await import("zod");' ) \
        2>"$probe_root/bridge-deps.err" \
        || {
            echo "--- staged bridge dependency stderr ---" >&2
            cat "$probe_root/bridge-deps.err" >&2 || true
            fail "the staged bridge cannot load its bundled production dependencies"
        }
    echo "the staged bridge resolves @getpaseo/client and zod from its own node_modules" >&2

    echo "--- staged bridge capabilities" >&2
    # The provider URL is left at its default so that a machine which does have
    # a provider still exercises the full exchange; only the connect timeout is
    # shortened, so a machine without one reaches its answer in seconds rather
    # than waiting out the bridge's own ten.
    response=$( cd "$staged/libexec/lvu/bridge" \
        && printf '%s\n' '{"schema_version":1,"request_id":"stage","method":"capabilities"}' \
        | LVU_PASEO_CONNECT_TIMEOUT_MS=5000 node dist/cli.js \
            2>"$probe_root/bridge.err" | head -1 ) || true
    echo "$response" | cut -c1-160 >&2
    case "$response" in
        *'"ok":true'*)
            echo "the staged bridge answered capabilities against a reachable provider" >&2 ;;
        *)
            # The bridge constructed itself and got as far as the transport, so
            # the payload is complete and only the provider is absent. Anything
            # else -- a missing module, a syntax error -- is a broken archive.
            if grep -q '^bridge connection failed:' "$probe_root/bridge.err"; then
                echo "no agent provider is reachable here, so the capabilities exchange was skipped; the staged bridge started and failed only at the transport:" >&2
                sed 's/^/    /' "$probe_root/bridge.err" >&2
            else
                echo "--- staged bridge stderr ---" >&2
                cat "$probe_root/bridge.err" >&2 || true
                echo "--- staged bridge tree ---" >&2
                ls -la "$staged/libexec/lvu/bridge" >&2 || true
                node --version >&2 || true
                fail "the staged bridge did not answer capabilities and did not fail at the transport"
            fi ;;
    esac
elif [ "$skip_bridge" -eq 0 ]; then
    echo "--- node is unavailable; skipped the staged bridge check" >&2
fi

if [ "$archive" -eq 1 ]; then
    echo "==> writing the archive" >&2
    ( cd "$out" && tar -czf "$name.tar.gz" "$name" )
    ( cd "$out" && shasum -a 256 "$name.tar.gz" > "$name.tar.gz.sha256" 2>/dev/null \
        || sha256sum "$name.tar.gz" > "$name.tar.gz.sha256" )
    cat "$out/$name.tar.gz.sha256"
fi

echo "staged and verified: $staged" >&2
