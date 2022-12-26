use crate::constants::COMMAND_DELIMITER;
use crate::encryption::decrypt_message;
use crate::runner::Task;
use crate::state::Message;
use anyhow::Error;
use anyhow::Result;

pub fn convert_message_to_task(msg: Message, cek: &[u8]) -> Result<Task> {
    let cmd = String::from_utf8(decrypt_message(msg.content, cek)?)?;
    let parts = cmd.split(COMMAND_DELIMITER).collect::<Vec<_>>();

    if parts.len() < 3 {
        return Err(Error::msg("Invalid command"));
    }

    let playbook = String::from(parts[0]);
    // TODO: validate
    let extra_vars = String::from(parts[1]);
    let uuid = String::from(parts[2]);

    Ok(Task {
        id: msg.id,
        uuid,
        playbook,
        extra_vars,
    })
}
