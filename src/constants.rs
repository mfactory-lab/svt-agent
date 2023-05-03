/// Messenger program
pub const MESSENGER_PROGRAM_ID: &str = "CgRaMXqqRHNT3Zo2uVZfX72TuxUgcLb8E3A8KrXnbXAC";

pub const DEFAULT_CLUSTER: &str = "devnet";

pub const DEFAULT_CHANNEL_ID: &str = "Bk1EAvKminEwHjyVhG2QA7sQeU1W3zPnFE6rTnLWDKYJ";

pub const WAIT_COMMAND_INTERVAL: u64 = 5000;
pub const WAIT_BALANCE_INTERVAL: u64 = 5000;
pub const WAIT_ACTION_INTERVAL: u64 = 5000;
pub const WAIT_AUTHORIZATION_INTERVAL: u64 = 10000;

/// Required balance before running commands
pub const MIN_BALANCE_REQUIRED: u64 = 5000; // one tx cost

/// @link: https://hub.docker.com/r/willhallonline/ansible
pub const ANSIBLE_IMAGE: &str = "willhallonline/ansible:2.13.7-alpine-3.15";

pub const CONTAINER_NAME: &str = "svt-agent";

/// Display name in the channel
pub const AGENT_NAME: &str = "agent";

pub const DEFAULT_MONITOR_PORT: &str = "8888";

// Notifier settings

pub const NOTIFY_INFLUX_URL: &str = "http://influx.thevalidators.io:8086";
pub const NOTIFY_INFLUX_DB: &str = "svt_agent";
pub const NOTIFY_INFLUX_USER: &str = "v_user";
pub const NOTIFY_INFLUX_PASS: &str = "thepassword";
