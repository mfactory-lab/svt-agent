# SVT Agent

## Installation (install.sh)
For ubuntu20
- Install docker
- docker pull svt-agent (agent + ansible + playbooks) image from registry
- Supervisor: systemd service or docker compose ?
- add authorized_keys (generate)
- add new user `svt`, configure sudoers

## Roadmap
- Keypair (generate new or use validator vote_id or identity json file)
- Run commands where id > `last_read_message_id`
- Listen realtime events

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
