#!/usr/bin/env bash
# Kokoro Kindle Reader - developer toolchain check (Linux). Reports which build tools are
# missing, all in one pass. Tools only: whether native-deps/linux/ is provisioned is checked
# by fetch-deps.py and build.rs, not here.
#
#   ./doctors/doctor.sh
#
# WHAT is checked (names, version rules, fixes, order) is in tools.conf, shared with
# doctor.cmd; this file only knows HOW to find each tool - one detect_<id> per row.
# Shell rather than Python so it can report a missing Python.

set -u

here=$(cd "$(dirname "$0")" && pwd)
failed=0
if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
    G=$'\033[32m' R=$'\033[31m' Y=$'\033[33m' N=$'\033[0m'
else
    G= R= Y= N=
fi

have() { command -v "$1" >/dev/null 2>&1; }
# First version number the tool prints, or nothing if it is absent or fails.
version() { have "$1" && "$@" 2>/dev/null | grep -Eo '[0-9]+\.[0-9]+(\.[0-9]+)?' | head -n1; }
# The first three dotted parts as one comparable number; missing parts count as 0.
vnum() { printf '%s\n' "$1" | awk -F'[.+-]' '{ printf "%d\n", $1 * 1000000 + $2 * 1000 + $3 }'; }
# Is VER within the rule (>=X or =X)?
ver_ok() {
    case $1 in
        '>='*) [ "$(vnum "$VER")" -ge "$(vnum "${1#>=}")" ] ;;
        '='*)  [ "$(vnum "$VER")" -eq "$(vnum "${1#=}")" ] ;;
    esac
}

# --- detectors: one per tools.conf id that Linux uses. Each sets FOUND, and optionally VER
# and DETAIL.
detect_git()     { VER=$(version git --version);     [ -n "$VER" ] && FOUND=1; }
detect_git_lfs() { VER=$(version git lfs version);   [ -n "$VER" ] && FOUND=1; }
# Git LFS installed is not the files downloaded: ls-files marks a file still checked out as a
# pointer with '-' (and '*' once it is real).
detect_lfs_files() {
    local out
    out=$(git -C "$here/.." lfs ls-files 2>/dev/null) || return 0
    printf '%s\n' "$out" | grep -q ' - ' || FOUND=1
}
detect_cmake()   { VER=$(version cmake --version);   [ -n "$VER" ] && FOUND=1; }
detect_python()  { VER=$(version python3 --version); [ -n "$VER" ] && FOUND=1; }
detect_rust()    { VER=$(version cargo --version);   [ -n "$VER" ] && FOUND=1; }
# The corresponding-source archive carries the standard-library source the host links.
detect_rust_src() {
    local sysroot
    sysroot=$(rustc --print sysroot 2>/dev/null) || return 0
    [ -f "$sysroot/lib/rustlib/src/rust/library/std/Cargo.toml" ] && FOUND=1
}
detect_bun()     { VER=$(version bun --version);     [ -n "$VER" ] && FOUND=1; }
detect_cargo_about() { VER=$(version cargo about --version); [ -n "$VER" ] && FOUND=1; }
# The package this distribution gets, by the same os-release family build_installer.py reads
# (target_platform.LINUX_FAMILY_FORMAT): a .deb from dpkg-deb, its Depends from
# dpkg-shlibdeps (dpkg-dev); an Arch package from makepkg, its depends from readelf +
# ldconfig + pacman (base-devel). Any other family has no package, so nothing satisfies it.
detect_linux_pkg() {
    local ids="" f
    for f in /etc/os-release /usr/lib/os-release; do
        [ -r "$f" ] && { ids=$(. "$f" && printf '%s %s' "${ID:-}" "${ID_LIKE:-}"); break; }
    done
    for f in $ids; do
        case $f in
            debian) have dpkg-deb && have dpkg-shlibdeps &&
                        { FOUND=1; DETAIL="dpkg-dev, for a .deb"; }; return ;;
            arch) have makepkg && have readelf && have ldconfig && have pacman &&
                      { FOUND=1; DETAIL="makepkg, for an Arch package"; }; return ;;
        esac
    done
}
# espeak-ng is built from source (its cmake enables C++), and cargo links with the C compiler.
detect_cc() {
    local cc="" cxx="" c
    for c in cc gcc clang; do have "$c" && { cc=$c; break; }; done
    for c in c++ g++ clang++; do have "$c" && { cxx=$c; break; }; done
    [ -n "$cc" ] && [ -n "$cxx" ] && have make && { FOUND=1; DETAIL="$cc, $cxx, make"; }
}

echo
while IFS='|' read -r id name rule level _win fix _rest <&3; do
    case $id in ''|'#'*) continue ;; esac
    [ "$fix" = - ] && continue
    FOUND= VER= DETAIL=
    "detect_$id"
    if [ -z "$FOUND" ]; then
        if [ "$level" = optional ]; then
            printf '%s[!]%s %s - %s\n' "$Y" "$N" "$name" "$fix"
        else
            printf '%s[x]%s %s - %s\n' "$R" "$N" "$name" "$fix"
            failed=$((failed + 1))
        fi
    elif [ "$rule" != - ] && ! ver_ok "$rule"; then
        printf '%s[x]%s %s %s, need %s - %s\n' "$R" "$N" "$name" "$VER" "$rule" "$fix"
        failed=$((failed + 1))
    else
        info=$VER
        [ -n "$DETAIL" ] && info=${info:+$info, }$DETAIL
        if [ -n "$info" ]; then
            printf '%s[+]%s %s (%s)\n' "$G" "$N" "$name" "$info"
        else
            printf '%s[+]%s %s\n' "$G" "$N" "$name"
        fi
    fi
done 3< "$here/tools.conf"

if [ "$failed" -gt 0 ]; then
    echo
    printf '%s%d missing.%s\n' "$R" "$failed" "$N"
    exit 1
fi
exit 0
