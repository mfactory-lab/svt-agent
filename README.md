# SVT Agent

```shell
docker run -it --rm --name svt-agent \
  -v /var/run/docker.sock:/var/run/docker.sock \
  -v $(pwd)/keypair.json:/app/keypair.json \
  svt-agent -k /app/keypair.json
```

## Installation (install.sh)
ubuntu20 (for now)
- install docker
- docker pull `svt-agent` image
- add systemd with `docker run`
- generate new `Keypair`
- add authorized_keys for ansible

## Roadmap
- Keypair (generate new or use validator vote_id or identity json file)

## Command Runner
...

## Log monitor
Immediately after running the command, the agent starts a separate service on a specific port (the port must be open)
which displays ansible logs in realtime
- Access by password or session token
- Example service
  - https://dozzle.dev/ (Realtime log viewer for docker containers.)

## Commands
- ansible playbooks (whitelist folder)
  - update validator
  - restart validator
  - reboot node
  - stop validator, clear ledger, start
  - reboot cluster
  - download new version
  - update monitoring
  - ...
- update ansible playbooks

## config.yml
- playbooks_path
- log-watcher_service_port
- webhook_urls (command_start, command_end)
- ansible_container_ttl
- ...

## Command State (Influx|Timescale public)
- Started(Command)
- Success(Command)
- Failed(Command, Error)

## Flow
- *Client* sees a list of allowed commands
- Client send some Encrypted(Command(...args)) to the on-chain channel
- Agent take, decrypt and parse the message
- Agent write command state [Command::Started] to the local state, influx, webhook
- Agent run [ansible] service if command is valid playbook
- Agent run [log-watcher] service with realtime log streaming
- Agent write command state [Command::Success] or [Command::Failed]
- Agent stopped [log-watcher] and [ansible]

## Questions
- Local state (sqlite) ?

## Misc

What port use for log-monitor 3123?
How to Run a Dockerized Service via systemd
https://jugmac00.github.io/blog/how-to-run-a-dockerized-service-via-systemd/
ssh tunnel
