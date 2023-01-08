/// Messenger program
pub const MESSENGER_PROGRAM_ID: &str = "4AnSBTc21f4wTBHmnFyarbosr28Qk4CgGFBHcRh4kYPw";

pub const DEFAULT_CHANNEL_ID: &str = "Bk1EAvKminEwHjyVhG2QA7sQeU1W3zPnFE6rTnLWDKYJ";

pub const WAIT_COMMAND_INTERVAL: u64 = 5000;
pub const WAIT_BALANCE_INTERVAL: u64 = 5000;
pub const WAIT_ACTION_INTERVAL: u64 = 5000;
pub const WAIT_AUTHORIZATION_INTERVAL: u64 = 10000;

/// Required balance before running commands
pub const MIN_BALANCE_REQUIRED: u64 = 5000; // one tx cost

/// @link: https://hub.docker.com/r/willhallonline/ansible
pub const ANSIBLE_IMAGE: &str = "willhallonline/ansible:alpine";

pub const CONTAINER_NAME: &str = "svt-agent";

/// Display name in the channel
pub const AGENT_NAME: &str = "agent";
