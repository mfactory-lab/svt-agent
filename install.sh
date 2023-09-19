#!/bin/bash
#set -e

#
# SVT Agent installation script
#
# Stable version:
#   CID=Bk1EAvKminEwHjyVhG2QA7sQeU1W3zPnFE6rTnLWDKYJ \
#   bash -c "$(curl -sSfL https://mfactory-lab.github.io/svt-agent/install.sh)"
#
# Nightly version:
#   CID=Bk1EAvKminEwHjyVhG2QA7sQeU1W3zPnFE6rTnLWDKYJ AGENT_RELEASE=nightly bash -c "$(curl -sSfL https://mfactory-lab.github.io/svt-agent/install-dev.sh)"
#

IMAGE_NAME=ghcr.io/mfactory-lab/svt-agent
AGENT_RELEASE="${AGENT_RELEASE:-latest}"
CLUSTER="${CLUSTER:-devnet}"
CONTAINER_NAME="${CONTAINER_NAME:-svt-agent}"
LOG_LEVEL="${LOG_LEVEL:-info}"
SSHKEY_PATH="$HOME/.ssh/svt-agent"
WORKING_DIR="$HOME/svt-agent"
KEYPAIR_PATH="$WORKING_DIR/keypair.json"

# ARE YOU ROOT (or sudo)?
if [[ $EUID -ne 0 ]]; then
	echo -e "ERROR: This script must be run as root"
	exit 1
fi

do_install() {
  echo "Installing SVT Agent..."

  ensure is_valid_os
  # ensure is_valid_cluster $CLUSTER
  # TODO: validate channel_id
  # ensure is_valid_channel $CID
  ensure generate_sshkey "id_rsa"

  if [ -z "$CID" ]; then
    err "Please provide some channel id \"CID=...\""
  fi

  if ! check_cmd "docker"; then
    echo "Installing docker..."
    wget -qO- https://get.docker.com | sh
    sudo systemctl start docker && sudo systemctl enable docker
    echo "Done"
  fi

  mkdir -p $WORKING_DIR
  mkdir -p $WORKING_DIR/logs

  say "Downloading agent image (release: $AGENT_RELEASE)..."
  ensure docker pull $IMAGE_NAME:$AGENT_RELEASE

  say "Cleaning up..."
  docker stop $CONTAINER_NAME 2>/dev/null 1>/dev/null
  docker container rm -f -v $CONTAINER_NAME 2>/dev/null 1>/dev/null
  docker volume rm -f "$CONTAINER_NAME-ansible" 2>/dev/null 1>/dev/null

  say "Try to generate agent keypair..."
  if [[ -f $KEYPAIR_PATH ]]; then
    say "Agent keypair already exits ($KEYPAIR_PATH)"
  else
    KEYPAIR="$(docker run --rm -it $IMAGE_NAME:$AGENT_RELEASE generate-keypair)"
    echo $KEYPAIR >$KEYPAIR_PATH
    say "Added new agent keypair ($KEYPAIR_PATH)"
  fi

  if [[ ! -f $KEYPAIR_PATH ]]; then
    err "Something went wrong. Agent keypair file is not exists."
  fi

  IP_ADDR=$(hostname -I | cut -f1 -d' ')

  say "Starting docker container..."
  CONTAINER_ID="$(docker run -d -it --restart=always --name $CONTAINER_NAME \
    --hostname $CONTAINER_NAME \
    --log-opt max-size=10m \
    --log-opt max-file=5 \
    -v /var/run/docker.sock:/var/run/docker.sock \
    -v /etc/sv_manager:/etc/sv_manager \
    -v $SSHKEY_PATH:/root/.ssh \
    -v $KEYPAIR_PATH:/app/keypair.json \
    -v $WORKING_DIR/logs:/app/logs \
    -v "$CONTAINER_NAME-ansible":/app/ansible \
    -e DOCKER_HOST_IP="$IP_ADDR" \
    -e AGENT_CLUSTER="$CLUSTER" \
    -e AGENT_CHANNEL_ID="$CID" \
    -e RUST_LOG="$LOG_LEVEL" \
    $IMAGE_NAME:$AGENT_RELEASE \
    run)"

  say "Container ID: $CONTAINER_ID"

  PUBKEY="$(docker run --rm -i -v $KEYPAIR_PATH:/app/keypair.json $IMAGE_NAME:$AGENT_RELEASE show-pubkey)"

  say ""
  say "SVT-Agent successfully installed!"
  say "Please add some balance to the agent address."
  say ""
  say "Host IP: $IP_ADDR"
  say "Cluster: $CLUSTER"
  say "Channel: $CID"
  say "Agent Address: $PUBKEY"
  say "Agent Release: $AGENT_RELEASE"
  say "Agent Home: $WORKING_DIR"
  say ""
  say "Done"
}

generate_sshkey() {
  if [[ -f $SSHKEY_PATH/$@ ]]; then
    say "SSH Keyfile already exists - skipping"
  else
    mkdir -p $SSHKEY_PATH
    ssh-keygen -t rsa -b 4096 -f "$SSHKEY_PATH/$@" -q -N '' -C 'svt-agent'
    chmod 600 "$SSHKEY_PATH/$@"

    # prepare authorized_keys
    [[ ! -d ~/.ssh ]] && mkdir -p ~/.ssh && chmod 700 ~/.ssh
    [[ ! -f ~/.ssh/authorized_keys ]] && touch ~/.ssh/authorized_keys && chmod 600 ~/.ssh/authorized_keys

    # check trailing newline
    x=$(tail -c 1 ~/.ssh/authorized_keys)
    if [ "$x" != "" ]
      then echo >> ~/.ssh/authorized_keys
    fi
    cat "$SSHKEY_PATH/$@.pub" >> ~/.ssh/authorized_keys
    say "SSH Keyfile was generated"
  fi
}

# Retrieves the operating system information.
# This function checks various methods to determine the OS and its version,
# including checking the /etc/os-release file, using the lsb_release command,
# and fallback options such as /etc/lsb-release, /etc/debian_version, and uname command.
# The OS and version information are stored in the variables OS and VER, respectively.
# Returns the OS and version as a string in the format "OS, Version".
detect_os() {
  if [ -f /etc/os-release ]; then
    # freedesktop.org and systemd
    . /etc/os-release
    OS=$NAME
    VER=$VERSION_ID
  elif command -v lsb_release >/dev/null 2>&1; then
    # linuxbase.org
    OS=$(lsb_release -si)
    VER=$(lsb_release -sr)
  elif [ -f /etc/lsb-release ]; then
    # For some versions of Debian/Ubuntu without lsb_release command
    . /etc/lsb-release
    OS=$DISTRIB_ID
    VER=$DISTRIB_RELEASE
  elif [ -f /etc/debian_version ]; then
    # Older Debian/Ubuntu/etc.
    OS=Debian
    VER=$(cat /etc/debian_version)
  elif [ -f /etc/SuSe-release ]; then
    # Older SuSE/etc.
    ...
  elif [ -f /etc/redhat-release ]; then
    # Older Red Hat, CentOS, etc.
    ...
  else
    # Fall back to uname, e.g. "Linux <version>", also works for BSD, etc.
    OS=$(uname -s)
    VER=$(uname -r)
  fi
  echo "$OS" "$VER"
}

is_valid_cluster() {
  if [[ "$@" =~ ^(mainnet-beta|devnet|testnet)$ ]]; then
    say "Cluster: $@"
  else
    err "Invalid cluster \"$@\""
  fi
}

is_valid_os() {
  read -r OS VER < <(detect_os)
  if [[ "$OS" =~ ^(Ubuntu)$ ]]; then
    say "OS: $OS ($VER)"
  else
    err "Unsupported operating system \"$OS\". Currently only Ubuntu is supported."
  fi
}

check_cmd() {
  command -v "$1" >/dev/null 2>&1
}

say() {
  printf 'svt-agent: %s\n' "$1"
}

err() {
  say "$1" >&2
  exit 1
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

# wrapped up in a function so that we have some protection against only getting
# half the file during "curl | sh"
do_install
