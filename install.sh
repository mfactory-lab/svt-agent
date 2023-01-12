#!/bin/bash
#set -e

#
# SVT Agent installation script
#
# Run command:
#   CLUSTER=devnet CID=QXK7qRCaabreGjcNHcKEadQDtTY9BJKHKfU11QAE6Xp \
#   bash -c "$(curl -sSfL https://mfactory-lab.github.io/svt-agent/install.sh)"
#

IMAGE_NAME=ghcr.io/mfactory-lab/svt-agent
AGENT_RELEASE="${AGENT_RELEASE:-latest}"
CLUSTER="${CLUSTER:-devnet}"
CONTAINER_NAME="${CONTAINER_NAME:-svt-agent}"
SSHKEY_PATH="$HOME/.ssh/svt-agent"
WORKING_DIR="$HOME/svt-agent"
KEYPAIR_PATH="$WORKING_DIR/keypair.json"

# ARE YOU ROOT (or sudo)?
#if [[ $EUID -ne 0 ]]; then
#	echo -e "ERROR: This script must be run as root"
#	exit 1
#fi

do_install() {
  echo "Installing SVT Agent..."

  ensure is_valid_cluster $CLUSTER
  # TODO: validate channel_id
  # ensure is_valid_channel $CID
  ensure generate_sshkey "id_rsa"

  if [ -z "$CID" ]; then
    err "Please provide some channel id \"CID=...\""
  fi

  if ! check_cmd "docker"; then
    echo "Installing docker..."
    sh -c "$(curl -fsSL https://get.docker.com)"
    sudo systemctl start docker && sudo systemctl enable docker
    echo "Done"
  fi

  mkdir -p $WORKING_DIR
  mkdir -p $WORKING_DIR/logs

  #say "Setup firewall..."
  #sudo ufw allow $EXPOSE_PORT/tcp
  #say "Done"

  say "Downloading agent image (release: $AGENT_RELEASE)..."
  ensure docker pull $IMAGE_NAME:$AGENT_RELEASE

  say "Cleanup..."
  docker stop $CONTAINER_NAME 2>/dev/null 1>/dev/null
  docker container rm $CONTAINER_NAME 2>/dev/null 1>/dev/null

  say "Try to generate agent keypair..."
  if [[ -f $KEYPAIR_PATH ]]; then
    say "Agent keypair already exits ($KEYPAIR_PATH)"
  else
    KEYPAIR="$(docker run --rm -it $IMAGE_NAME:$AGENT_RELEASE generate-keypair)"
    echo $KEYPAIR > $KEYPAIR_PATH
    say "Added new agent keypair ($KEYPAIR_PATH)"
  fi

  if [[ ! -f $KEYPAIR_PATH ]]; then
    err "Something went wrong. Agent keypair file is not exists."
  fi

  say "Starting docker container..."
  CONTAINER_ID="$(docker run -d -it --restart=always --name $CONTAINER_NAME \
    --hostname $CONTAINER_NAME \
    -v /var/run/docker.sock:/var/run/docker.sock \
    -v svt-agent-ansible:/app/ansible \
    -v $SSHKEY_PATH:/root/.ssh \
    -v $KEYPAIR_PATH:/app/keypair.json \
    -v $WORKING_DIR/logs:/app/logs \
    $IMAGE_NAME:$AGENT_RELEASE \
    run \
    --cluster $CLUSTER \
    --channel-id $CID)"
  say "Container ID: $CONTAINER_ID"

  say "SVT-Agent successfully installed!"
  say "Please add some balance to the agent address."
  say ""
  say "Address(Pubkey): ..."
  say ""
  say "Done"
}

generate_sshkey() {
  if [[ -f $SSHKEY_PATH/$@ ]]; then
      say "SSH Keyfile already exists - skipping"
  else
      mkdir -p $SSHKEY_PATH
      ssh-keygen -t rsa -b 4096 -f $SSHKEY_PATH/$@ -q -N '' -C 'svt-agent'
      chmod 600 $SSHKEY_PATH/$@
      cat $SSHKEY_PATH/$@.pub >> ~/.ssh/authorized_keys
      say "SSH Keyfile was generated"
  fi
}

is_valid_cluster() {
  if [[ "$@" =~ ^(mainnet-beta|devnet|testnet)$ ]]; then
    say "Cluster: $@"
  else
    err "Invalid cluster \"$@\""
  fi
}

check_cmd() {
    command -v "$1" > /dev/null 2>&1
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
