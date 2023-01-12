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

#[event]
#[derive(Debug, Clone)]
pub struct NewMessageEvent {
    #[index]
    pub channel: Pubkey,
    pub message: Message,
}

#[event]
#[derive(Debug, Clone)]
pub struct UpdateMessageEvent {
    #[index]
    pub channel: Pubkey,
    pub message: Message,
}

#[event]
#[derive(Debug, Clone)]
pub struct DeleteMessageEvent {
    #[index]
    pub channel: Pubkey,
    pub authority: Pubkey,
    pub id: u64,
}

#[test]
fn test_channel() {
    let mut data: &[u8] = &[
        0, 0, 0, 0, 5, 0, 0, 0, 116, 101, 115, 116, 50, 212, 187, 201, 36, 193, 154, 106, 252, 42,
        57, 224, 82, 49, 66, 121, 115, 239, 73, 20, 146, 111, 168, 32, 147, 213, 73, 8, 203, 3,
        111, 154, 39, 45, 27, 137, 99, 0, 0, 0, 0, 129, 30, 137, 99, 0, 0, 0, 0, 0, 1, 0, 1, 0, 0,
        0, 0, 0, 0, 0, 1, 0, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 212, 187, 201, 36, 193, 154, 106,
        252, 42, 57, 224, 82, 49, 66, 121, 115, 239, 73, 20, 146, 111, 168, 32, 147, 213, 73, 8,
        203, 3, 111, 154, 39, 129, 30, 137, 99, 0, 0, 0, 0, 1, 60, 0, 0, 0, 48, 54, 114, 84, 69,
        79, 99, 102, 121, 48, 72, 86, 99, 83, 71, 119, 47, 105, 73, 86, 116, 75, 47, 88, 67, 69,
        49, 71, 115, 87, 118, 107, 56, 103, 98, 87, 78, 99, 84, 88, 109, 117, 113, 119, 50, 109,
        67, 55, 74, 55, 68, 103, 101, 100, 55, 107, 107, 119, 61, 61, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ];

    let ch = Channel::deserialize(&mut data).unwrap();

    assert_eq!(ch.name, "test2");
    assert_eq!(
        ch.creator.to_string(),
        "FKRWcYU1V5xUW9GjdoFRgh5Ymky6Uk16PHiZnUPbUaEz"
    );
    assert_eq!(ch.member_count, 1);
    assert_eq!(ch.message_count, 1);
    assert_eq!(ch.last_message_at, 1669930625);
    assert_eq!(ch.messages.len(), 1);
    // println!("{:?}", ch);
}
