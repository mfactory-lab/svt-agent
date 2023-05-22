/// Messenger program
pub const MESSENGER_PROGRAM_ID: &str = "CgRaMXqqRHNT3Zo2uVZfX72TuxUgcLb8E3A8KrXnbXAC";

pub const DEFAULT_CLUSTER: &str = "devnet";

pub const DEFAULT_CHANNEL_ID: &str = "Bk1EAvKminEwHjyVhG2QA7sQeU1W3zPnFE6rTnLWDKYJ";

pub const WAIT_COMMAND_INTERVAL: u64 = 5000;
pub const WAIT_BALANCE_INTERVAL: u64 = 5000;
pub const WAIT_ACTION_INTERVAL: u64 = 5000;
pub const WAIT_AUTHORIZATION_INTERVAL: u64 = 10000;

/// Required balance before running commands
pub const MIN_BALANCE_REQUIRED: u64 = 5000; // lamports

/// @link: https://hub.docker.com/r/willhallonline/ansible
// pub const ANSIBLE_IMAGE: &str = "willhallonline/ansible:2.13.7-alpine-3.15";
pub const ANSIBLE_IMAGE: &str = "ghcr.io/mfactory-lab/sv-manager";
pub const ANSIBLE_DEFAULT_TAG: &str = "agent-latest";

pub const CONTAINER_NAME: &str = "svt-agent";

/// Display name in the channel
pub const AGENT_NAME: &str = "agent";

/// The task monitor will be launched on this port
pub const DEFAULT_MONITOR_PORT: &str = "8888";

/// Configuration files to be added to each task as `--extra-vars=@file`
pub const TASK_CONFIG_FILES: &[&str] = &[
    "{home}/inventory/group_vars/all.yml",
    "{home}/inventory/group_vars/{cluster}_validators.yaml",
    "/etc/sv_manager/sv_manager.conf",
];

pub const TASK_VERSION_ARG: &str = "pb-version";
pub const TASK_WORKING_DIR: &str = "/ansible";

// Notifier settings

pub const NOTIFY_INFLUX_URL: &str = "http://influx.thevalidators.io:8086";
pub const NOTIFY_INFLUX_DB: &str = "svt_agent";
pub const NOTIFY_INFLUX_USER: &str = "v_user";
pub const NOTIFY_INFLUX_PASS: &str = "thepassword";
