use super::encryption::*;
use super::state::*;
use anchor_client::solana_client::nonblocking::rpc_client::RpcClient;
use anchor_client::solana_sdk::commitment_config::CommitmentConfig;
use anchor_client::solana_sdk::instruction::{AccountMeta, Instruction};
use anchor_client::solana_sdk::message::Message;
use anchor_client::solana_sdk::signature::{Keypair, Signature};
use anchor_client::solana_sdk::signer::Signer;
use anchor_client::solana_sdk::system_program;
use anchor_client::solana_sdk::transaction::Transaction;
use anchor_client::Cluster;
use anchor_lang::prelude::*;
use anyhow::Error;
use anyhow::Result;
use tracing::info;

pub struct MessengerClient {
    pub program_id: Pubkey,
    pub rpc: RpcClient,
    pub cluster: Cluster,
    pub authority: Keypair,
}

impl MessengerClient {
    pub fn new(cluster: Cluster, program_id: Pubkey, authority: Keypair) -> Self {
        let rpc = RpcClient::new_with_commitment(
            cluster.url().to_string(),
            CommitmentConfig::confirmed(),
        );

        Self {
            rpc,
            cluster,
            program_id,
            authority,
        }
    }

    /// Retrieve [authority] pubkey
    pub fn authority_pubkey(&self) -> Pubkey {
        self.authority.pubkey()
    }

    /// Retrieve [authority] balance
    pub async fn get_balance(&self) -> Result<u64> {
        self.rpc
            .get_balance(&self.authority_pubkey())
            .await
            .map_err(|e| Error::from(e))
    }

    #[tracing::instrument(skip(self))]
    pub async fn join_channel(&self, channel: &Pubkey, name: Option<String>) -> Result<Signature> {
        let authority = self.authority_pubkey();
        let membership = self.membership_pda(channel, &authority);
        let device = self.device_pda(&membership.0, &authority);

        let account_metas = vec![
            AccountMeta::new(*channel, false),
            AccountMeta::new(membership.0, false),
            AccountMeta::new(device.0, false),
            AccountMeta::new(authority, true),
            AccountMeta::new_readonly(authority, true),
            AccountMeta::new_readonly(system_program::id(), false),
        ];

        let mut data = vec![
            // discriminator
            124, 39, 115, 89, 217, 26, 38, 29,
        ];

        JoinChannelData::serialize(
            &JoinChannelData {
                name: name.unwrap_or_default(),
                authority: None,
            },
            &mut data,
        )?;

        let message = Message::new(
            &[Instruction::new_with_bytes(
                self.program_id,
                data.as_slice(),
                account_metas,
            )],
            Some(&authority),
        );

        let sig = self.send_transaction(message, None).await?;

        Ok(sig)
    }

    #[tracing::instrument(skip(self))]
    pub async fn read_message(&self, message_id: u64, channel: &Pubkey) -> Result<Signature> {
        let authority = self.authority_pubkey();
        let membership = self.membership_pda(channel, &authority);

        let account_metas = vec![
            AccountMeta::new(*channel, false),
            AccountMeta::new(membership.0, false),
            AccountMeta::new_readonly(authority, true),
            AccountMeta::new_readonly(system_program::id(), false),
        ];

        let discriminator: &[u8] = &[54, 166, 48, 51, 234, 46, 110, 163];

        let message = Message::new(
            &[Instruction::new_with_bytes(
                self.program_id,
                &[discriminator, &message_id.to_le_bytes()].concat(),
                account_metas,
            )],
            Some(&authority),
        );

        let sig = self.send_transaction(message, None).await?;

        Ok(sig)
    }

    #[tracing::instrument(skip(self))]
    pub async fn load_cek(&self, channel: &Pubkey) -> Result<Vec<u8>> {
        info!("Loading device...");
        let device = self.load_device(channel).await?;
        info!("Decrypting CEK...");
        decrypt_cek(device.cek, self.authority.secret().as_bytes())
    }

    #[tracing::instrument(skip(self))]
    pub async fn load_membership(&self, channel: &Pubkey) -> Result<ChannelMembership> {
        let authority = &self.authority_pubkey();
        let pda = self.membership_pda(channel, authority);
        self.load_account(&pda.0).await
    }

    #[tracing::instrument(skip(self))]
    pub async fn load_device(&self, channel: &Pubkey) -> Result<ChannelDevice> {
        let authority = &self.authority_pubkey();
        let membership_pda = self.membership_pda(channel, authority);
        let pda = self.device_pda(&membership_pda.0, authority);
        self.load_account(&pda.0).await
    }

    #[tracing::instrument(skip(self))]
    pub async fn load_channel(&self, channel: &Pubkey) -> Result<Channel> {
        self.load_account(channel).await
    }

    pub async fn load_account<T: AnchorDeserialize>(&self, addr: &Pubkey) -> Result<T> {
        let data = self.rpc.get_account_data(addr).await?;
        // skip anchor discriminator
        let account = T::deserialize(&mut &data.as_slice()[8..])?;
        Ok(account)
    }

    pub fn membership_pda(&self, channel: &Pubkey, authority: &Pubkey) -> (Pubkey, u8) {
        Pubkey::find_program_address(
            &[&channel.to_bytes(), &authority.to_bytes()],
            &self.program_id,
        )
    }

    pub fn device_pda(&self, membership: &Pubkey, key: &Pubkey) -> (Pubkey, u8) {
        Pubkey::find_program_address(&[&membership.to_bytes(), &key.to_bytes()], &self.program_id)
    }

    async fn send_transaction(
        &self,
        message: Message,
        signers: Option<Vec<&Keypair>>,
    ) -> Result<Signature> {
        let mut tx = Transaction::new_unsigned(message);
        let blockhash = self.rpc.get_latest_blockhash().await?;

        tx.sign(&[&self.authority], blockhash);
        if let Some(signers) = signers {
            tx.partial_sign(&signers, blockhash);
        }

        let sig = self.rpc.send_and_confirm_transaction(&tx).await?;

        Ok(sig)
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct JoinChannelData {
    pub name: String,
    pub authority: Option<Pubkey>,
}
