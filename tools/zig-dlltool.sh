#!/bin/sh
# Translate GNU dlltool flags from rustc into llvm-dlltool flags (zig dlltool).
skip=0
for a in "$@"; do
  if [ $skip = 1 ]; then skip=0; shift; continue; fi
  case "$a" in
    -f|--64|--no-leading-underscore) ;;
    --temp-prefix) skip=1 ;;
    *) set -- "$@" "$a" ;;
  esac
  shift
done
exec zig dlltool "$@"
