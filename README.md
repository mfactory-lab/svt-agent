# SVT Agent (Solana Validator Toolkit Agent)
**SVT Agent** is an app for secure communication between user and server
through the solana blockchain.

The user just runs the predefined commands by signing them with his wallet. 
The commands are stored in the blockchain in encrypted form. 
The agent instantly receives new commands and starts executing them. 
If a command fails, the user can decide what to do with it manually. 
The execution log of each command is available via a secure link, 
all logs are stored on the server.

## Getting Started
You should know your channel id (`CID`) before installing the agent.
By default cluster is `devent`, but you can change it with `CLUSTER=devnet` var.

Example command:
```shell
CLUSTER=devnet CID=9ZTubJxjAPfPUAoC8Y9PeHNHbn3hkcq7DkiQpZT2cucV \
bash -c "$(curl -sSfL https://mfactory-lab.github.io/svt-agent/install.sh)"
````

# Configuration

Available environment variables
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
- On-chain task history
- Influx notification support
- Webhook support
- Real-time log tracing

# License
GNU AGPL v3
