#!/bin/sh
# Linker shim: lets the installed zig link Rust's x86_64-pc-windows-gnu output.
# - drops gcc-only libs/flags zig doesn't know
# - renames ___chkstk_ms in a temp copy of compiler_builtins (zig's compiler_rt also defines it)
HOST="$(rustc -vV | sed -n 's/^host: //p')"
OBJCOPY="$(rustc --print sysroot)/lib/rustlib/$HOST/bin/rust-objcopy"
TMP="${TMPDIR:-/tmp}/zig-win-cc.$$"
mkdir -p "$TMP"
trap 'rm -rf "$TMP"' EXIT
for a in "$@"; do
  case "$a" in
    -lgcc_eh) set -- "$@" -lunwind ;;
    -lgcc_s|-lgcc|-l:libpthread.a|-Wl,--disable-auto-image-base) ;;
    *libcompiler_builtins-*.rlib)
      cp "$a" "$TMP/cb.rlib"
      "$OBJCOPY" --redefine-sym ___chkstk_ms=___chkstk_ms_rust "$TMP/cb.rlib" 2>/dev/null
      set -- "$@" "$TMP/cb.rlib" ;;
    *) set -- "$@" "$a" ;;
  esac
  shift
done
zig cc -target x86_64-windows-gnu "$@"
