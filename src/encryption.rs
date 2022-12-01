use crate::state::CEKData;
use anchor_client::solana_sdk::pubkey::Pubkey;
use anchor_lang::__private::base64;
use chacha20poly1305::aead::{Aead, NewAead, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use sha2::{Digest, Sha256};
use std::error::Error;
use x25519_dalek::{PublicKey, StaticSecret};

const XC20P_NONCE_LENGTH: usize = 24;
const XC20P_TAG_LENGTH: usize = 16;

const ECDH_ES_XC20PKW_ALG: &str = "ECDH-ES+XC20PKW";
const ECDH_ES_XC20PKW_KEYLEN: usize = 256;

pub type SecretKey = [u8; 32];

/// Decrypt an encrypted CEK for the with the key that was used to encrypt it
pub fn decrypt_cek(cek: CEKData, private_key: &SecretKey) -> Result<Vec<u8>, Box<dyn Error>> {
    let bytes = base64::decode(cek.header)?;
    let encrypted_key = base64::decode(cek.encrypted_key)?;
    let nonce = &bytes[0..XC20P_NONCE_LENGTH];
    let tag = &bytes[XC20P_NONCE_LENGTH..XC20P_NONCE_LENGTH + XC20P_TAG_LENGTH];
    let epk_pub = Pubkey::new(&bytes[XC20P_NONCE_LENGTH + XC20P_TAG_LENGTH..]);

    let curve25519key = ed25519_to_x25519_secret(private_key);
    let shared_secret = shared_key(&curve25519key, &epk_pub);

    // Key Encryption Key
    let kek = concat_kdf(
        ECDH_ES_XC20PKW_ALG,
        ECDH_ES_XC20PKW_KEYLEN,
        Key::from_slice(&shared_secret),
        &[],
        &[],
    )?;

    let cipher = XChaCha20Poly1305::new(Key::from_slice(kek.as_slice()));
    let payload = Payload {
        msg: &[encrypted_key.as_slice(), tag].concat(),
        aad: &[],
    };

    let nonce = XNonce::from_slice(nonce);
    let res = cipher
        .decrypt(nonce, payload)
        .expect("CEK decryption failure");

    Ok(res)
}

/// Decrypt a message with the CEK used to encrypt it
pub fn decrypt_message<T: AsRef<[u8]>>(message: T, cek: &[u8]) -> Result<Vec<u8>, Box<dyn Error>> {
    let bytes = base64::decode(message)?;
    let nonce = &bytes[0..XC20P_NONCE_LENGTH];
    let tag = &bytes[bytes.len() - XC20P_TAG_LENGTH..];
    let ciphertext = &bytes[XC20P_NONCE_LENGTH..bytes.len() - XC20P_TAG_LENGTH];

    let cipher = XChaCha20Poly1305::new(Key::from_slice(cek));

    let res = cipher
        .decrypt(
            XNonce::from_slice(nonce),
            Payload {
                msg: &[ciphertext, tag].concat(),
                aad: &[],
            },
        )
        .expect("Message decryption failure");

    Ok(res)
}

/// Convert ed25519 to x25519
fn ed25519_to_x25519_secret(key: &SecretKey) -> [u8; 32] {
    let pk = curve25519_dalek::edwards::CompressedEdwardsY(*key);
    let pk = pk
        .decompress()
        .expect("Failed to convert key")
        .to_montgomery();
    pk.as_bytes().to_owned()
}

/// Returns a shared key between our secret key and a peer's public key.
fn shared_key(secret: &SecretKey, public: &Pubkey) -> [u8; 32] {
    let pk = StaticSecret::from(*secret);
    let sh = pk.diffie_hellman(&PublicKey::from(public.to_bytes()));
    sh.as_bytes().to_owned()
}

/// The Concat KDF (using SHA-256) as defined in Section 5.8.1 of NIST.800-56A
fn concat_kdf(
    alg: &str,
    len: usize,
    z: &[u8],
    apu: &[u8],
    apv: &[u8],
) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut digest: Sha256 = Sha256::new();
    let mut output: Vec<u8> = Vec::new();

    // since our key length is 256 we only have to do one round
    // let target: usize = (len + (Sha256::output_size() - 1)) / Sha256::output_size();
    let rounds: u32 = 1; //u32::try_from(target)?;

    for count in 0..rounds {
        // Iteration Count
        digest.update((count + 1).to_be_bytes());

        // Derived Secret
        digest.update(z);

        // AlgorithmId
        digest.update((alg.len() as u32).to_be_bytes());
        digest.update(alg.as_bytes());

        // PartyUInfo
        digest.update((apu.len() as u32).to_be_bytes());
        digest.update(apu);

        // PartyVInfo
        digest.update((apv.len() as u32).to_be_bytes());
        digest.update(apv);

        // Shared Key Length
        digest.update(((len * 8) as u32).to_be_bytes());

        output.extend_from_slice(&digest.finalize_reset());
    }

    output.truncate(len);

    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_client::solana_sdk::signature::{Keypair, Signer};

    #[test]
    fn test_decrypt_cek() {
        let cek = CEKData {
            header: "pPl/keSte54NxRKdVH3zzok7kTQZ7hK167m8KWlwjyFY4BmCnqMujEYkPgxz5JVLYmk28XXg9rqsp6SGyYyueFvqtOn2EDxJ".to_string(),
            encrypted_key: "Hmaw0OER0udmva1WdofmM6BO1ikquhghkPeG4+297S0=".to_string(),
        };

        let kp = Keypair::from_base58_string(
            "4MmWSnmarhM148vGcvoG5cTBA42sMvnM6PCZYkquJA4xBx8JkV9KdfDcDRfycRm5Qwz4Xe5m28hDgEmwTkNacrqQ",
        );

        println!("Device key: {:?}", kp.pubkey().to_string());

        let res = decrypt_cek(cek, kp.secret().as_bytes()).expect("Cannot decrypt");

        println!("Device key: {:?}", res);
    }

    #[test]
    fn test_decrypt_message() {
        let expected = "abc";
        let encrypted = "C2TSXg/RL2KqIHWF0BD5ZSv0DOHGIAOLhj/3pRztkVZj91O47cw5UfSJfg==";
        let cek = &[]; // TODO: CEK
        let res = decrypt_message(encrypted, cek).expect("Cannot decrypt");
        let actual = String::from_utf8(res).unwrap();

        assert_eq!(actual, expected);
    }
}
