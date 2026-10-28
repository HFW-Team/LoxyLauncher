pub use loxy_core::{auth, config, discord, java, launcher, modpack, ping};

use auth::DeviceCodeResponse;
use chrono::Utc;
use config::{LauncherConfig, MinecraftAccount, SystemMemoryInfo};
use discord::DiscordRpcManager;
use java::DetectedJava;
use launcher::{push_log, GameLogEntry, WindowAction};
use modpack::{
    fetch_or_fallback_manifest, get_modrinth_mod_versions, install_modrinth_mod,
    list_installed_mods, search_modrinth_mods, synchronize_modpack,
    toggle_or_delete_installed_mod, InstalledModEntry, ManifestCheckResult, ModrinthSearchHit,
    ModrinthVersionItem, SyncProgressEvent,
};
use ping::{ping_minecraft_server, ServerStatus};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};

pub struct AppState {
    pub config: Mutex<LauncherConfig>,
    pub http_client: Client,
    pub discord: DiscordRpcManager,
    pub logs: Arc<Mutex<Vec<GameLogEntry>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitialStateResponse {
    pub config: LauncherConfig,
    pub system_memory: SystemMemoryInfo,
    pub detected_javas: Vec<DetectedJava>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardStatus {
    pub manifest_check: ManifestCheckResult,
    pub server_status: ServerStatus,
}

#[tauri::command]
async fn get_initial_state(state: State<'_, AppState>) -> Result<InitialStateResponse, String> {
    let cfg = state
        .config
        .lock()
        .map_err(|e| format!("Config lock poisoned: {}", e))?
        .clone();

    state
        .discord
        .update_status(cfg.discord_rpc_enabled, "In Launcher Menu", "Ready to launch");

    Ok(InitialStateResponse {
        config: cfg,
        system_memory: LauncherConfig::system_memory_info(),
        detected_javas: java::detect_system_javas(),
    })
}

#[tauri::command]
async fn save_launcher_config(
    state: State<'_, AppState>,
    new_config: LauncherConfig,
) -> Result<LauncherConfig, String> {
    new_config.save().map_err(|e| e.to_string())?;
    state.discord.update_status(
        new_config.discord_rpc_enabled,
        "Configuring LoxyLauncher",
        "In Settings",
    );
    if let Ok(mut guard) = state.config.lock() {
        *guard = new_config.clone();
    }
    Ok(new_config)
}

#[tauri::command]
async fn check_manifest_and_server(state: State<'_, AppState>) -> Result<DashboardStatus, String> {
    let cfg = state
        .config
        .lock()
        .map_err(|e| e.to_string())?
        .clone();

    let instance_dir = PathBuf::from(&cfg.instance_dir);
    let manifest_check = fetch_or_fallback_manifest(
        &state.http_client,
        &cfg.manifest_url,
        &cfg.server_host,
        cfg.server_port,
        &instance_dir,
    )
    .await;

    let server_status = ping_minecraft_server(
        &manifest_check.manifest.server_host,
        manifest_check.manifest.server_port,
    )
    .await;

    let status_line = if server_status.online {
        format!(
            "Server Online ({}/{})",
            server_status.players_online, server_status.players_max
        )
    } else {
        "Server Offline".to_string()
    };

    state.discord.update_status(
        cfg.discord_rpc_enabled,
        &format!("Pack: {}", manifest_check.manifest.name),
        &status_line,
    );

    Ok(DashboardStatus {
        manifest_check,
        server_status,
    })
}

#[tauri::command]
async fn ping_server(host: String, port: u16) -> Result<ServerStatus, String> {
    Ok(ping_minecraft_server(&host, port).await)
}

#[tauri::command]
async fn start_microsoft_device_auth(
    state: State<'_, AppState>,
) -> Result<DeviceCodeResponse, String> {
    let client_id = state
        .config
        .lock()
        .map_err(|e| e.to_string())?
        .azure_client_id
        .clone();

    auth::request_device_code(&state.http_client, &client_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn complete_microsoft_device_auth(
    state: State<'_, AppState>,
    device_code: String,
    interval: u64,
    expires_in: u64,
) -> Result<MinecraftAccount, String> {
    let client_id = state
        .config
        .lock()
        .map_err(|e| e.to_string())?
        .azure_client_id
        .clone();

    let account = auth::poll_for_microsoft_token(
        &state.http_client,
        &client_id,
        &device_code,
        interval,
        expires_in,
    )
    .await
    .map_err(|e| e.to_string())?;

    if let Ok(mut cfg) = state.config.lock() {
        cfg.account = Some(account.clone());
        let _ = cfg.save();
    }

    Ok(account)
}

#[tauri::command]
async fn logout_account(state: State<'_, AppState>) -> Result<(), String> {
    if let Ok(mut cfg) = state.config.lock() {
        cfg.account = None;
        cfg.save().map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
async fn get_game_logs(state: State<'_, AppState>) -> Result<Vec<GameLogEntry>, String> {
    let guard = state.logs.lock().map_err(|e| e.to_string())?;
    Ok(guard.clone())
}

#[tauri::command]
async fn clear_game_logs(state: State<'_, AppState>) -> Result<(), String> {
    let mut guard = state.logs.lock().map_err(|e| e.to_string())?;
    guard.clear();
    Ok(())
}

#[tauri::command]
async fn search_mods_on_modrinth(
    state: State<'_, AppState>,
    query: String,
    mc_version: Option<String>,
    loader: Option<String>,
    sort_index: Option<String>,
) -> Result<Vec<ModrinthSearchHit>, String> {
    search_modrinth_mods(
        &state.http_client,
        &query,
        mc_version.as_deref(),
        loader.as_deref(),
        sort_index.as_deref(),
    )
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_mod_versions_on_modrinth(
    state: State<'_, AppState>,
    project_id: String,
    mc_version: Option<String>,
    loader: Option<String>,
) -> Result<Vec<ModrinthVersionItem>, String> {
    get_modrinth_mod_versions(
        &state.http_client,
        &project_id,
        mc_version.as_deref(),
        loader.as_deref(),
    )
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn install_mod_from_modrinth(
    state: State<'_, AppState>,
    project_id: String,
    version_id: Option<String>,
    mc_version: String,
    loader: String,
) -> Result<InstalledModEntry, String> {
    let instance_dir = {
        let cfg = state.config.lock().map_err(|e| e.to_string())?;
        PathBuf::from(&cfg.instance_dir)
    };
    install_modrinth_mod(
        &state.http_client,
        &project_id,
        version_id.as_deref(),
        &mc_version,
        &loader,
        &instance_dir,
    )
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_installed_mods(state: State<'_, AppState>) -> Result<Vec<InstalledModEntry>, String> {
    let instance_dir = {
        let cfg = state.config.lock().map_err(|e| e.to_string())?;
        PathBuf::from(&cfg.instance_dir)
    };
    list_installed_mods(&instance_dir).map_err(|e| e.to_string())
}

#[tauri::command]
async fn manage_installed_mod(
    state: State<'_, AppState>,
    filename: String,
    delete: bool,
) -> Result<Vec<InstalledModEntry>, String> {
    let instance_dir = {
        let cfg = state.config.lock().map_err(|e| e.to_string())?;
        PathBuf::from(&cfg.instance_dir)
    };
    toggle_or_delete_installed_mod(&instance_dir, &filename, delete).map_err(|e| e.to_string())?;
    list_installed_mods(&instance_dir).map_err(|e| e.to_string())
}

#[tauri::command]
async fn sync_and_launch_game(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mut cfg = state
        .config
        .lock()
        .map_err(|e| e.to_string())?
        .clone();

    let mut account = cfg
        .account
        .clone()
        .ok_or_else(|| "Please sign in with your Microsoft account before launching.".to_string())?;

    let app_log = app.clone();
    let emit_log = Arc::new(move |entry: GameLogEntry| {
        let _ = app_log.emit("game-log", entry);
    });

    if Utc::now().timestamp() >= account.expires_at && !account.refresh_token.is_empty() {
        push_log(
            &*emit_log,
            &state.logs,
            "INFO",
            "Refreshing Microsoft authentication session...".to_string(),
        );
        match auth::refresh_minecraft_session(
            &state.http_client,
            &cfg.azure_client_id,
            &account.refresh_token,
        )
        .await
        {
            Ok(refreshed) => {
                account = refreshed.clone();
                cfg.account = Some(refreshed);
                let _ = cfg.save();
                if let Ok(mut guard) = state.config.lock() {
                    *guard = cfg.clone();
                }
            }
            Err(e) => {
                push_log(
                    &*emit_log,
                    &state.logs,
                    "WARN",
                    format!(
                        "Could not refresh Microsoft token ({}). Attempting launch with cached session.",
                        e
                    ),
                );
            }
        }
    }

    let instance_dir = PathBuf::from(&cfg.instance_dir);
    let shared_dir = PathBuf::from(&cfg.shared_dir);

    let mut check = fetch_or_fallback_manifest(
        &state.http_client,
        &cfg.manifest_url,
        &cfg.server_host,
        cfg.server_port,
        &instance_dir,
    )
    .await;

    if check.is_offline_fallback {
        if let Some(warn) = &check.offline_warning {
            push_log(
                &*emit_log,
                &state.logs,
                "WARN",
                format!("Offline fallback: {}", warn),
            );
        }
    } else if check.update_available {
        let app_sync = app.clone();
        synchronize_modpack(
            &state.http_client,
            &mut check.manifest,
            &instance_dir,
            cfg.keep_user_mods,
            move |prog| {
                let _ = app_sync.emit("sync-progress", prog);
            },
        )
        .await
        .map_err(|e| format!("Modpack synchronization failed: {}", e))?;

        check.update_available = false;
        check.installed_version = Some(check.manifest.version.clone());
        let _ = app.emit("manifest-synced", check.clone());
    } else {
        // Pack is already at the latest version; load cached manifest so loader/MC versions from .mrpack are preserved
        let cached_path = instance_dir.join("cached-manifest.json");
        if let Ok(raw) = std::fs::read_to_string(&cached_path) {
            if let Ok(cached_manifest) = serde_json::from_str(&raw) {
                check.manifest = cached_manifest;
            }
        }
        push_log(
            &*emit_log,
            &state.logs,
            "INFO",
            format!(
                "Modpack {} v{} is already up to date. Skipping sync.",
                check.manifest.name, check.manifest.version
            ),
        );
    }

    let _ = app.emit(
        "sync-progress",
        SyncProgressEvent {
            stage: "Verifying Java Runtime (JRE)".to_string(),
            current_file: format!("Minecraft {}", check.manifest.minecraft_version),
            completed: 1,
            total: 2,
            percent: 50,
        },
    );

    let java_bin = java::resolve_or_download_java(
        &state.http_client,
        &check.manifest.minecraft_version,
        &shared_dir,
        cfg.java_path.as_deref(),
    )
    .await
    .map_err(|e| format!("Java runtime resolution failed: {}", e))?;

    push_log(
        &*emit_log,
        &state.logs,
        "INFO",
        format!("Using Java runtime at {:?}", java_bin),
    );

    state.discord.update_status(
        cfg.discord_rpc_enabled,
        &format!("Playing {}", check.manifest.name),
        &format!(
            "{} on {}",
            account.username, check.manifest.server_host
        ),
    );

    let app_prog = app.clone();
    let app_win = app.clone();
    let app_exit = app.clone();

    launcher::prepare_and_launch_minecraft(
        &state.http_client,
        &cfg,
        &account,
        &check.manifest,
        &java_bin,
        state.logs.clone(),
        move |prog| {
            let _ = app_prog.emit("sync-progress", prog);
        },
        emit_log,
        Arc::new(move |win_action| match win_action {
            WindowAction::HideMain => {
                if let Some(win) = app_win.get_webview_window("main") {
                    let _ = win.hide();
                }
            }
            WindowAction::ShowAndFocusMain => {
                if let Some(win) = app_win.get_webview_window("main") {
                    let _ = win.show();
                    let _ = win.set_focus();
                }
            }
            WindowAction::ExitLauncher => {
                app_win.exit(0);
            }
        }),
        Arc::new(move |exit_ev| {
            let _ = app_exit.emit("game-exited", exit_ev);
        }),
    )
    .await
    .map_err(|e| format!("Failed to launch Minecraft: {}", e))?;

    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let config = LauncherConfig::load().unwrap_or_default();
    let http_client = Client::builder()
        .user_agent("LoxyLauncher/0.1.0 (https://github.com/loxy-community/loxylauncher)")
        .pool_max_idle_per_host(32)
        .tcp_nodelay(true)
        .timeout(Duration::from_secs(120))
        .build()
        .expect("Failed to initialize HTTP client");

    let app_state = AppState {
        config: Mutex::new(config),
        http_client,
        discord: DiscordRpcManager::new(),
        logs: Arc::new(Mutex::new(Vec::new())),
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![
            get_initial_state,
            save_launcher_config,
            check_manifest_and_server,
            ping_server,
            start_microsoft_device_auth,
            complete_microsoft_device_auth,
            logout_account,
            get_game_logs,
            clear_game_logs,
            search_mods_on_modrinth,
            get_mod_versions_on_modrinth,
            install_mod_from_modrinth,
            get_installed_mods,
            manage_installed_mod,
            sync_and_launch_game,
        ])
        .run(tauri::generate_context!())
        .expect("error while running LoxyLauncher");
}
