# SVT Agent
SVT Agent is an application for secure communication with the server 
through the blockchain

## Getting Started
You must know your `CHANNEL_ID` before installing the agent.

Run this command

`shell
CHANNEL_ID=... CLUSTER=devnet sh -c "$(curl -sSfL https://svt.one/install-agent.sh)"
`

# Configuration

Env
`
AGENT_KEYPAIR = "/app/keypair.json"
AGENT_CLUSTER = "devnet"
AGENT_CHANNEL_ID = "..."
AGENT_MESSENGER_PROGRAM = "..."
AGENT_MONITOR_PORT = "..."
AGENT_NOTIFY_INFLUX_URL = "https://..."
AGENT_NOTIFY_INFLUX_DB = "svt-agent"
AGENT_NOTIFY_INFLUX_USER = ""
AGENT_NOTIFY_INFLUX_PASSWORD = ""
`

# Features
- Secure validator management
- Encrypted communication
- Task history

# License
GNU AGPL v3
