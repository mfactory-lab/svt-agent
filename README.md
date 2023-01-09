# SVT Agent
SVT Agent is an app for secure communication with the server 
through the solana blockchain.

## Getting Started
You must to know your channel id (`CID`) before installing the agent.

Run this command

`shell
CLUSTER=devnet CID=... sh -c "$(curl -sSfL https://mfactory-lab.github.io/svt-agent/install.sh)"
`

# Configuration

Env
```shell
AGENT_KEYPAIR = "/app/keypair.json"
AGENT_CLUSTER = "devnet"
AGENT_CHANNEL_ID = "..."
AGENT_MESSENGER_PROGRAM = "4AnSBTc21f4wTBHmnFyarbosr28Qk4CgGFBHcRh4kYPw"
AGENT_MONITOR_PORT = "..."
AGENT_NOTIFY_WEBHOOK_URL = "https://..."
AGENT_NOTIFY_INFLUX_URL = "https://..."
AGENT_NOTIFY_INFLUX_DB = "svt-agent"
AGENT_NOTIFY_INFLUX_USER = ""
AGENT_NOTIFY_INFLUX_PASSWORD = ""
```

# Features
- Secure validator management
- Encrypted communication
- Task history

# License
GNU AGPL v3
