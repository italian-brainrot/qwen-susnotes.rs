#!/usr/bin/env bash
#
# scripts/dev-env-check.sh - say whether this machine can build, test and run susnotes.
#
# The question it answers is "what do I have to install?", and it answers it in every
# mode: a missing thing comes with the command that would fix it. Most of what is
# checked is outside the Rust world (apt packages, a display server, GPU drivers),
# which is why this is a shell script rather than a cargo subcommand.
#
# Exit codes: 0 when everything required is present, 1 when something required is
# missing, 2 when the flags were wrong. Optional things never change the exit status;
# they are reported as `optional` so a session can decide whether it wants them.
#
# Run it with no arguments. `just env` is the usual way. The CI `tooling` job runs
# `--tools --quiet` so the command list here stays honest about what a development
# session uses without needing the desktop app's libraries.

set -euo pipefail

scope=all
quiet=0
print_installs=0

usage() {
    cat <<'EOF'
usage: scripts/dev-env-check.sh [OPTION]

Options:
  --tools    check only the commands and rustup components, skip the system libraries
  --libs     check only the system libraries, skip the commands
  --quiet    print only the problems and the summary
  --install  imply --quiet, then print the apt and rustup commands that fix the
             missing things; the exit status still reflects whether anything is missing
  -h, --help print this text
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --tools) scope=tools ;;
        --libs) scope=libs ;;
        --quiet) quiet=1 ;;
        --install)
            print_installs=1
            quiet=1
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            printf 'dev-env-check: unknown option: %s\n\n' "$1" >&2
            usage >&2
            exit 2
            ;;
    esac
    shift
done

if [[ -t 1 && "${TERM:-dumb}" != "dumb" ]] && command -v tput >/dev/null 2>&1 && [[ "$(tput colors 2>/dev/null || echo 0)" -ge 2 ]]; then
    red=$(tput setaf 1)
    green=$(tput setaf 2)
    yellow=$(tput setaf 3)
    bold=$(tput bold)
    reset=$(tput sgr0)
else
    red="" green="" yellow="" bold="" reset=""
fi

declare -a apt_fixes=()
rustup_fixes=()
missing=0

heading() {
    [[ $quiet -eq 1 ]] && return 0
    printf '%s%s%s\n' "$bold" "$1" "$reset"
}

# report <ok|info|optional|missing> <text...>
report() {
    local what=$1 label colour
    shift
    case "$what" in
        ok)
            label="ok"
            colour=$green
            ;;
        info)
            label="info"
            colour=""
            ;;
        optional)
            label="optional"
            colour=$yellow
            ;;
        missing)
            label="MISSING"
            colour=$red
            ;;
        *) return 1 ;;
    esac
    # ok and info lines are the noise this script exists to produce, so --quiet skips
    # them and leaves only the problems, the optional notes and the summary.
    if [[ $quiet -eq 1 && ("$what" == "ok" || "$what" == "info") ]]; then
        return 0
    fi
    printf '  %s%-8s%s %s\n' "$colour" "$label" "$reset" "$*"
    if [[ $what == "missing" ]]; then
        missing=1
    fi
}

have() { command -v "$1" >/dev/null 2>&1; }

# _check <names> <apt package or "-"> <what it is for>
#
# <names> is a space separated list of command names tried in order, because a tool can
# be installed under another name (Ubuntu's fd is fdfind). <apt> is the package that
# provides it, or "-" when it does not come from apt and the message carries the hint.
# The severity of a failure comes from $kind, which the calling function sets.
_check() {
    local names=$1 apt=$2 purpose=$3
    local found="" name
    for name in $names; do
        if have "$name"; then
            found=$name
            break
        fi
    done
    if [[ -n $found ]]; then
        report ok "$found - $purpose"
    elif [[ $apt == "-" ]]; then
        report "$kind" "$names - $purpose (not an apt package on this distribution, install it from its own source)"
    else
        report "$kind" "$names - $purpose; sudo apt-get install -y $apt"
        if [[ $kind == "missing" ]]; then
            apt_fixes+=("$apt")
        fi
    fi
}

# required: changes the exit status when it is not there
check_cmd() {
    local kind=missing
    _check "$@"
}

# optional: a nice-to-have for a development session, never fatal
check_opt() {
    local kind=optional
    _check "$@"
}

rustup_has_component() {
    rustup component list --installed 2>/dev/null | grep -q "^$1-"
}

cargo_has_subcommand() {
    cargo --list 2>/dev/null | grep -qE "^[[:space:]]+$1([[:space:]]|$)"
}

# ---------------------------------------------------------------- commands
if [[ $scope != "libs" ]]; then
    heading "commands"
    check_cmd "git" "-" "the repository (comes with the system)"
    check_cmd "gh" "-" "GitHub CLI: branches, pull requests, review comments (https://github.com/cli/cli#installation)"
    check_cmd "cargo" "-" "the build tool (comes with rustup, see https://rustup.rs)"
    check_cmd "rustup" "-" "the rust toolchain manager (https://rustup.rs)"
    check_cmd "pkg-config pkgconf" "pkg-config" "build scripts find system libraries through it"
    check_cmd "cmake" "cmake" "used by some C build scripts"
    check_cmd "cc gcc" "build-essential" "a C compiler (ring and freetype build C)"
    check_cmd "make" "make" "used by some build scripts"
    check_cmd "shellcheck" "shellcheck" "lints the scripts in scripts/ (just lint)"
    check_cmd "just" "just" "the task runner this repository uses (the justfile)"
    check_opt "fd fdfind" "fd-find" "the file finder a shell session expects (Ubuntu names it fdfind)"
    check_opt "rg" "ripgrep" "grep, the way a shell session expects it"
fi

# ---------------------------------------------------------- rust toolchain
if [[ $scope != "libs" ]]; then
    heading "rust toolchain"
    if have rustc; then
        rustc_v=$(rustc --version 2>/dev/null | awk '{print $2}')
        if [[ $rustc_v == *[!0-9.]* ]]; then
            report ok "rustc $rustc_v (a non-release channel, version not compared)"
        elif [[ $(printf '1.85\n%s\n' "$rustc_v" | sort -V | head -1) == "1.85" ]]; then
            report ok "rustc $rustc_v (the workspace needs 1.85 or newer)"
        else
            report missing "rustc $rustc_v is too old: edition 2024 and 1.85 features are used; rustup update stable"
        fi
    else
        report missing "no rustc: rustup is not installed, or ~/.cargo/bin is not in PATH (https://rustup.rs)"
    fi

    if have rustup; then
        for component in clippy rustfmt; do
            if rustup_has_component "$component"; then
                report ok "$component component"
            else
                report missing "$component component: rustup component add $component"
                rustup_fixes+=("$component")
            fi
        done
        if rustup_has_component "rust-analyzer"; then
            report ok "rust-analyzer component (editor integration)"
        else
            report optional "no rust-analyzer component (editor integration): rustup component add rust-analyzer"
        fi
    fi

    if have cargo; then
        if cargo_has_subcommand "tree"; then
            report ok "cargo tree (built into cargo)"
        else
            report optional "no cargo tree (it ships with cargo; nothing to install)"
        fi
        if cargo_has_subcommand "mutants"; then
            report ok "cargo mutants (mutation testing, just mutation)"
        else
            report optional "no cargo mutants (mutation testing): sudo apt-get install -y cargo-mutants"
        fi
    fi
fi

# -------------------------------------------------------- system libraries
if [[ $scope == tools ]]; then
    heading "system libraries"
    report info "skipped (--tools)"
else
    heading "system libraries (the desktop app links against these)"
    if ! have pkg-config; then
        report missing "pkg-config is needed to find the system libraries"
    else
        # The same list the README's install block and the CI workflow use: they belong
        # together, and this list is what notices when one of them goes missing.
        pc_check=(
            "x11|libx11-dev"
            "x11-xcb|libx11-xcb-dev"
            "xcb|libxcb1-dev"
            "xkbcommon|libxkbcommon-dev"
            "xkbcommon-x11|libxkbcommon-x11-dev"
            "freetype2|libfreetype-dev"
            "fontconfig|libfontconfig1-dev"
            "openssl|libssl-dev"
            "zlib|zlib1g-dev"
        )
        for entry in "${pc_check[@]}"; do
            pc=${entry%%"|"*}
            apt=${entry##*"|"}
            if mod=$(pkg-config --modversion "$pc" 2>/dev/null); then
                report ok "$pc $mod"
            else
                report missing "$pc: sudo apt-get install -y $apt"
                apt_fixes+=("$apt")
            fi
        done
    fi
fi

if [[ $print_installs -eq 1 ]]; then
    if [[ ${#apt_fixes[@]} -gt 0 ]]; then
        printf 'sudo apt-get update && sudo apt-get install -y'
        printf ' %s' "${apt_fixes[@]}"
        printf '\n'
    fi
    if [[ ${#rustup_fixes[@]} -gt 0 ]]; then
        printf 'rustup component add'
        printf ' %s' "${rustup_fixes[@]}"
        printf '\n'
    fi
    if [[ $missing -eq 0 ]]; then
        printf 'nothing is missing: the environment is complete\n'
    else
        printf 'the MISSING lines above say what is wrong, the lines above say what to install\n'
    fi
    exit "$missing"
fi

# --------------------------------------- runtime, never part of the exit status
heading "runtime (informational, only the running app needs these)"
gpu_nodes=(/dev/dri/* /dev/nvidia*)
if [[ -n "${WAYLAND_DISPLAY:-}" ]]; then
    report info "wayland socket visible as $WAYLAND_DISPLAY"
elif [[ -n "${DISPLAY:-}" ]]; then
    report info "x11 display $DISPLAY (wayland is the preferred path)"
else
    report info "no display server in this session: the desktop app needs X11 or Wayland, try xvfb-run for a headless run"
fi
if [[ -e ${gpu_nodes[0]} || -e ${gpu_nodes[1]} ]]; then
    report info "GPU device nodes present"
else
    report info "no GPU device nodes: GPUI renders through a driver, or mesa-vulkan-drivers for software"
fi
icd_files=(/usr/share/vulkan/icd.d/*.json /etc/vulkan/icd.d/*.json)
if [[ -e ${icd_files[0]} || -e ${icd_files[1]} ]]; then
    report info "a Vulkan ICD is installed (GPUI renders through it)"
else
    report info "no Vulkan ICD: GPUI needs a driver, or mesa-vulkan-drivers for software rendering"
fi
if have fc-list; then
    report info "fontconfig sees $(fc-list 2>/dev/null | wc -l | tr -d ' ') fonts (GPUI renders text through fontconfig)"
else
    report info "no fc-list: text rendering comes from fontconfig and freetype"
fi

if [[ $missing -eq 0 ]]; then
    report ok "the environment is ready for this workspace"
else
    printf '\n%srequired tools are missing%s: each MISSING line above says what to install\n' "$bold" "$reset"
fi
exit "$missing"
