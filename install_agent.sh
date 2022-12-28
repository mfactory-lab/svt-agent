#!/bin/bash
set -e

#
# SVT Agent installation script
#
# Run command:
#   curl -fsSL https://svt.one/install-agent.sh && sudo sh install-agent.sh
#

VERSION="${VERSION:-latest}"
CLUSTER="${VERSION:-devnet}"
CONTAINER_NAME="${VERSION:-svt-agent}"
EXPOSE_PORT="${EXPOSE_PORT:-8888}"
CHANNEL_ID=

generate_sshkeys() {
  if [[ -f '~/.ssh/id_rsa' ]]; then
      echo "Keyfile already exists - skipping"
  else
      mkdir ~/.ssh
      ssh-keygen -t rsa -b 4096 -f ~/.ssh/id_rsa -q -N ''
      chmod 600 ~/.ssh/id_rsa
      cat ~/.ssh/id_rsa.pub >> ~/.ssh/authorized_keys
  fi
}

do_install() {

  generate_sshkeys();

  echo "Installing docker..."
  curl -fsSL https://get.docker.com -o get-docker.sh && sudo sh get-docker.sh
  sudo systemctl start docker && sudo systemctl enable docker
  echo "Done"

  #echo "Setup firewall..."
  #sudo ufw allow $EXPOSE_PORT/tcp
  #echo "Done"

  echo "Pull and run docker container..."

  docker pull ghcr.io/mfactory-lab/svt-agent:$VERSION
  docker stop $CONTAINER_NAME 2>/dev/null
  docker container rm $CONTAINER_NAME 2>/dev/null

  # Generate agent keypair
  if [[ -f '~/agent-keypair.json' ]]; then
    echo ""
  else
    docker run --rm -it mfactory-lab/svt-agent:$VERSION generate-keypair > ~/agent-keypair.json
  fi

  docker run -d -it --restart=always --name $CONTAINER_NAME \
    -v ~/.ssh/id_rsa:/root/.ssh/id_rsa \
    -v ~/agent-keypair.json:/app/keypair.json \
    -p $EXPOSE_PORT:8888 \
    mfactory-lab/svt-agent:$VERSION \
    --cluster $CLUSTER \
    --channel-id $CHANNEL_ID

  echo "Done"

  echo "\n"

  echo "Agent Pubkey: .."
  echo "Agent Cluster: $CLUSTER"
  echo "Agent Channel ID: $CHANNEL_ID"
}

# wrapped up in a function so that we have some protection against only getting
# half the file during "curl | sh"
do_install
