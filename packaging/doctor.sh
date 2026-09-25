#!/usr/bin/env bash
# Kokoro Kindle Reader - developer toolchain check (Linux). Reports which build tools are
# missing, all in one pass. Tools only: whether native-deps/linux/ is provisioned is checked
# by fetch-deps.py and build.rs, not here.
#
#   ./packaging/doctor.sh
#
# Shell rather than Python so it can report a missing Python.

set -u

failed=0
if [ -t 1 ]; then G=$'\033[32m' R=$'\033[31m' N=$'\033[0m'; else G= R= N=; fi

ok()   { printf '%s[+]%s %s (%s)\n' "$G" "$N" "$1" "$2"; }
fail() { printf '%s[x]%s %s - %s\n' "$R" "$N" "$1" "$2"; failed=$((failed + 1)); }
have() { command -v "$1" >/dev/null 2>&1; }
# First version number the tool prints, or nothing if it is absent or fails.
version() { have "$1" && "$@" 2>/dev/null | grep -Eo '[0-9]+\.[0-9]+(\.[0-9]+)?' | head -n1; }

echo

# Checks run in install order: on a fresh machine each one is needed before the next.
git=$(version git --version)
lfs=$(version git lfs version)
if [ -z "$git" ]; then fail Git "install git"
elif [ -z "$lfs" ]; then fail "Git LFS" "install git-lfs, then: git lfs install && git lfs pull"
else ok Git "git $git, git-lfs $lfs"; fi

# espeak-ng is built from source (its cmake enables C++), and cargo links with the C compiler.
cc=""; for c in cc gcc clang; do have "$c" && { cc=$c; break; }; done
cxx=""; for c in c++ g++ clang++; do have "$c" && { cxx=$c; break; }; done
if [ -n "$cc" ] && [ -n "$cxx" ] && have make; then
    ok "C/C++ toolchain" "$cc, $cxx, make"
else
    fail "C/C++ toolchain" "install gcc, g++ and make (e.g. build-essential)"
fi

cmake=$(version cmake --version)
if [ -n "$cmake" ]; then ok CMake "$cmake"; else fail CMake "install cmake"; fi

py=$(version python3 --version)
if [ -n "$py" ]; then ok Python "$py"; else fail "Python 3" "install python3"; fi

cargo=$(version cargo --version)
if [ -n "$cargo" ]; then ok Rust "cargo $cargo"; else fail Rust "install from https://rustup.rs"; fi

bun=$(version bun --version)
if [ -n "$bun" ]; then ok bun "$bun"
else fail bun "curl -fsSL https://bun.sh/install | bash"; fi

echo
if [ "$failed" -gt 0 ]; then printf '%s%d missing.%s\n' "$R" "$failed" "$N"; exit 1; fi
exit 0
