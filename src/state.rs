use anchor_lang::prelude::*;

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

#[event]
#[derive(Debug)]
pub struct NewMessageEvent {
    pub channel: Pubkey,
    pub message: Message,
}

#[derive(AnchorSerialize, AnchorDeserialize, Debug)]
pub struct Message {
    pub id: u32,
    pub sender: Pubkey,
    pub created_at: i64,
    pub flags: u8,
    pub content: String,
}