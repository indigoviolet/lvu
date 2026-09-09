#!/bin/sh
# Compiler/server scratch outlives disposable test fixtures. The first caller
# starts the shared sccache daemon, which retains its inherited TMPDIR.
set -eu
export TMPDIR="${LVU_COMPILER_TMPDIR:-${SCCACHE_DIR:?SCCACHE_DIR must be set}/tmp}"
mkdir -p "$TMPDIR"
exec sccache "$@"
