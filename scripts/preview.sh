#!/bin/sh
#USAGE arg "[file]..." double_dash="preserve" help="Log files to open (use ./ for paths starting with a dash)"
#USAGE flag "--file <path>" var=#true help="Add a file source; repeatable"
#USAGE flag "-c --command <command>" var=#true help="Add a shell command source; repeatable"
#USAGE flag "--stdin" help="Capture standard input (redirected input is detected automatically)"
#USAGE flag "--capture-dir <directory>" help="Override the durable capture directory"

set -eu
exec "$(dirname "$0")/../previews/latest/lvu" "$@"
