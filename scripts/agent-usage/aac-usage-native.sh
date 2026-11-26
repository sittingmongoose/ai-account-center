#!/bin/sh
# Shared-folder launcher for the installed native offline reader.
if [ -n "${AAC_USAGE_DIR:-}" ]; then
  exec ai-account-center usage "$@"
fi
case "$0" in
  */*) reader_dir=${0%/*}; [ -n "$reader_dir" ] || reader_dir=/ ;;
  *) reader_dir=. ;;
esac
reader_dir=$(CDPATH= cd -P "$reader_dir" && pwd) || exit 1
exec ai-account-center usage --dir "$reader_dir" "$@"
