#!/bin/sh
# Fake gh for srv-cli tests (Unix; run as `sh fake-gh.sh <log> <gh args>`). "run view <id>
# --log" prints the log the test chose; "alias list" prints "<log>.aliases" if it exists
# (else nothing); anything else echoes its arguments.
log="$1"
shift
if [ "$1" = "alias" ] && [ "$2" = "list" ]; then
  if [ -f "$log.aliases" ]; then
    cat "$log.aliases"
  fi
  exit 0
fi
if [ "$1" = "run" ] && [ "$2" = "view" ] && [ "$4" = "--log" ]; then
  cat "$log"
  exit $?
fi
echo "fake gh: $*"
