use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use sysinfo::System;

/// Compile-time default remote manifest URL for the dedicated modpack/server.
/// Can be overridden via environment variable `LOXY_MANIFEST_URL` or in settings.
pub const DEFAULT_MANIFEST_URL: &str = match option_env!("LOXY_MANIFEST_URL") {
    Some(url) => url,
    None => "https://raw.githubusercontent.com/loxy-community/modpack-manifest/main/launcher-manifest.json",
};

/// Compile-time default Microsoft OAuth Client ID for Device Code OAuth.
/// Defaults to the pre-whitelisted Minecraft Client ID ("00000000441cc96b") so
/// `/authentication/login_with_xbox` works out of the box without requiring
/// manual Microsoft aka.ms/AppRegInfo approval.
pub const DEFAULT_AZURE_CLIENT_ID: &str = match option_env!("LOXY_AZURE_CLIENT_ID") {
    Some(id) => id,
    None => "00000000441cc96b",
};

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
    pub access_token: String,
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
    pub azure_client_id: String,
    pub account: Option<MinecraftAccount>,
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
        let mut sys = System::new();
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
        if path.exists() {
            let raw = fs::read_to_string(&path)
                .with_context(|| format!("Failed to read config at {:?}", path))?;
            let cfg: LauncherConfig = serde_json::from_str(&raw)
                .with_context(|| "Failed to parse launcher config.json")?;
            Ok(cfg)
        } else {
            let default_cfg = Self::default();
            let _ = default_cfg.save();
            Ok(default_cfg)
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::config_file_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create config directory {:?}", parent))?;
        }
        let serialized = serde_json::to_string_pretty(self)?;
        fs::write(&path, serialized)
            .with_context(|| format!("Failed to write config file {:?}", path))?;
        Ok(())
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
        }
    }
}
