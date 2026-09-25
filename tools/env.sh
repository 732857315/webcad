# shellcheck shell=bash
# Put the portable Rust toolchain on PATH for the current shell.
#
#   . tools/env.sh
#
# No drive letters are hard-coded. The toolchain is looked up in order:
#   1. $PORTABLE_ROOT/Rust   (set by MoveCoding/Scripts/Use-PortableEnvironment.ps1)
#   2. <ancestor>/MoveCoding/Rust for the nearest ancestor directory of this repo
#   3. whatever cargo is already on PATH (CI runners, other machines)

_webcad_repo="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")/.." && pwd)"
_webcad_portable=""

if [ -n "${PORTABLE_ROOT:-}" ]; then
  _webcad_p="$PORTABLE_ROOT"
  command -v cygpath >/dev/null 2>&1 && _webcad_p="$(cygpath -u "$_webcad_p")"
  [ -d "$_webcad_p/Rust/cargo/bin" ] && _webcad_portable="$_webcad_p"
fi

if [ -z "$_webcad_portable" ]; then
  _webcad_d="$_webcad_repo"
  while :; do
    if [ -d "$_webcad_d/MoveCoding/Rust/cargo/bin" ]; then
      _webcad_portable="$_webcad_d/MoveCoding"
      break
    fi
    _webcad_parent="$(dirname "$_webcad_d")"
    [ "$_webcad_parent" = "$_webcad_d" ] && break
    _webcad_d="$_webcad_parent"
  done
fi

if [ -n "$_webcad_portable" ]; then
  # rustup/cargo are native Windows programs there: hand them D:/style paths.
  if command -v cygpath >/dev/null 2>&1; then
    RUSTUP_HOME="$(cygpath -m "$_webcad_portable/Rust/rustup")"
    CARGO_HOME="$(cygpath -m "$_webcad_portable/Rust/cargo")"
  else
    RUSTUP_HOME="$_webcad_portable/Rust/rustup"
    CARGO_HOME="$_webcad_portable/Rust/cargo"
  fi
  export RUSTUP_HOME CARGO_HOME
  case ":$PATH:" in
    *":$_webcad_portable/Rust/cargo/bin:"*) ;;
    *) export PATH="$_webcad_portable/Rust/cargo/bin:$PATH" ;;
  esac
  # GNU binutils (dlltool + as) from the portable MSYS2. The windows-gnu target needs them for crates
  # linked via raw-dylib (windows-link, getrandom). Appended so they never shadow other tools.
  if [ -x "$_webcad_portable/MSYS2/ucrt64/bin/dlltool.exe" ]; then
    case ":$PATH:" in
      *":$_webcad_portable/MSYS2/ucrt64/bin:"*) ;;
      *) export PATH="$PATH:$_webcad_portable/MSYS2/ucrt64/bin" ;;
    esac
  fi
fi

export CARGO_NET_RETRY="${CARGO_NET_RETRY:-10}"
# On Windows, schannel's certificate revocation lookup fails intermittently on this network
# (CRYPT_E_NO_REVOCATION_CHECK). Chain validation stays on; only the revocation lookup is skipped.
if command -v cygpath >/dev/null 2>&1; then
  export CARGO_HTTP_CHECK_REVOKE="${CARGO_HTTP_CHECK_REVOKE:-false}"
fi

if ! command -v cargo >/dev/null 2>&1; then
  echo "webcad: cargo not found. Install Rust into MoveCoding/Rust or put cargo on PATH." >&2
fi

unset _webcad_repo _webcad_portable _webcad_p _webcad_d _webcad_parent
