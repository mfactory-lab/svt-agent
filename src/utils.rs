use crate::constants::{COMMAND_DELIMITER, MESSENGER_PROGRAM_ID};
use crate::encryption::decrypt_message;
use crate::runner::Task;
use anchor_client::solana_client::nonblocking::rpc_client::RpcClient;
use anchor_client::solana_sdk::instruction::{AccountMeta, Instruction};
use anchor_client::solana_sdk::message::Message;
use anchor_client::solana_sdk::signature::{Keypair, Signature};
use anchor_client::solana_sdk::signer::Signer;
use anchor_client::solana_sdk::system_program;
use anchor_client::solana_sdk::transaction::Transaction;
use anchor_lang::prelude::*;
use anyhow::Error;
use anyhow::Result;
use std::str::FromStr;

pub fn convert_message_to_task(msg: crate::state::Message, cek: &[u8]) -> Result<Task> {
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

pub async fn set_last_read_message_id(
    client: &RpcClient,
    message_id: u64,
    channel: &Pubkey,
    authority: &Keypair,
) -> Result<Signature> {
    let program_id = Pubkey::from_str(MESSENGER_PROGRAM_ID)?;

    let membership = Pubkey::find_program_address(
        &[&channel.to_bytes(), &authority.pubkey().to_bytes()],
        &program_id,
    );

    let account_metas = vec![
        AccountMeta::new(*channel, false),
        AccountMeta::new(membership.0, false),
        AccountMeta::new_readonly(authority.pubkey(), true),
        AccountMeta::new_readonly(system_program::id(), false),
    ];

    let message = Message::new(
        &[Instruction::new_with_bytes(
            program_id,
            &[
                [54, 166, 48, 51, 234, 46, 110, 163],
                message_id.to_le_bytes(),
            ]
            .concat(),
            account_metas,
        )],
        Some(&authority.pubkey()),
    );

    let mut tx = Transaction::new_unsigned(message);
    let blockhash = client.get_latest_blockhash().await?;
    tx.sign(&[authority], blockhash);

    let sig = client.send_and_confirm_transaction(&tx).await?;

    Ok(sig)
}
