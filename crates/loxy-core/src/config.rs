use crate::vault::{encrypt_and_save_vault, load_and_decrypt_vault, AccountTokens};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use sysinfo::System;

/// Compile-time default remote manifest URL for the dedicated modpack/server.
/// Can be overridden via environment variable `LOXY_MANIFEST_URL` or in settings.
pub const DEFAULT_MANIFEST_URL: &str = match option_env!("LOXY_MANIFEST_URL") {
    Some(url) => url,
    None => "https://raw.githubusercontent.com/loxy-community/modpack-manifest/main/launcher-manifest.json",
};

/// Verified Mojang Microsoft OAuth Client ID for LoxyLauncher.
pub const AZURE_CLIENT_ID: &str = match option_env!("LOXY_AZURE_CLIENT_ID") {
    Some(id) => id,
    None => "bbf3ce62-2e8d-4a2b-b99c-f1ad59ee7371",
};

pub const DEFAULT_AZURE_CLIENT_ID: &str = AZURE_CLIENT_ID;

fn default_azure_client_id() -> String {
    AZURE_CLIENT_ID.to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CloseBehavior {
    HideAndReopen,
    KeepOpen,
    CloseLauncher,
}

impl Default for CloseBehavior {
    fn default() -> Self {
        Self::HideAndReopen
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MinecraftAccount {
    pub uuid: String,
    pub username: String,
    #[serde(default)]
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: String,
    pub expires_at: i64,
    pub skin_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LauncherConfig {
    pub instance_dir: String,
    pub shared_dir: String,
    pub min_ram_mb: u32,
    pub max_ram_mb: u32,
    pub jvm_args: String,
    pub java_path: Option<String>,
    pub resolution_width: u32,
    pub resolution_height: u32,
    pub fullscreen: bool,
    pub close_behavior: CloseBehavior,
    pub discord_rpc_enabled: bool,
    pub keep_user_mods: bool,
    pub manifest_url: String,
    pub server_host: String,
    pub server_port: u16,
    #[serde(default = "default_azure_client_id", skip_serializing)]
    pub azure_client_id: String,
    pub account: Option<MinecraftAccount>,
    #[serde(default)]
    pub accounts: Vec<MinecraftAccount>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemMemoryInfo {
    pub total_ram_mb: u32,
    pub recommended_max_mb: u32,
}

impl LauncherConfig {
    pub fn base_app_dir() -> PathBuf {
        dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("loxylauncher")
    }

    pub fn config_file_path() -> PathBuf {
        Self::base_app_dir().join("config.json")
    }

    pub fn system_memory_info() -> SystemMemoryInfo {
        let mut sys = System::new_with_specifics(
            sysinfo::RefreshKind::nothing().with_memory(sysinfo::MemoryRefreshKind::everything()),
        );
        sys.refresh_memory();
        let total_mb = (sys.total_memory() / (1024 * 1024)) as u32;
        let total_ram_mb = if total_mb < 4096 { 8192 } else { total_mb };
        let recommended_max_mb = (total_ram_mb / 2).clamp(4096, 8192);
        SystemMemoryInfo {
            total_ram_mb,
            recommended_max_mb,
        }
    }

    pub fn load() -> Result<Self> {
        let path = Self::config_file_path();
        let base_dir = Self::base_app_dir();
        let mut cfg = if path.exists() {
            let raw = fs::read_to_string(&path)
                .with_context(|| format!("Failed to read config at {:?}", path))?;
            serde_json::from_str(&raw)
                .with_context(|| "Failed to parse launcher config.json")?
        } else {
            let default_cfg = Self::default();
            let _ = default_cfg.save();
            default_cfg
        };

        // Decrypt secure tokens from encrypted vault
        let vault_tokens = load_and_decrypt_vault(&base_dir).unwrap_or_default();
        let mut needs_migration_save = false;

        for acc in &mut cfg.accounts {
            if let Some(tokens) = vault_tokens.get(&acc.uuid) {
                acc.access_token = tokens.access_token.clone();
                acc.refresh_token = tokens.refresh_token.clone();
            } else if !acc.refresh_token.is_empty() {
                // Account had tokens in plaintext config.json from previous version;
                // flag for saving into encrypted vault and scrubbing from config.json.
                needs_migration_save = true;
            }
        }

        if let Some(active) = &mut cfg.account {
            if let Some(tokens) = vault_tokens.get(&active.uuid) {
                active.access_token = tokens.access_token.clone();
                active.refresh_token = tokens.refresh_token.clone();
            }
        }

        if cfg.azure_client_id.trim().is_empty() || cfg.azure_client_id == "00000000441cc96b" {
            cfg.azure_client_id = AZURE_CLIENT_ID.to_string();
            needs_migration_save = true;
        }

        if needs_migration_save {
            let _ = cfg.save();
        }

        Ok(cfg)
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::config_file_path();
        let base_dir = Self::base_app_dir();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create config directory {:?}", parent))?;
        }

        // 1. Save sensitive tokens securely into encrypted vault
        let mut vault_map: HashMap<String, AccountTokens> = HashMap::new();
        for acc in &self.accounts {
            if !acc.refresh_token.is_empty() || !acc.access_token.is_empty() {
                vault_map.insert(
                    acc.uuid.clone(),
                    AccountTokens {
                        access_token: acc.access_token.clone(),
                        refresh_token: acc.refresh_token.clone(),
                    },
                );
            }
        }
        if let Some(active) = &self.account {
            if !active.refresh_token.is_empty() || !active.access_token.is_empty() {
                vault_map.insert(
                    active.uuid.clone(),
                    AccountTokens {
                        access_token: active.access_token.clone(),
                        refresh_token: active.refresh_token.clone(),
                    },
                );
            }
        }

        let _ = encrypt_and_save_vault(&base_dir, &vault_map);

        // 2. Sanitize tokens before writing config.json so tokens are never exposed in plaintext
        let mut sanitized = self.clone();
        for acc in &mut sanitized.accounts {
            acc.access_token.clear();
            acc.refresh_token.clear();
        }
        if let Some(active) = &mut sanitized.account {
            active.access_token.clear();
            active.refresh_token.clear();
        }

        let serialized = serde_json::to_string_pretty(&sanitized)?;
        fs::write(&path, serialized)
            .with_context(|| format!("Failed to write config file {:?}", path))?;
        Ok(())
    }

    pub fn add_or_update_account(&mut self, acc: MinecraftAccount) {
        if let Some(existing) = self.accounts.iter_mut().find(|a| a.uuid == acc.uuid) {
            *existing = acc.clone();
        } else {
            self.accounts.push(acc.clone());
        }
        self.account = Some(acc);
    }

    pub fn switch_account(&mut self, uuid: &str) -> bool {
        if let Some(target) = self.accounts.iter().find(|a| a.uuid == uuid).cloned() {
            self.account = Some(target);
            true
        } else {
            false
        }
    }

    pub fn remove_account(&mut self, uuid: &str) {
        self.accounts.retain(|a| a.uuid != uuid);
        if let Some(active) = &self.account {
            if active.uuid == uuid {
                self.account = self.accounts.first().cloned();
            }
        }
    }
}

impl Default for LauncherConfig {
    fn default() -> Self {
        let base = Self::base_app_dir();
        let mem = Self::system_memory_info();
        Self {
            instance_dir: base.join("instance").to_string_lossy().to_string(),
            shared_dir: base.join("shared").to_string_lossy().to_string(),
            min_ram_mb: 2048,
            max_ram_mb: mem.recommended_max_mb,
            jvm_args: "-XX:+UseG1GC -XX:+ParallelRefProcEnabled -XX:MaxGCPauseMillis=200 -XX:+UnlockExperimentalVMOptions -XX:+DisableExplicitGC".to_string(),
            java_path: None,
            resolution_width: 1280,
            resolution_height: 720,
            fullscreen: false,
            close_behavior: CloseBehavior::HideAndReopen,
            discord_rpc_enabled: true,
            keep_user_mods: true,
            manifest_url: DEFAULT_MANIFEST_URL.to_string(),
            server_host: "play.loxy.gg".to_string(),
            server_port: 25565,
            azure_client_id: DEFAULT_AZURE_CLIENT_ID.to_string(),
            account: None,
            accounts: Vec::new(),
        }
    }
}
