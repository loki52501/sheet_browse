#!/bin/sh
# Fake gh for accept_t15b (Unix; `sh fake-gh-page.sh <page> <gh args>`). "alias list" prints
# nothing; any other call prints the page to stdout, or to stderr and exits 1 when
# "<page>.fail" exists. When "<page>.aliasfail" exists, "alias list" fails (exit 1).
page="$1"
shift
if [ "$1" = "alias" ]; then
  if [ -f "$page.aliasfail" ]; then
    echo "alias list failed" >&2
    exit 1
  fi
  exit 0
fi
if [ -f "$page.fail" ]; then
  cat "$page" >&2
  exit 1
fi
cat "$page"
