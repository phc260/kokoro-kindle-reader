#!/usr/bin/env bash
# Kokoro Kindle Reader - toolchain doctor (Linux). The POSIX-shell twin of doctor.ps1.
#
#   ./packaging/doctor.sh              # everything (app + extension)
#   ./packaging/doctor.sh app          # just build and run the Linux host
#   ./packaging/doctor.sh extension    # the browser extension's suite
#   ./packaging/doctor.sh -For app     # same, for muscle memory from the .ps1
#
# WHY THIS IS SHELL AND NOT PYTHON. Same reason as doctor.ps1: a doctor has to run on a
# machine that has NOTHING installed, and a Python script cannot report a missing Python -
# it needs one to start, so the user gets "python3: command not found" from the shell, which
# is not a diagnosis. bash is present on every Linux desktop this targets; the interpreters,
# compilers and cargo it looks for are the things that may not be.
#
# WHAT THIS IS THE TWIN OF, AND WHERE IT DIVERGES. The Linux build is the synth core plus the
# loopback HTTP endpoint and nothing else - no tray, no settings panel, no Kindle, and so no
# SAPI shim, hook or injector. There is therefore no x86 target to check (those three x86
# artifacts do not exist here), and no NSIS installer or release source archive (both are
# Windows-only), so the 'installer' and 'source' tiers are gone. What Linux adds instead is a
# CMake + C-toolchain check: espeak-ng is BUILT FROM SOURCE by native-deps/fetch-deps.py
# (a distribution's own libespeak-ng is unmodified and changes the phonemes, so it is not a
# substitute), and cargo drives the system C compiler as its linker.
#
# WHAT THIS REPORTS, AND WHAT IT DOES NOT. Presence and versions only - is there a thing
# called X, and what does it say its version is. It deliberately does NOT check the state of
# this checkout: whether the native dependencies are provisioned and current, whether the
# pinned inventories still match, whether icons/ are real images or unresolved LFS pointers.
# Those turn on facts the build owns; on Linux it is native-deps/fetch-deps.py and
# kokoro-host's build.rs that enforce them (build.rs panics if the linux/runtime tree is
# missing). **A green report here means the tools are installed, not that a build will get
# past provisioning.**
#
# SHAPE, after flutter doctor and doctor.ps1: one line per CATEGORY - "[status] Name - what it
# is for (what was found)" - and a category that is not ok expands underneath with the problem
# and the exact command that fixes it. A category groups tools installed together and failing
# together (cargo+rustc, git+git-lfs) and takes the WORST status among them. Markers are the
# same ASCII [+] [!] [x] [-] the .ps1 uses, so the two reports read alike.

set -u

FOR="all"

usage() {
    cat <<'EOF'
Usage: doctor.sh [TIER]
  TIER is one of:
    app         build and run the Linux host (kokoro-host: synth core + loopback endpoint)
    extension   work on the browser extension, which shares nothing with the rest
    all         both of the above (the default)
  -For / --for TIER is also accepted, to match doctor.ps1.
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        -h|--help) usage; exit 0 ;;
        -For|--for)
            shift
            [ $# -gt 0 ] || { echo "doctor.sh: -For needs a value" >&2; exit 2; }
            FOR="$1" ;;
        -For=*|--for=*) FOR="${1#*=}" ;;
        app|extension|all) FOR="$1" ;;
        installer|source)
            echo "doctor.sh: '$1' is a Windows-only tier - there is no Linux NSIS installer" >&2
            echo "or release source archive. Try: app | extension | all (or doctor.ps1 on Windows)." >&2
            exit 2 ;;
        *) echo "doctor.sh: unknown argument '$1'" >&2; usage; exit 2 ;;
    esac
    shift
done

# Tier implication: asking for one tier includes the cheaper ones it builds on. Only two tiers
# exist on Linux, and they share nothing, so this is simpler than the .ps1's chain.
case "$FOR" in
    app)       WANTED="app" ;;
    extension) WANTED="extension" ;;
    all)       WANTED="app extension" ;;
    *) echo "doctor.sh: unknown tier '$FOR' (want app | extension | all)" >&2; exit 2 ;;
esac

goal() {
    case "$1" in
        app)       echo "build and run the Linux host (synth core + loopback endpoint, no tray/panel/Kindle)" ;;
        extension) echo "work on the browser extension, which shares nothing with the rest" ;;
        all)       echo "the Linux host and the browser extension" ;;
    esac
}

failures=0
warnings=0

# Colour is presentation only - every status is still readable as plain text, because this
# output gets piped, redirected and pasted into issues. Honour NO_COLOR (the de facto
# convention) and its project-local KKR_NO_COLOR sibling, and fall back to plain when stdout
# is not a terminal.
use_color=1
[ -n "${NO_COLOR:-}" ] && use_color=0
[ -n "${KKR_NO_COLOR:-}" ] && use_color=0
[ -t 1 ] || use_color=0

C_RESET=$'\033[0m'
C_GREEN=$'\033[32m'
C_RED=$'\033[31m'
C_YELLOW=$'\033[33m'
C_GRAY=$'\033[90m'
C_CYAN=$'\033[36m'

declare -A STATUS_COLOR=( [ok]="$C_GREEN" [FAIL]="$C_RED" [warn]="$C_YELLOW" [skip]="$C_GRAY" )
declare -A MARKER=( [ok]="[+]" [FAIL]="[x]" [warn]="[!]" [skip]="[-]" )
declare -A BULLET=( [ok]="+" [FAIL]="x" [warn]="!" [skip]="-" )

# paint COLOR TEXT... - write TEXT in COLOR (no trailing newline), or plain when colour is off.
paint() {
    local color="$1"; shift
    if [ "$use_color" = 1 ] && [ -n "$color" ]; then
        printf '%s%s%s' "$color" "$*" "$C_RESET"
    else
        printf '%s' "$*"
    fi
}

wanted() {
    case " $WANTED " in *" $1 "*) return 0 ;; *) return 1 ;; esac
}

# One category. Args after DETAIL are the optional problem block: the first is the headline
# printed beside the bullet, the rest are explanation/fix lines indented under it.
report() {
    local tier="$1" status="$2" name="$3" purpose="$4" detail="$5"
    shift 5
    wanted "$tier" || return 0
    paint "${STATUS_COLOR[$status]}" "${MARKER[$status]} "
    printf '%s' "$name"
    [ -n "$purpose" ] && paint "$C_GRAY" " - $purpose"
    [ -n "$detail" ] && paint "$C_GRAY" " ($detail)"
    printf '\n'
    if [ "$#" -gt 0 ]; then
        paint "${STATUS_COLOR[$status]}" "    ${BULLET[$status]} "
        printf '%s\n' "$1"
        shift
        local line
        for line in "$@"; do
            paint "$C_GRAY" "      $line"
            printf '\n'
        done
    fi
    [ "$status" = FAIL ] && failures=$((failures + 1))
    [ "$status" = warn ] && warnings=$((warnings + 1))
    return 0
}

# Resolve by RUNNING the candidate. Prints the first line of `exe args...`, or nothing and a
# non-zero return when the tool is absent or fails. python3 under a distro's alternatives, a
# rustup shim, etc. are all just programs on PATH here - none of the Windows App Execution
# Alias trickery - so `command -v` plus actually running it is the whole story.
tool_version() {
    local exe="$1"; shift
    command -v "$exe" >/dev/null 2>&1 || return 1
    local out
    out=$("$exe" "$@" 2>/dev/null) || return 1
    printf '%s' "$out" | head -n1
}

# The version banners are mostly build metadata; a category line wants the number.
short_version() {
    local v
    v=$(printf '%s' "$1" | grep -Eo '[0-9]+\.[0-9]+(\.[0-9]+)?' | head -n1)
    [ -n "$v" ] && printf '%s' "$v" || printf '%s' "$1"
}

join_comma() {
    local out="" x
    for x in "$@"; do out="${out:+$out, }$x"; done
    printf '%s' "$out"
}

# The exact install command for this distribution. The .ps1 hands out `choco install ...`;
# the Linux equivalent has to name the package manager AND translate a few package names that
# differ between families (the build toolchain especially). Rust and bun are NOT distro
# packages - they keep their upstream installers below, same as the .ps1.
pm_install() {
    local pm names=() t
    if command -v apt-get >/dev/null 2>&1; then pm=apt
    elif command -v dnf >/dev/null 2>&1; then pm=dnf
    elif command -v pacman >/dev/null 2>&1; then pm=pacman
    elif command -v zypper >/dev/null 2>&1; then pm=zypper
    else pm=""; fi
    for t in "$@"; do
        case "$pm:$t" in
            apt:buildtools)               names+=("build-essential") ;;
            dnf:buildtools|zypper:buildtools) names+=("gcc" "make") ;;
            pacman:buildtools)            names+=("base-devel") ;;
            *:buildtools)                 names+=("a C toolchain (gcc/clang) and make") ;;
            pacman:python3)               names+=("python") ;;
            *:python3)                    names+=("python3") ;;
            *)                            names+=("$t") ;;
        esac
    done
    case "$pm" in
        apt)    printf 'sudo apt install -y %s' "${names[*]}" ;;
        dnf)    printf 'sudo dnf install -y %s' "${names[*]}" ;;
        pacman) printf 'sudo pacman -S --needed %s' "${names[*]}" ;;
        zypper) printf 'sudo zypper install -y %s' "${names[*]}" ;;
        *)      printf 'install %s with your distribution package manager' "${names[*]}" ;;
    esac
}

printf '\n'
paint "$C_CYAN" "$(printf "Doctor summary for '%s' - %s." "$FOR" "$(goal "$FOR")")"; printf '\n'
paint "$C_GRAY" "Tools only, not this checkout's provisioned state - that is fetch-deps.py and build.rs."; printf '\n'
if [ "$FOR" = all ]; then
    paint "$C_GRAY" "Narrow it with: doctor.sh app | extension."; printf '\n'
fi
printf '\n'

# --- Python ---------------------------------------------------------------------------------
# ONE category. python3 is the canonical name on Linux; `python` is a fallback (it may be
# absent, or on an old box point at Python 2). First match that reports Python 3 wins, and the
# detail names which one answered - everything under native-deps/ and packaging/ is invoked as
# "python3 <script>.py", so a working `python` that is not `python3` is still worth naming.
py_cmd=""
py_ver=""
for cand in python3 python; do
    v=$(tool_version "$cand" --version) || continue
    case "$v" in
        *"Python 3"*) py_cmd="$cand"; py_ver="$v"; break ;;
    esac
done
if [ -n "$py_cmd" ]; then
    report app ok "Python" "every build and provisioning script" \
        "$(short_version "$py_ver"), via $py_cmd"
else
    report app FAIL "Python" "every build and provisioning script" "" \
        "No python3 or python on PATH reports Python 3." \
        "native-deps/fetch-deps.py, fetch-model.py and fetch-ocr-models.py are invoked as" \
        "\"python3 <script>.py\"; nothing in this tree resolves an interpreter for you." \
        "Install it: $(pm_install python3)"
fi

# --- Rust -----------------------------------------------------------------------------------
# No x86 target check here: the SAPI shim, hook and injector are Windows-only, and the default
# x86_64-unknown-linux-gnu that rustup installs is all the Linux host needs.
cargo=$(tool_version cargo --version)
rustc=$(tool_version rustc --version)
if [ -n "$cargo" ] && [ -n "$rustc" ]; then
    report app ok "Rust" "compiles the Linux host (cargo + rustc)" \
        "cargo $(short_version "$cargo"), rustc $(short_version "$rustc")"
else
    missing=()
    [ -z "$cargo" ] && missing+=("cargo")
    [ -z "$rustc" ] && missing+=("rustc")
    report app FAIL "Rust" "compiles the Linux host (cargo + rustc)" "" \
        "Not found: $(join_comma "${missing[@]}")." \
        "Install Rust from https://rustup.rs - rustup brings both, and its default" \
        "x86_64-unknown-linux-gnu target is the only one the Linux host builds for."
fi

# --- Git, and the LFS filter the icons need -------------------------------------------------
git=$(tool_version git --version)
lfs=$(tool_version git lfs version)
if [ -n "$git" ] && [ -n "$lfs" ]; then
    report app ok "Git" "the checkout, and the LFS filter icons/ needs" \
        "git $(short_version "$git"), git-lfs $(short_version "$lfs")"
elif [ -z "$git" ]; then
    report app FAIL "Git" "the checkout, and the LFS filter icons/ needs" "" \
        "git not found." \
        "Install it: $(pm_install git)"
else
    report app FAIL "Git" "the checkout, and the LFS filter icons/ needs" "git $(short_version "$git")" \
        "git-lfs not found." \
        "icons/* live in LFS, so without the filter they check out as small pointer stubs" \
        "and the browser extension's toolbar icon builds broken." \
        "Install it, then enable it: $(pm_install git-lfs) && git lfs install && git lfs pull"
fi

# --- C toolchain (Linux only) ---------------------------------------------------------------
# espeak-ng is built FROM SOURCE by native-deps/fetch-deps.py -> build-espeak.py, so a C
# compiler + make are a real build prerequisite here in a way they are not on Windows (where
# MSVC comes with the Rust install). cargo also drives the system C compiler as its linker, so
# a missing compiler fails the host build too, not just the espeak step.
cc_bin=""
for cand in cc gcc clang; do
    command -v "$cand" >/dev/null 2>&1 && { cc_bin="$cand"; break; }
done
have_make=0
command -v make >/dev/null 2>&1 && have_make=1
if [ -n "$cc_bin" ] && [ "$have_make" = 1 ]; then
    ccver=$(tool_version "$cc_bin" --version)
    report app ok "C toolchain" "builds espeak-ng from source, and links the Rust host" \
        "$cc_bin $(short_version "$ccver"), make"
else
    lack=()
    [ -z "$cc_bin" ] && lack+=("a C compiler (gcc/clang)")
    [ "$have_make" != 1 ] && lack+=("make")
    report app FAIL "C toolchain" "builds espeak-ng from source, and links the Rust host" "" \
        "Missing: $(join_comma "${lack[@]}")." \
        "native-deps/build-espeak.py builds the modified espeak-ng 1.52.0 from source - a" \
        "distribution's own libespeak-ng is unmodified and changes the phonemes, so it is not" \
        "a substitute - and cargo uses the C compiler as its linker driver." \
        "Install it: $(pm_install buildtools)"
fi

# --- CMake (Linux only) ---------------------------------------------------------------------
cmake=$(tool_version cmake --version)
if [ -n "$cmake" ]; then
    report app ok "CMake" "configures and drives the espeak-ng build" "$(short_version "$cmake")"
else
    report app FAIL "CMake" "configures and drives the espeak-ng build" "" \
        "cmake not found." \
        "native-deps/fetch-deps.py runs build-espeak.py, which configures and builds" \
        "espeak-ng with cmake." \
        "Install it: $(pm_install cmake)"
fi

# --- Browser extension ----------------------------------------------------------------------
bun=$(tool_version bun --version)
if [ -n "$bun" ]; then
    report extension ok "bun" "builds the browser extension and runs its tests" \
        "$(short_version "$bun")"
else
    report extension FAIL "bun" "builds the browser extension and runs its tests" "" \
        "bun not found." \
        "It builds kokoro-browser-extension/ and runs its 145 tests; no CI workflow runs" \
        "those, so this machine is the only place they run at all." \
        "Install it: curl -fsSL https://bun.sh/install | bash"
fi

# NOTE: this reports TOOLS, not the state of this checkout. Whether the native dependencies
# are provisioned and current (the ORT and espeak markers under native-deps/linux/), whether
# the runtime libraries are staged, and whether icons/ are real images rather than unresolved
# LFS pointers are all things the build itself checks - fetch-deps.py and build.rs. So a green
# report here means the tools are installed, not that a build will get past provisioning.

# A clean run says nothing: every line is already a green [+], so a closing "no issues found"
# only adds a line to read on the runs that need the least reading. The footer exists to say
# how much is wrong, so it appears only when something is.
printf '\n'
issues=$((failures + warnings))
[ "$issues" -eq 0 ] && exit 0

word="categories"
[ "$issues" -eq 1 ] && word="category"
color="$C_YELLOW"
[ "$failures" -gt 0 ] && color="$C_RED"
paint "$color" "$(printf '! Doctor found issues in %d %s.' "$issues" "$word")"; printf '\n'
if [ "$failures" -gt 0 ]; then exit 1; fi
paint "$C_GREEN" "$(printf "  Nothing blocking - ready for '%s'." "$FOR")"; printf '\n'
exit 0
