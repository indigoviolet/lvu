#!/bin/sh
#USAGE arg "[file]..." double_dash="preserve" help="Log files to open (use ./ for paths starting with a dash)"
#USAGE flag "--file <path>" var=#true help="Add a file source; repeatable"
#USAGE flag "-c --command <command>" var=#true help="Add a shell command source; repeatable"
#USAGE flag "--stdin" help="Capture standard input (redirected input is detected automatically)"
#USAGE flag "--resume" help="Re-acquire the most recent session's sources in this capture root (the default)"
#USAGE flag "--fresh" help="Start with no sources acquired; captured data stays in the workspace and nothing is deleted"
#USAGE flag "--capture-dir <directory>" help="Override the durable capture directory"

set -eu
exec "$(dirname "$0")/../versions/latest/bin/lvu" "$@"
