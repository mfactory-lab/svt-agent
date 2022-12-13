use crate::constants::COMMAND_DELIMITER;
use crate::encryption::decrypt_message;
use crate::runner::Task;
use anchor_lang::prelude::borsh::BorshSerialize;
use anchor_lang::prelude::*;
use anyhow::{Error, Result};

#[derive(AnchorSerialize, AnchorDeserialize, Debug)]
pub struct Channel {
    /// Workspace used to group channels
    pub workspace: String,
    /// Name of the channel
    pub name: String,
    /// Channel creator
    pub creator: Pubkey,
    /// Creation date
    pub created_at: i64,
    /// Last message date
    pub last_message_at: i64,
    /// Channel flags
    pub flags: u8,
    /// Number of members
    pub member_count: u16,
    /// Message counter
    pub message_count: u64,
    /// The maximum number of messages that are stored in [messages]
    pub max_messages: u16,
    /// List of latest messages
    pub messages: Vec<Message>,
}

#[derive(AnchorSerialize, AnchorDeserialize, Debug)]
pub struct ChannelMembership {
    /// Associated [Channel] address
    pub channel: Pubkey,
    /// Authority of membership
    pub authority: Pubkey,
    /// Status of membership
    pub status: ChannelMembershipStatus,
    /// Status target key
    pub status_target: Option<Pubkey>,
    /// Name of the channel member
    pub name: String,
    /// The last read message id
    pub last_read_message_id: u64,
    /// Creation date
    pub created_at: i64,
    /// Membership flags
    pub flags: u8,
    /// Bump seed for deriving PDA seeds
    pub bump: u8,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, PartialEq, Debug)]
pub enum ChannelMembershipStatus {
    Authorized,
    Pending,
}

#[derive(AnchorSerialize, AnchorDeserialize, Debug)]
pub struct ChannelDevice {
    /// Associated [Channel] address
    pub channel: Pubkey,
    /// Authority of the [ChannelKey]
    pub authority: Pubkey,
    /// The device key used to encrypt the `cek`
    pub key: Pubkey,
    /// The content encryption key (CEK)
    pub cek: CEKData,
    /// Bump seed for deriving PDA seeds
    pub bump: u8,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct CEKData {
    /// The header information for the CEK
    pub header: String,
    /// The CEK itself
    pub encrypted_key: String,
}

#[derive(AnchorSerialize, AnchorDeserialize, Debug, Clone)]
pub struct Message {
    /// Uniq message id
    pub id: u64,
    /// The message sender id
    pub sender: Pubkey,
    /// The unix timestamp at which the message was received
    pub created_at: i64,
    /// Message flags
    pub flags: u8,
    /// The (typically encrypted) message content
    pub content: String,
}

impl Message {
    pub fn convert_to_task(&self, cek: &[u8]) -> Result<Task> {
        let cmd = String::from_utf8(decrypt_message(&self.content, cek)?)?;
        let parts = cmd.split(COMMAND_DELIMITER).collect::<Vec<_>>();

        if parts.len() < 3 {
            return Err(Error::msg("Invalid command"));
        }

        let playbook = String::from(parts[0]);
        // TODO: validate
        let extra_vars = String::from(parts[1]);
        let uuid = String::from(parts[2]);

        Ok(Task {
            id: self.id,
            uuid,
            playbook,
            extra_vars,
        })
    }
}

#[event]
#[derive(Debug, Clone)]
pub struct NewMessageEvent {
    pub channel: Pubkey,
    pub message: Message,
}
