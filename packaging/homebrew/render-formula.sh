#!/usr/bin/env bash
# Render the tap formula from lvu.rb.in, taking every checksum from a release's
# SHA256SUMS rather than from a human.
#
#   packaging/homebrew/render-formula.sh 0.1.0 SHA256SUMS > Formula/lvu.rb
#   gh release download v0.1.0 -p SHA256SUMS -O - | \
#       packaging/homebrew/render-formula.sh 0.1.0 - > Formula/lvu.rb
#
# The supported set is Linux x86_64/arm64 and Apple-silicon macOS. Intel
# macOS left the set: its archive is ignored and its formula block dropped
# unless --with-intel-darwin is passed, which restores the historical
# four-target behavior for re-rendering older releases.
#
# A target whose archive is absent from SHA256SUMS is a failure, not a
# placeholder: a formula that keeps an unresolved token is unloadable, and one
# that keeps a stale checksum is worse. If a runner failed and you mean to ship
# without that platform, pass --allow-missing TARGET and the formula is
# rendered with that platform's block removed.
set -euo pipefail

usage() {
    cat <<'USAGE'
usage: render-formula.sh VERSION SHA256SUMS [--allow-missing TARGET ...] [--with-intel-darwin]

  VERSION       release version without the leading v, e.g. 0.1.0
  SHA256SUMS    checksum file from the release, or - for stdin
  --allow-missing TARGET
                drop TARGET's platform block instead of failing when the
                release has no archive for it (repeatable)
  --with-intel-darwin
                restore the historical x86_64-apple-darwin target: require
                its archive like any other target (still subject to
                --allow-missing). Without this flag the Intel macOS block is
                always dropped and any Intel archive is ignored.
USAGE
}

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
template="$here/lvu.rb.in"

[ $# -ge 2 ] || { usage >&2; exit 2; }
version="$1"; shift
sums_path="$1"; shift

allow_missing=()
with_intel=0
while [ $# -gt 0 ]; do
    case "$1" in
        --allow-missing) allow_missing+=("$2"); shift 2 ;;
        --with-intel-darwin) with_intel=1; shift ;;
        --help) usage; exit 0 ;;
        *) echo "render-formula.sh: unknown option $1" >&2; usage >&2; exit 2 ;;
    esac
done

case "$version" in
    v*) echo "render-formula.sh: pass the version without the leading v" >&2; exit 2 ;;
esac

if [ "$sums_path" = "-" ]; then sums=$(cat); else sums=$(cat "$sums_path"); fi
[ -n "$sums" ] || { echo "render-formula.sh: SHA256SUMS is empty" >&2; exit 1; }

targets=(x86_64-unknown-linux-musl aarch64-unknown-linux-musl aarch64-apple-darwin)
[ "$with_intel" = 1 ] && targets+=(x86_64-apple-darwin)

is_allowed_missing() {
    local target="$1" allowed
    for allowed in ${allow_missing+"${allow_missing[@]}"}; do
        [ "$allowed" = "$target" ] && return 0
    done
    return 1
}

# Delete the on_arm/on_intel block whose url names TARGET, printing the rest.
drop_target_block() {
    awk -v target="$1" '
        /^    on_(arm|intel) do$/ { buffered = $0 "\n"; inblock = 1; next }
        inblock { buffered = buffered $0 "\n"
                  if ($0 ~ /^    end$/) {
                      if (index(buffered, target) == 0) printf "%s", buffered
                      inblock = 0
                  }
                  next }
        { print }
    '
}

# `shasum`/`sha256sum` write "<hex>  <name>"; the name may carry a directory.
checksum_for() {
    awk -v archive="lvu-$version-$1.tar.gz" '
        { name = $NF; sub(/^.*\//, "", name); sub(/^\*/, "", name)
          if (name == archive) { print $1; found = 1 } }
        END { exit found ? 0 : 1 }
    ' <<<"$sums"
}

rendered=$(cat "$template")
rendered=${rendered//@@VERSION@@/$version}

for target in "${targets[@]}"; do
    token="@@SHA256_$(echo "$target" | tr 'a-z-' 'A-Z_')@@"
    if sum=$(checksum_for "$target"); then
        case "$sum" in
            [0-9a-f]*) [ "${#sum}" -eq 64 ] || { echo "render-formula.sh: $target checksum is not 64 hex characters: $sum" >&2; exit 1; } ;;
            *) echo "render-formula.sh: $target checksum is not lowercase hex: $sum" >&2; exit 1 ;;
        esac
        rendered=${rendered//$token/$sum}
    elif is_allowed_missing "$target"; then
        echo "render-formula.sh: dropping $target; it is not in SHA256SUMS" >&2
        rendered=$(printf '%s\n' "$rendered" | drop_target_block "$target")
    else
        echo "render-formula.sh: the release has no lvu-$version-$target.tar.gz" >&2
        echo "  pass --allow-missing $target only if you mean to ship without it" >&2
        exit 1
    fi
done

if [ "$with_intel" = 0 ]; then
    # Intel macOS is out of the supported set: drop its block even if an
    # Intel archive happens to be listed, so the default formula never
    # names an unsupported install target. Historical four-target
    # re-renders pass --with-intel-darwin.
    echo "render-formula.sh: dropping x86_64-apple-darwin; not in the supported set (pass --with-intel-darwin for historical releases)" >&2
    rendered=$(printf '%s\n' "$rendered" | drop_target_block x86_64-apple-darwin)
fi

# Dropping every target under an `on_macos`/`on_linux` wrapper leaves a wrapper
# that installs nothing, which rubocop reports as an empty block. The test is
# whether the wrapper still contains a `url`, not whether it is literally
# empty: a wrapper can be left holding only the comment that documented the
# blocks just removed. The blank line that followed it goes too, so the
# rendered formula stays style-clean.
rendered=$(printf '%s\n' "$rendered" | awk '
    /^  on_(macos|linux) do$/ { buffered = $0 "\n"; inwrapper = 1; next }
    inwrapper {
        buffered = buffered $0 "\n"
        if ($0 ~ /^  end$/) {
            if (buffered ~ /\n      url /) printf "%s", buffered
            else drop_blank = 1
            inwrapper = 0
        }
        next
    }
    drop_blank { drop_blank = 0; if ($0 == "") next }
    { print }
')

case "$rendered" in
    *@@*) echo "render-formula.sh: unresolved token left in the formula" >&2; exit 1 ;;
esac

# A formula with no url at all installs nothing on any platform.
printf '%s\n' "$rendered" | grep -q '^      url ' \
    || { echo "render-formula.sh: every platform was dropped; there is nothing to install" >&2; exit 1; }

# Strip the template's own preamble; the rendered formula is not a template.
printf '%s\n' "$rendered" | sed '1,/^### END TEMPLATE PREAMBLE$/d' | {
    cat <<EOF
# lvu $version, rendered by packaging/homebrew/render-formula.sh from the
# release's SHA256SUMS. Edit packaging/homebrew/lvu.rb.in in the lvu repository
# and re-render; hand edits here are lost on the next release.
#
# No bottles: the archives are prebuilt executables, so there is nothing to
# compile. The archive is relocatable, so bin/lvu resolves libexec/lvu/* from
# its own canonicalized path and Homebrew's bin symlink works unchanged.
EOF
    cat
}
