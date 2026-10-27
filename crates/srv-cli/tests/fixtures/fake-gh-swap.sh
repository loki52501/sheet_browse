#!/bin/sh
# Fake gh for accept_t15b (Unix; `sh fake-gh-swap.sh <log> <gh args>`). "alias list" prints
# "<log>.aliases", then replaces it with "<log>.aliases2" when that exists, so the second
# listing (in call) differs from the first (in classify). Anything else appends its args to
# "<log>.ran" and echoes them.
log="$1"
shift
if [ "$1" = "alias" ] && [ "$2" = "list" ]; then
  if [ -f "$log.aliases" ]; then
    cat "$log.aliases"
  fi
  if [ -f "$log.aliases2" ]; then
    mv -f "$log.aliases2" "$log.aliases"
  fi
  exit 0
fi
echo "$*" >> "$log.ran"
echo "fake gh: $*"
