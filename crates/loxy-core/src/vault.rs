use anyhow::{anyhow, bail, Result};
use ring::aead::{
    Aad, BoundKey, Nonce, NonceSequence, OpeningKey, SealingKey, UnboundKey, AES_256_GCM,
    NONCE_LEN,
};
use ring::pbkdf2;
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::num::NonZeroU32;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountTokens {
    pub access_token: String,
    pub refresh_token: String,
}

struct OneNonce(Option<[u8; NONCE_LEN]>);

impl NonceSequence for OneNonce {
    fn advance(&mut self) -> Result<Nonce, ring::error::Unspecified> {
        let bytes = self.0.take().ok_or(ring::error::Unspecified)?;
        Nonce::try_assume_unique_for_key(&bytes)
    }
}

fn get_machine_user_seed() -> Vec<u8> {
    let mut seed = Vec::new();
    if let Some(home) = dirs::home_dir() {
        seed.extend_from_slice(home.to_string_lossy().as_bytes());
    }
    if let Ok(user) = std::env::var("USER").or_else(|_| std::env::var("USERNAME")) {
        seed.extend_from_slice(user.as_bytes());
    }
    if let Ok(mid) = fs::read_to_string("/etc/machine-id") {
        seed.extend_from_slice(mid.trim().as_bytes());
    }
    // Fixed domain separator
    seed.extend_from_slice(b"loxylauncher::auth_vault::v1");
    seed
}

pub fn encrypt_and_save_vault(
    base_dir: &Path,
    tokens: &HashMap<String, AccountTokens>,
) -> Result<()> {
    let raw_json = serde_json::to_vec(tokens)?;
    let rng = SystemRandom::new();
    let mut salt = [0u8; 16];
    rng.fill(&mut salt).map_err(|_| anyhow!("RNG failure"))?;
    let mut nonce_bytes = [0u8; NONCE_LEN];
    rng.fill(&mut nonce_bytes).map_err(|_| anyhow!("RNG failure"))?;

    let seed = get_machine_user_seed();
    let mut key_bytes = [0u8; 32];
    pbkdf2::derive(
        pbkdf2::PBKDF2_HMAC_SHA256,
        NonZeroU32::new(10_000).unwrap(),
        &salt,
        &seed,
        &mut key_bytes,
    );

    let unbound =
        UnboundKey::new(&AES_256_GCM, &key_bytes).map_err(|_| anyhow!("Invalid key"))?;
    let mut sealing_key = SealingKey::new(unbound, OneNonce(Some(nonce_bytes)));

    let mut in_out = raw_json;
    sealing_key
        .seal_in_place_append_tag(Aad::empty(), &mut in_out)
        .map_err(|_| anyhow!("Failed to encrypt auth tokens"))?;

    let mut file_data = Vec::with_capacity(4 + 16 + NONCE_LEN + in_out.len());
    file_data.extend_from_slice(b"LXVT");
    file_data.extend_from_slice(&salt);
    file_data.extend_from_slice(&nonce_bytes);
    file_data.extend_from_slice(&in_out);

    let vault_path = base_dir.join(".auth_vault.dat");
    fs::create_dir_all(base_dir)?;
    fs::write(&vault_path, &file_data)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&vault_path, fs::Permissions::from_mode(0o600));
    }

    Ok(())
}

pub fn load_and_decrypt_vault(base_dir: &Path) -> Result<HashMap<String, AccountTokens>> {
    let vault_path = base_dir.join(".auth_vault.dat");
    if !vault_path.exists() {
        return Ok(HashMap::new());
    }
    let data = fs::read(&vault_path)?;
    if data.len() < 4 + 16 + NONCE_LEN + 16 {
        bail!("Corrupt or empty auth vault file");
    }
    if &data[0..4] != b"LXVT" {
        bail!("Invalid vault file header");
    }
    let salt = &data[4..20];
    let mut nonce_bytes = [0u8; NONCE_LEN];
    nonce_bytes.copy_from_slice(&data[20..20 + NONCE_LEN]);
    let mut ciphertext = data[20 + NONCE_LEN..].to_vec();

    let seed = get_machine_user_seed();
    let mut key_bytes = [0u8; 32];
    pbkdf2::derive(
        pbkdf2::PBKDF2_HMAC_SHA256,
        NonZeroU32::new(10_000).unwrap(),
        salt,
        &seed,
        &mut key_bytes,
    );

    let unbound =
        UnboundKey::new(&AES_256_GCM, &key_bytes).map_err(|_| anyhow!("Invalid key"))?;
    let mut opening_key = OpeningKey::new(unbound, OneNonce(Some(nonce_bytes)));

    let decrypted = opening_key
        .open_in_place(Aad::empty(), &mut ciphertext)
        .map_err(|_| anyhow!("Failed to decrypt auth vault"))?;

    let tokens: HashMap<String, AccountTokens> = serde_json::from_slice(decrypted)?;
    Ok(tokens)
}
