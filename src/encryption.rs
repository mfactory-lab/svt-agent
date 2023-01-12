use crate::messenger::CEKData;
use anchor_lang::__private::base64;
use anyhow::Error;
use anyhow::Result;
use chacha20poly1305::aead::{Aead, NewAead, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use sha2::{Digest, Sha256, Sha512};

use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroize;

const XC20P_NONCE_LENGTH: usize = 24;
const XC20P_TAG_LENGTH: usize = 16;

// const ED25519_PUBLIC_KEY_LEN: usize = 32;
const ED25519_SECRET_KEY_LEN: usize = 32;
// const X25519_PUBLIC_KEY_LEN: usize = 32;
const X25519_SECRET_KEY_LEN: usize = 32;

const ECDH_ES_XC20PKW_ALG: &str = "ECDH-ES+XC20PKW";
const ECDH_ES_XC20PKW_KEYLEN: usize = 256;

// pub fn encrypt_cek(cek: &[u8], pubkey: &[u8; 32]) -> Result<CEKData> {
//     todo!()
// }
//
// pub fn encrypt_message<T: AsRef<[u8]>>(msg: T, cek: &[u8]) {
//     todo!()
// }

/// Decrypt an encrypted CEK for the with the key that was used to encrypt it
pub fn decrypt_cek(cek: CEKData, private_key: &[u8]) -> Result<Vec<u8>> {
    let bytes = base64::decode(cek.header)?;
    let encrypted_key = base64::decode(cek.encrypted_key)?;
    let nonce = &bytes[0..XC20P_NONCE_LENGTH];
    let tag = &bytes[XC20P_NONCE_LENGTH..XC20P_NONCE_LENGTH + XC20P_TAG_LENGTH];
    let epk_pub = &bytes[XC20P_NONCE_LENGTH + XC20P_TAG_LENGTH..];

    let curve25519key = ed25519_to_x25519_secret(private_key)?;
    let shared_secret = shared_key(&curve25519key, epk_pub.try_into()?);

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
        .map_err(|e| Error::msg("Message encryption failure"))?;

    Ok(res)
}

/// Decrypt a message with the CEK used to encrypt it
pub fn decrypt_message<T: AsRef<[u8]>>(message: T, cek: &[u8]) -> Result<Vec<u8>> {
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
        .map_err(|e| Error::msg("Message decryption failure"))?;

    Ok(res)
}

// pub fn ed25519_to_x25519_public<T>(
//     public: &T,
// ) -> Result<[u8; X25519_PUBLIC_KEY_LEN], Box<dyn Error>>
// where
//     T: AsRef<[u8]> + ?Sized,
// {
//     let mut ed25519: [u8; ED25519_PUBLIC_KEY_LEN] = public.as_ref().try_into()?;
//
//     let x25519: [u8; X25519_PUBLIC_KEY_LEN] = CompressedEdwardsY(ed25519)
//         .decompress()
//         .map(|edwards| edwards.to_montgomery().0)?;
//
//     ed25519.zeroize();
//
//     Ok(x25519)
// }

pub fn ed25519_to_x25519_secret<T>(secret: &T) -> Result<[u8; X25519_SECRET_KEY_LEN]>
where
    T: AsRef<[u8]> + ?Sized,
{
    let mut ed25519: [u8; ED25519_SECRET_KEY_LEN] = secret.as_ref().try_into()?;
    let mut x25519: [u8; X25519_SECRET_KEY_LEN] = [0; X25519_SECRET_KEY_LEN];
    let hash = Sha512::digest(ed25519);

    x25519.copy_from_slice(&hash[..X25519_SECRET_KEY_LEN]);
    x25519[0] &= 248;
    x25519[31] &= 127;
    x25519[31] |= 64;

    ed25519.zeroize();

    Ok(x25519)
}

/// Returns a shared key between our secret key and a peer's public key.
fn shared_key(secret: &[u8; 32], public: &[u8; 32]) -> [u8; 32] {
    let pk = StaticSecret::from(*secret);
    let sh = pk.diffie_hellman(&PublicKey::from(*public));
    sh.as_bytes().to_owned()
}

/// The Concat KDF (using SHA-256) as defined in Section 5.8.1 of NIST.800-56A
fn concat_kdf(alg: &str, len: usize, z: &[u8], apu: &[u8], apv: &[u8]) -> Result<Vec<u8>> {
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
        digest.update((len as u32 * rounds).to_be_bytes());

        output.extend_from_slice(&digest.finalize_reset());
    }

    output.truncate(len);

    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_client::solana_sdk::signature::Keypair;

    #[test]
    fn test_decrypt_cek() {
        let cek = CEKData {
            header: "vs3gIr710ivLea1va2vAfNmhUfLE+D2bMzA3KnlZlMeXlmc0T5Tbb9Br3XG4lpkpA7wnNcPiSEmh7X2/bmUa1MVBAkntxnUp".to_string(),
            encrypted_key: "lIRaL0hk8yb3J+ASEkKmZqW+x8Y2zo/3K06dN255jaA=".to_string(),
        };

        let kp = Keypair::from_base58_string(
            "2gnHrTqnjf86eDaF2Po8Au7VYLtfbHCB4hFrUsJqeK7HVCjDwU8CVmaQZVegJkHhkK1Kf8PGet21JjxV2sAGmLrN",
        );

        // println!("Secret key: {:?}", kp.to_bytes());
        // println!("Device key: {:?}", kp.pubkey().to_string());

        let res = decrypt_cek(cek, kp.secret().as_bytes()).expect("Cannot decrypt");

        assert_eq!(
            res,
            [
                159, 163, 245, 237, 4, 56, 130, 57, 52, 134, 158, 3, 198, 242, 7, 239, 60, 14, 74,
                29, 65, 21, 109, 66, 139, 187, 226, 89, 32, 167, 36, 154
            ]
        );
    }

    #[test]
    fn test_decrypt_message() {
        let expected = "abc";
        let encrypted = "aFdMrRbgPz6brrc46CeD9ipK1meHn0S5A3I42aiY2bwQ8jaoZqV+qGSIKw==";
        let cek = [
            159, 163, 245, 237, 4, 56, 130, 57, 52, 134, 158, 3, 198, 242, 7, 239, 60, 14, 74, 29,
            65, 21, 109, 66, 139, 187, 226, 89, 32, 167, 36, 154,
        ];
        let res = decrypt_message(encrypted, &cek).expect("Cannot decrypt");
        let actual = String::from_utf8(res).unwrap();
        assert_eq!(actual, expected);
    }
}
