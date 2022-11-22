#!/bin/sh
# shellcheck shell=dash

# This is just a little script that can be downloaded from the internet to
# install svt-agent. It just does platform detection, downloads the installer
# and runs it.

{ # this ensures the entire script is downloaded #

DOWNLOAD_ROOT="${DOWNLOAD_ROOT:-https://github.com/mfactory-lab/svt-agent/releases/download/}"
GH_LATEST_RELEASE="https://api.github.com/repos/mfactory-lab/svt-agent/releases/latest"
BIN_NANE=svt-agent-install
#AGENT_RELEASE=

set -e

usage() {
    cat 1>&2 <<EOF
$BIN_NANE
initializes a new installation

USAGE:
    $BIN_NANE [FLAGS] [OPTIONS] --data_dir <PATH> --pubkey <PUBKEY>

FLAGS:
    -h, --help              Prints help information
        --no-modify-path    Don't configure the PATH environment variable

OPTIONS:
    -d, --data_dir <PATH>    Directory to store install data
    -u, --url <URL>          JSON RPC URL for the solana cluster
    -p, --pubkey <PUBKEY>    Public key of the update manifest
EOF
}

main() {
    downloader --check
    need_cmd uname
    need_cmd mktemp
    need_cmd chmod
    need_cmd mkdir
    need_cmd rm
    need_cmd sed
    need_cmd grep

    for arg in "$@"; do
      case "$arg" in
        -h|--help)
          usage
          exit 0
          ;;
        *)
          ;;
      esac
    done

    _ostype="$(uname -s)"
    _cputype="$(uname -m)"

    case "$_ostype" in
    Linux)
      _ostype=unknown-linux-gnu
      ;;
    Darwin)
      if [[ $_cputype = arm64 ]]; then
        _cputype=aarch64
      fi
      _ostype=apple-darwin
      ;;
    *)
      err "machine architecture is currently unsupported"
      ;;
    esac
    TARGET="${_cputype}-${_ostype}"

    _ansi_escapes_are_valid=false
    if [ -t 2 ]; then
        if [ "${TERM+set}" = 'set' ]; then
            case "$TERM" in
                xterm*|rxvt*|urxvt*|linux*|vt*)
                    _ansi_escapes_are_valid=true
                ;;
            esac
        fi
    fi

    _dir="$(ensure mktemp -d)"

    # Check for $AGENT_RELEASE environment variable override.  Otherwise fetch
    # the latest release tag from github
    if [ -n "$AGENT_RELEASE" ]; then
      release="$AGENT_RELEASE"
    else
      release_file="$_dir/release"
      printf 'looking for latest release\n' 1>&2
      ensure downloader "$GH_LATEST_RELEASE" "$release_file"
      release=$(\
        grep -m 1 \"tag_name\": "$release_file" \
        | sed -ne 's/^ *"tag_name": "\([^"]*\)",$/\1/p' \
      )
      if [ -z "$release" ]; then
        err 'Unable to figure latest release'
      fi
    fi

    _url="$DOWNLOAD_ROOT/$release/$BIN_NANE-$TARGET"
    _file="$_dir/$BIN_NANE"

    if $_ansi_escapes_are_valid; then
        printf "\33[1minfo:\33[0m downloading installer\n" 1>&2
    else
        printf '%s\n' 'info: downloading installer' 1>&2
    fi

    ensure mkdir -p "$_dir"
    ensure downloader "$_url" "$_file" "$TARGET"
    ensure chmod u+x "$_file"

    if [ ! -x "$_file" ]; then
        printf '%s\n' "Cannot execute $_file (likely because of mounting /tmp as noexec)." 1>&2
        printf '%s\n' "Please copy the file to a location where you can execute binaries and run ./svt-agent-init." 1>&2
        exit 1
    fi

    ignore "$_file" "$@"
    _retval=$?

    ignore rm "$_file"
    ignore rmdir "$_dir"

    return "$_retval"
}

say() {
    printf 'svt-agent: %s\n' "$1"
}

err() {
    say "$1" >&2
    exit 1
}

need_cmd() {
    if ! check_cmd "$1"; then
        err "need '$1' (command not found)"
    fi
}

check_cmd() {
    command -v "$1" > /dev/null 2>&1
}

# Run a command that should never fail. If the command fails execution
# will immediately terminate with an error showing the failing
# command.
ensure() {
    if ! "$@"; then err "command failed: $*"; fi
}

# This is just for indicating that commands' results are being
# intentionally ignored. Usually, because it's being executed
# as part of error handling.
ignore() {
    "$@"
}

# This wraps curl or wget. Try curl first, if not installed,
# use wget instead.
downloader() {
    if check_cmd curl; then
        program=curl
    elif check_cmd wget; then
        program=wget
    else
        program='curl or wget' # to be used in error message of need_cmd
    fi

    if [ "$1" = --check ]; then
        need_cmd "$program"
    elif [ "$program" = curl ]; then
        curl -sSfL "$1" -o "$2"
    elif [ "$program" = wget ]; then
        wget "$1" -O "$2"
    else
        err "Unknown downloader"   # should not reach here
    fi
}

main "$@" || exit 1

} # this ensures the entire script is downloaded #
