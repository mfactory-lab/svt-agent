use crate::constants::MESSENGER_PROGRAM_ID;
use anchor_lang::prelude::Pubkey;
use std::str::FromStr;

pub fn get_membership_pda(channel: &Pubkey, authority: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[&channel.to_bytes(), &authority.to_bytes()],
        &Pubkey::from_str(MESSENGER_PROGRAM_ID).expect("Invalid messenger program id"),
    )
}

pub fn get_channel_pda(membership: &Pubkey, key: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[&membership.to_bytes(), &key.to_bytes()],
        &Pubkey::from_str(MESSENGER_PROGRAM_ID).expect("Invalid messenger program id"),
    )
}
