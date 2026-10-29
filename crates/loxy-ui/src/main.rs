#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

slint::include_modules!();

mod character_renderer;

use character_renderer::{ArmModel, Character3DScene};

use anyhow::Result;
use chrono::Utc;
use loxy_core::{
    auth,
    config::{CloseBehavior, LauncherConfig},
    discord::DiscordRpcManager,
    java,
    launcher::{push_log, GameLogEntry, WindowAction},
    modpack::{
        extract_icon_from_archive, fetch_or_fallback_manifest, get_installed_modrinth_projects,
        get_modrinth_mod_versions, install_modrinth_mod, list_installed_mods, search_modrinth_mods,
        synchronize_modpack, toggle_or_delete_installed_mod,
    },
    ping::ping_minecraft_server,
};
use reqwest::Client;
use slint::{ComponentHandle, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex,
};
use std::time::Duration;

type IconBuffer = SharedPixelBuffer<Rgba8Pixel>;
type IconCache = Arc<Mutex<HashMap<String, IconBuffer>>>;

fn decode_image_to_buffer(bytes: &[u8], target_size: u32) -> Option<IconBuffer> {
    let img = image::load_from_memory(bytes).ok()?;
    let resized = if img.width() != target_size || img.height() != target_size {
        img.resize_exact(target_size, target_size, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let rgba = resized.to_rgba8();
    let (w, h) = rgba.dimensions();
    Some(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
        rgba.as_raw(),
        w,
        h,
    ))
}

fn extract_head_from_skin(bytes: &[u8], target_size: u32) -> Option<IconBuffer> {
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    if img.width() < 16 || img.height() < 16 {
        return None;
    }

    let mut head = image::RgbaImage::new(8, 8);
    // Base face at (8, 8, 8, 8)
    for y in 0..8 {
        for x in 0..8 {
            head.put_pixel(x, y, *img.get_pixel(8 + x, 8 + y));
        }
    }

    // Outer hat layer at (40, 8, 8, 8) if skin dimensions permit
    if img.width() >= 48 && img.height() >= 16 {
        for y in 0..8 {
            for x in 0..8 {
                let hat_p = img.get_pixel(40 + x, 8 + y);
                let alpha = hat_p[3] as u32;
                if alpha > 0 {
                    if alpha == 255 {
                        head.put_pixel(x, y, *hat_p);
                    } else {
                        let base_p = head.get_pixel_mut(x, y);
                        let inv_alpha = 255 - alpha;
                        for c in 0..3 {
                            base_p[c] = ((hat_p[c] as u32 * alpha + base_p[c] as u32 * inv_alpha) / 255) as u8;
                        }
                        base_p[3] = 255;
                    }
                }
            }
        }
    }

    let upscaled = image::imageops::resize(&head, target_size, target_size, image::imageops::FilterType::Nearest);
    let (w, h) = upscaled.dimensions();
    Some(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
        upscaled.as_raw(),
        w,
        h,
    ))
}

fn format_downloads(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // 1. Force Pure Software Rendering to guarantee ~20-35MB RAM and 0 GPU/LLVM overhead
    std::env::set_var("SLINT_BACKEND", "winit-software");

    let main_window = MainWindow::new()?;
    let ui_handle = main_window.as_weak();

    // 2. Setup Shared Backend State
    let config = Arc::new(Mutex::new(LauncherConfig::load().unwrap_or_default()));
    let logs = Arc::new(Mutex::new(Vec::<GameLogEntry>::new()));
    let icon_cache: IconCache = Arc::new(Mutex::new(HashMap::new()));
    let search_seq = Arc::new(AtomicU64::new(0));
    let mod_filters_initialized = Arc::new(AtomicBool::new(false));

    let http_client = Client::builder()
        .user_agent("LoxyLauncher-Native/0.1.0 (https://github.com/HFW-Team/LoxyLauncher)")
        .pool_max_idle_per_host(32)
        .tcp_nodelay(true)
        .timeout(Duration::from_secs(120))
        .build()?;
    let discord = Arc::new(DiscordRpcManager::new());

    // 3. Initialize UI with Current Configuration, Detected JDKs & System Stats
    let sys_mem = LauncherConfig::system_memory_info();
    let sys_javas = java::detect_system_javas();
    {
        let jdks: Vec<DetectedJavaData> = sys_javas
            .into_iter()
            .map(|j| DetectedJavaData {
                path: j.path.into(),
                major: j.major_version as i32,
                version_string: j.label.clone().into(),
                label: j.label.into(),
            })
            .collect();
        main_window.set_detected_javas(ModelRc::new(VecModel::from(jdks)));
    }

    {
        let cfg = config.lock().unwrap();
        discord.update_status(cfg.discord_rpc_enabled, "In Launcher Menu", "Ready to launch");

        let ui_cfg = ConfigData {
            instance_dir: cfg.instance_dir.clone().into(),
            shared_dir: cfg.shared_dir.clone().into(),
            min_ram_mb: cfg.min_ram_mb as i32,
            max_ram_mb: cfg.max_ram_mb as i32,
            recommended_ram_mb: sys_mem.recommended_max_mb as i32,
            total_system_ram_mb: sys_mem.total_ram_mb as i32,
            jvm_args: cfg.jvm_args.clone().into(),
            java_path: cfg.java_path.clone().unwrap_or_default().into(),
            resolution_width: cfg.resolution_width.to_string().into(),
            resolution_height: cfg.resolution_height.to_string().into(),
            fullscreen: cfg.fullscreen,
            close_behavior: match cfg.close_behavior {
                CloseBehavior::HideAndReopen => 0,
                CloseBehavior::KeepOpen => 1,
                CloseBehavior::CloseLauncher => 2,
            },
            discord_rpc: cfg.discord_rpc_enabled,
            keep_user_mods: cfg.keep_user_mods,
            manifest_url: cfg.manifest_url.clone().into(),
            server_host: cfg.server_host.clone().into(),
            server_port: cfg.server_port.to_string().into(),
        };
        main_window.set_config(ui_cfg);

        update_ui_accounts(&main_window, &cfg, &http_client, ui_handle.clone());
    }

    // 4. Background Startup Task: Initial Ping, Manifest Check & Installed Mods List
    {
        let ui_w = ui_handle.clone();
        let cfg_arc = config.clone();
        let client = http_client.clone();
        let cache = icon_cache.clone();
        let init_flag = mod_filters_initialized.clone();
        tokio::spawn(async move {
            refresh_dashboard_and_ping(ui_w, cfg_arc, client, cache, init_flag).await;
        });
    }

    // 5. Connect Slint Callbacks
    // --- Full Refresh (Manifest + Server Ping) ---
    {
        let ui_w = ui_handle.clone();
        let cfg_arc = config.clone();
        let client = http_client.clone();
        let cache = icon_cache.clone();
        let init_flag = mod_filters_initialized.clone();
        main_window.on_refresh_clicked(move || {
            let ui_w = ui_w.clone();
            let cfg_arc = cfg_arc.clone();
            let client = client.clone();
            let cache = cache.clone();
            let init_flag = init_flag.clone();
            tokio::spawn(async move {
                refresh_dashboard_and_ping(ui_w, cfg_arc, client, cache, init_flag).await;
            });
        });
    }

    // --- Ping Server Only ---
    {
        let ui_w = ui_handle.clone();
        main_window.on_ping_only_clicked(move || {
            let ui_w = ui_w.clone();
            if let Some(ui) = ui_w.upgrade() {
                let mp = ui.get_modpack();
                let host = mp.server_host.to_string();
                let port = mp.server_port as u16;
                ui.set_is_refreshing(true);
                tokio::spawn(async move {
                    let s_status = ping_minecraft_server(&host, port).await;
                    let _ = ui_w.upgrade_in_event_loop(move |ui| {
                        ui.set_server_status(ServerStatusData {
                            online: s_status.online,
                            host: s_status.host.into(),
                            port: s_status.port as i32,
                            motd: s_status.motd.into(),
                            players_online: s_status.players_online as i32,
                            players_max: s_status.players_max as i32,
                            latency_ms: s_status.latency_ms.unwrap_or(0) as i32,
                            has_latency: s_status.latency_ms.is_some(),
                        });
                        ui.set_is_refreshing(false);
                    });
                });
            }
        });
    }

    // --- Copy Server Address ---
    {
        let ui_w = ui_handle.clone();
        main_window.on_copy_server_address_clicked(move || {
            if let Some(ui) = ui_w.upgrade() {
                let mp = ui.get_modpack();
                let addr = if mp.server_port == 25565 {
                    mp.server_host.to_string()
                } else {
                    format!("{}:{}", mp.server_host, mp.server_port)
                };
                if let Ok(mut clip) = arboard::Clipboard::new() {
                    let _ = clip.set_text(addr.clone());
                }
                ui.set_status_banner_type("info".into());
                ui.set_status_banner_text(format!("Copied server address: {}", addr).into());
            }
        });
    }

    // --- Open Instance Folder ---
    {
        let cfg_arc = config.clone();
        main_window.on_open_instance_folder_clicked(move || {
            let dir = {
                let cfg = cfg_arc.lock().unwrap();
                PathBuf::from(&cfg.instance_dir)
            };
            let _ = std::fs::create_dir_all(&dir);
            let _ = open::that(&dir);
        });
    }

    // --- Open Mods Modal (Auto-loads Installed Mods + Initial Modrinth Search) ---
    {
        let ui_w = ui_handle.clone();
        let cfg_arc = config.clone();
        let client = http_client.clone();
        let cache = icon_cache.clone();
        let seq = search_seq.clone();
        main_window.on_open_mods_modal_clicked(move || {
            if let Some(ui) = ui_w.upgrade() {
                ui.set_mod_notice_text("".into());
                ui.set_mods_open(true);

                let instance_dir = {
                    let cfg = cfg_arc.lock().unwrap();
                    PathBuf::from(&cfg.instance_dir)
                };
                let filter_str = ui.get_mod_installed_filter().to_string();
                let cat_filter = ui.get_mod_installed_category().to_string();
                refresh_installed_mods(ui_w.clone(), &instance_dir, &filter_str, &cat_filter, cache.clone());

                if ui.get_mod_search_results().row_count() == 0 {
                    trigger_modrinth_search(
                        ui_w.clone(),
                        client.clone(),
                        cache.clone(),
                        seq.clone(),
                        0,
                    );
                }
            }
        });
    }

    // --- Save RAM ---
    {
        let cfg_arc = config.clone();
        let ui_w = ui_handle.clone();
        main_window.on_save_ram_clicked(move |max_ram| {
            if let Ok(mut cfg) = cfg_arc.lock() {
                cfg.max_ram_mb = (max_ram as u32).max(2048);
                if cfg.min_ram_mb > cfg.max_ram_mb {
                    cfg.min_ram_mb = cfg.max_ram_mb;
                }
                let _ = cfg.save();
            }
            if let Some(ui) = ui_w.upgrade() {
                ui.set_status_banner_type("info".into());
                ui.set_status_banner_text(format!("Allocated RAM set to {} MB.", max_ram).into());
            }
        });
    }

    // --- Save Settings ---
    {
        let cfg_arc = config.clone();
        let ui_w = ui_handle.clone();
        let client = http_client.clone();
        let cache = icon_cache.clone();
        let init_flag = mod_filters_initialized.clone();
        main_window.on_save_settings_clicked(move || {
            if let Some(ui) = ui_w.upgrade() {
                let ui_cfg = ui.get_config();
                if let Ok(mut cfg) = cfg_arc.lock() {
                    cfg.instance_dir = ui_cfg.instance_dir.to_string();
                    cfg.shared_dir = ui_cfg.shared_dir.to_string();
                    cfg.min_ram_mb = ui_cfg.min_ram_mb as u32;
                    cfg.max_ram_mb = (ui_cfg.max_ram_mb as u32).max(2048);
                    if cfg.min_ram_mb > cfg.max_ram_mb {
                        cfg.min_ram_mb = cfg.max_ram_mb;
                    }
                    cfg.jvm_args = ui_cfg.jvm_args.to_string();
                    let jp = ui_cfg.java_path.trim().to_string();
                    cfg.java_path = if jp.is_empty() { None } else { Some(jp) };
                    cfg.resolution_width = ui_cfg
                        .resolution_width
                        .trim()
                        .parse::<u32>()
                        .unwrap_or(1280)
                        .max(640);
                    cfg.resolution_height = ui_cfg
                        .resolution_height
                        .trim()
                        .parse::<u32>()
                        .unwrap_or(720)
                        .max(480);
                    cfg.fullscreen = ui_cfg.fullscreen;
                    cfg.close_behavior = match ui_cfg.close_behavior {
                        1 => CloseBehavior::KeepOpen,
                        2 => CloseBehavior::CloseLauncher,
                        _ => CloseBehavior::HideAndReopen,
                    };
                    cfg.discord_rpc_enabled = ui_cfg.discord_rpc;
                    cfg.keep_user_mods = ui_cfg.keep_user_mods;
                    cfg.manifest_url = ui_cfg.manifest_url.trim().to_string();
                    cfg.server_host = ui_cfg.server_host.trim().to_string();
                    cfg.server_port = ui_cfg
                        .server_port
                        .trim()
                        .parse::<u16>()
                        .unwrap_or(25565);
                    let _ = cfg.save();
                }
                ui.set_status_banner_type("info".into());
                ui.set_status_banner_text("Settings saved.".into());

                let ui_w2 = ui_w.clone();
                let cfg_arc2 = cfg_arc.clone();
                let client2 = client.clone();
                let cache2 = cache.clone();
                let init_flag2 = init_flag.clone();
                tokio::spawn(async move {
                    refresh_dashboard_and_ping(ui_w2, cfg_arc2, client2, cache2, init_flag2).await;
                });
            }
        });
    }

    // --- Toggle Keep User Mods in Mods Dialog ---
    {
        let cfg_arc = config.clone();
        let ui_w = ui_handle.clone();
        main_window.on_toggle_keep_user_mods_clicked(move || {
            if let Some(ui) = ui_w.upgrade() {
                let mut c = ui.get_config();
                c.keep_user_mods = !c.keep_user_mods;
                let new_val = c.keep_user_mods;
                ui.set_config(c);
                if let Ok(mut cfg) = cfg_arc.lock() {
                    cfg.keep_user_mods = new_val;
                    let _ = cfg.save();
                }
            }
        });
    }

    // --- File & Folder Pickers ---
    {
        let ui_w = ui_handle.clone();
        main_window.on_browse_instance_clicked(move || {
            if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                if let Some(ui) = ui_w.upgrade() {
                    let mut c = ui.get_config();
                    c.instance_dir = folder.to_string_lossy().to_string().into();
                    ui.set_config(c);
                }
            }
        });
    }
    {
        let ui_w = ui_handle.clone();
        main_window.on_browse_java_clicked(move || {
            if let Some(file) = rfd::FileDialog::new().pick_file() {
                if let Some(ui) = ui_w.upgrade() {
                    let mut c = ui.get_config();
                    c.java_path = file.to_string_lossy().to_string().into();
                    ui.set_config(c);
                }
            }
        });
    }

    // --- Microsoft Auth Callbacks ---
    {
        let ui_w = ui_handle.clone();
        let cfg_arc = config.clone();
        let client = http_client.clone();
        main_window.on_login_clicked(move || {
            let ui_w = ui_w.clone();
            let cfg_arc = cfg_arc.clone();
            let client = client.clone();
            tokio::spawn(async move {
                let client_id = {
                    let cfg = cfg_arc.lock().unwrap();
                    cfg.azure_client_id.clone()
                };

                let _ = ui_w.upgrade_in_event_loop(|ui| {
                    ui.set_auth_user_code("".into());
                    ui.set_auth_status("Requesting verification code from Microsoft...".into());
                    ui.set_auth_code_copied(false);
                    ui.set_auth_url_copied(false);
                    ui.set_auth_open(true);
                });

                match auth::request_device_code(&client, &client_id).await {
                    Ok(dev_code) => {
                        let dev_clone = dev_code.clone();
                        let _ = ui_w.upgrade_in_event_loop(move |ui| {
                            ui.set_auth_user_code(dev_clone.user_code.into());
                            ui.set_auth_verification_uri(dev_clone.verification_uri.into());
                            ui.set_auth_status(
                                "Waiting for you to complete login in browser...".into(),
                            );
                            ui.set_auth_code_copied(false);
                            ui.set_auth_url_copied(false);
                        });

                        match auth::poll_for_microsoft_token(
                            &client,
                            &client_id,
                            &dev_code.device_code,
                            dev_code.interval,
                            dev_code.expires_in,
                        )
                        .await
                        {
                            Ok(account) => {
                                let cfg_clone = {
                                    let mut cfg = cfg_arc.lock().unwrap();
                                    cfg.add_or_update_account(account.clone());
                                    let _ = cfg.save();
                                    cfg.clone()
                                };
                                let uname = account.username.clone();
                                let ui_w_update = ui_w.clone();
                                let client_update = client.clone();
                                let _ = ui_w.upgrade_in_event_loop(move |ui| {
                                    update_ui_accounts(&ui, &cfg_clone, &client_update, ui_w_update);
                                    ui.set_auth_open(false);
                                    ui.set_status_banner_type("info".into());
                                    ui.set_status_banner_text(
                                        format!("Signed in as {}", uname).into(),
                                    );
                                });
                            }
                            Err(e) => {
                                let err_str = e.to_string();
                                let _ = ui_w.upgrade_in_event_loop(move |ui| {
                                    ui.set_auth_status(format!("Login failed: {}", err_str).into());
                                });
                            }
                        }
                    }
                    Err(e) => {
                        let err_str = e.to_string();
                        let _ = ui_w.upgrade_in_event_loop(move |ui| {
                            ui.set_auth_status(format!("Error starting login: {}", err_str).into());
                        });
                    }
                }
            });
        });
    }

    {
        let cfg_arc = config.clone();
        let ui_w = ui_handle.clone();
        let client = http_client.clone();
        main_window.on_logout_clicked(move || {
            let cfg_clone = {
                let mut cfg = cfg_arc.lock().unwrap();
                cfg.account = None;
                let _ = cfg.save();
                cfg.clone()
            };
            if let Some(ui) = ui_w.upgrade() {
                update_ui_accounts(&ui, &cfg_clone, &client, ui_w.clone());
                ui.set_status_banner_type("info".into());
                ui.set_status_banner_text("Signed out of Microsoft account.".into());
            }
        });
    }

    // --- Fast Account Switcher Callbacks ---
    {
        let cfg_arc = config.clone();
        let ui_w = ui_handle.clone();
        let client = http_client.clone();
        main_window.on_select_account_clicked(move |uuid| {
            let u_str = uuid.to_string();
            let mut cfg = cfg_arc.lock().unwrap();
            if cfg.switch_account(&u_str) {
                let _ = cfg.save();
                let cfg_clone = cfg.clone();
                if let Some(ui) = ui_w.upgrade() {
                    ui.set_accounts_open(false);
                    update_ui_accounts(&ui, &cfg_clone, &client, ui_w.clone());
                    if let Some(acc) = &cfg_clone.account {
                        ui.set_status_banner_type("info".into());
                        ui.set_status_banner_text(
                            format!("Switched to account {}", acc.username).into(),
                        );
                    }
                }
            }
        });
    }

    {
        let cfg_arc = config.clone();
        let ui_w = ui_handle.clone();
        let client = http_client.clone();
        main_window.on_remove_account_clicked(move |uuid| {
            let u_str = uuid.to_string();
            let mut cfg = cfg_arc.lock().unwrap();
            cfg.remove_account(&u_str);
            let _ = cfg.save();
            let cfg_clone = cfg.clone();
            if let Some(ui) = ui_w.upgrade() {
                update_ui_accounts(&ui, &cfg_clone, &client, ui_w.clone());
            }
        });
    }

    {
        let ui_w = ui_handle.clone();
        main_window.on_copy_auth_code_clicked(move || {
            if let Some(ui) = ui_w.upgrade() {
                let code = ui.get_auth_user_code().to_string();
                if let Ok(mut clip) = arboard::Clipboard::new() {
                    let _ = clip.set_text(code);
                    ui.set_auth_code_copied(true);
                }
            }
        });
    }

    {
        let ui_w = ui_handle.clone();
        main_window.on_copy_auth_url_clicked(move || {
            if let Some(ui) = ui_w.upgrade() {
                let uri = ui.get_auth_verification_uri().to_string();
                if let Ok(mut clip) = arboard::Clipboard::new() {
                    let _ = clip.set_text(uri);
                    ui.set_auth_url_copied(true);
                }
            }
        });
    }

    {
        let ui_w = ui_handle.clone();
        main_window.on_open_auth_browser_clicked(move || {
            if let Some(ui) = ui_w.upgrade() {
                let uri = ui.get_auth_verification_uri().to_string();
                let _ = open::that(uri);
            }
        });
    }

    // --- Console Callbacks ---
    {
        let logs_arc = logs.clone();
        let ui_w = ui_handle.clone();
        main_window.on_console_filter_selected(move |lvl| {
            let lvl_str = lvl.to_string();
            refresh_console_logs_view(&ui_w, &logs_arc, &lvl_str);
        });
    }

    {
        let logs_arc = logs.clone();
        let ui_w = ui_handle.clone();
        main_window.on_copy_console_logs_clicked(move || {
            let text = {
                let guard = logs_arc.lock().unwrap();
                guard
                    .iter()
                    .map(|l| format!("[{}] [{}] {}", l.timestamp, l.level, l.line))
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            if let Ok(mut clip) = arboard::Clipboard::new() {
                let _ = clip.set_text(text);
                if let Some(ui) = ui_w.upgrade() {
                    ui.set_console_logs_copied(true);
                }
            }
        });
    }

    {
        let logs_arc = logs.clone();
        let ui_w = ui_handle.clone();
        main_window.on_clear_console_logs_clicked(move || {
            if let Ok(mut guard) = logs_arc.lock() {
                guard.clear();
            }
            if let Some(ui) = ui_w.upgrade() {
                ui.set_console_logs(ModelRc::new(VecModel::default()));
                ui.set_console_error_count(0);
                ui.set_console_logs_copied(false);
            }
        });
    }

    // --- Modrinth Real-Time Search & Filter Callbacks ---
    {
        let ui_w = ui_handle.clone();
        let client = http_client.clone();
        let cache = icon_cache.clone();
        let seq = search_seq.clone();
        main_window.on_mod_search_edited(move |_q| {
            trigger_modrinth_search(
                ui_w.clone(),
                client.clone(),
                cache.clone(),
                seq.clone(),
                220,
            );
        });
    }

    {
        let ui_w = ui_handle.clone();
        let client = http_client.clone();
        let cache = icon_cache.clone();
        let seq = search_seq.clone();
        main_window.on_mod_search_triggered(move |_q| {
            trigger_modrinth_search(ui_w.clone(), client.clone(), cache.clone(), seq.clone(), 0);
        });
    }

    {
        let ui_w = ui_handle.clone();
        let client = http_client.clone();
        let cache = icon_cache.clone();
        let seq = search_seq.clone();
        main_window.on_mod_mc_version_edited(move |_v| {
            if let Some(ui) = ui_w.upgrade() {
                ui.set_mod_expanded_project_id("".into());
            }
            trigger_modrinth_search(
                ui_w.clone(),
                client.clone(),
                cache.clone(),
                seq.clone(),
                250,
            );
        });
    }

    {
        let ui_w = ui_handle.clone();
        let client = http_client.clone();
        let cache = icon_cache.clone();
        let seq = search_seq.clone();
        main_window.on_mod_toggle_mc_version_all(move || {
            if let Some(ui) = ui_w.upgrade() {
                let cur = ui.get_mod_filter_mc_version().to_string();
                let next = if cur.eq_ignore_ascii_case("all") {
                    ui.get_modpack().mc_version.to_string()
                } else {
                    "all".to_string()
                };
                ui.set_mod_filter_mc_version(next.into());
                ui.set_mod_expanded_project_id("".into());
            }
            trigger_modrinth_search(ui_w.clone(), client.clone(), cache.clone(), seq.clone(), 0);
        });
    }

    {
        let ui_w = ui_handle.clone();
        let client = http_client.clone();
        let cache = icon_cache.clone();
        let seq = search_seq.clone();
        main_window.on_mod_loader_selected(move |loader| {
            if let Some(ui) = ui_w.upgrade() {
                ui.set_mod_filter_loader(loader);
                ui.set_mod_expanded_project_id("".into());
            }
            trigger_modrinth_search(ui_w.clone(), client.clone(), cache.clone(), seq.clone(), 0);
        });
    }

    {
        let ui_w = ui_handle.clone();
        let client = http_client.clone();
        let cache = icon_cache.clone();
        let seq = search_seq.clone();
        main_window.on_mod_project_type_selected(move |pt| {
            if let Some(ui) = ui_w.upgrade() {
                ui.set_mod_filter_project_type(pt);
                ui.set_mod_expanded_project_id("".into());
            }
            trigger_modrinth_search(ui_w.clone(), client.clone(), cache.clone(), seq.clone(), 0);
        });
    }

    {
        let ui_w = ui_handle.clone();
        let client = http_client.clone();
        let cache = icon_cache.clone();
        let seq = search_seq.clone();
        main_window.on_mod_sort_selected(move |sort| {
            if let Some(ui) = ui_w.upgrade() {
                ui.set_mod_filter_sort(sort);
            }
            trigger_modrinth_search(ui_w.clone(), client.clone(), cache.clone(), seq.clone(), 0);
        });
    }

    // --- Mod Version Picker Dropdown Toggle ---
    {
        let ui_w = ui_handle.clone();
        let client = http_client.clone();
        let cfg_arc = config.clone();
        main_window.on_mod_toggle_versions_clicked(move |project_id| {
            let pid = project_id.to_string();
            let ui_w = ui_w.clone();
            let client = client.clone();
            let cfg_arc = cfg_arc.clone();

            let (should_fetch, mc_ver, loader) = if let Some(ui) = ui_w.upgrade() {
                if ui.get_mod_expanded_project_id().as_str() == pid {
                    ui.set_mod_expanded_project_id("".into());
                    (false, String::new(), String::new())
                } else {
                    ui.set_mod_expanded_project_id(pid.clone().into());
                    ui.set_mod_loading_versions(true);
                    ui.set_mod_expanded_versions(ModelRc::new(VecModel::default()));
                    (
                        true,
                        ui.get_mod_filter_mc_version().to_string(),
                        ui.get_mod_filter_loader().to_string(),
                    )
                }
            } else {
                (false, String::new(), String::new())
            };

            if should_fetch {
                tokio::spawn(async move {
                    let instance_dir = {
                        let cfg = cfg_arc.lock().unwrap();
                        PathBuf::from(&cfg.instance_dir)
                    };
                    let installed_map =
                        get_installed_modrinth_projects(&instance_dir);
                    let installed_entry = installed_map.get(&pid).cloned();

                    let vers = get_modrinth_mod_versions(
                        &client,
                        &pid,
                        Some(mc_ver.as_str()),
                        Some(loader.as_str()),
                    )
                    .await
                    .unwrap_or_default();

                    let pid_clone = pid.clone();
                    let _ = ui_w.upgrade_in_event_loop(move |ui| {
                        if ui.get_mod_expanded_project_id().as_str() == pid_clone {
                            let items: Vec<ModVersionData> = vers
                                .into_iter()
                                .map(|v| {
                                    let is_selected = installed_entry
                                        .as_ref()
                                        .map(|p| {
                                            p.version_id == v.id
                                                || p.version_number == v.version_number
                                        })
                                        .unwrap_or(false);
                                    ModVersionData {
                                        id: v.id.into(),
                                        project_id: pid_clone.clone().into(),
                                        name: v.name.into(),
                                        version_number: v.version_number.into(),
                                        game_versions: v.game_versions.join(", ").into(),
                                        loaders: v.loaders.join(", ").into(),
                                        date_published: v.date_published.into(),
                                        installing: false,
                                        is_selected,
                                    }
                                })
                                .collect();
                            ui.set_mod_expanded_versions(ModelRc::new(VecModel::from(items)));
                            ui.set_mod_loading_versions(false);
                        }
                    });
                });
            }
        });
    }

    // --- Install Mod (Latest or Specific Version) ---
    {
        let ui_w = ui_handle.clone();
        let client = http_client.clone();
        let cfg_arc = config.clone();
        let cache = icon_cache.clone();
        let seq = search_seq.clone();
        main_window.on_mod_install_clicked(move |project_id, version_id| {
            let pid = project_id.to_string();
            let vid = version_id.to_string();
            let ui_w = ui_w.clone();
            let client = client.clone();
            let cfg_arc = cfg_arc.clone();
            let cache = cache.clone();
            let seq = seq.clone();

            let (mc_ver, loader, project_type) = if let Some(ui) = ui_w.upgrade() {
                set_mod_installing_state(&ui, &pid, &vid, true);
                let mut mc = ui.get_mod_filter_mc_version().trim().to_string();
                if mc.is_empty() || mc.eq_ignore_ascii_case("all") {
                    mc = ui.get_modpack().mc_version.to_string();
                }
                let mut ld = ui.get_mod_filter_loader().trim().to_lowercase();
                if ld.is_empty() || ld == "all" {
                    ld = ui.get_modpack().loader.to_lowercase();
                }
                let pt = ui.get_mod_filter_project_type().to_string();
                (mc, ld, pt)
            } else {
                ("1.21.1".to_string(), "fabric".to_string(), "mod".to_string())
            };

            tokio::spawn(async move {
                let instance_dir = {
                    let mut cfg = cfg_arc.lock().unwrap();
                    if !cfg.keep_user_mods {
                        cfg.keep_user_mods = true;
                        let _ = cfg.save();
                    }
                    PathBuf::from(&cfg.instance_dir)
                };

                let ver_opt = if vid.is_empty() {
                    None
                } else {
                    Some(vid.as_str())
                };

                let res = install_modrinth_mod(
                    &client,
                    &pid,
                    Some(project_type.as_str()),
                    ver_opt,
                    &mc_ver,
                    &loader,
                    &instance_dir,
                )
                .await;

                let pid_done = pid.clone();
                let vid_done = vid.clone();
                match res {
                    Ok(entry) => {
                        let filename = entry.filename;
                        let ui_w2 = ui_w.clone();
                        let client_c = client.clone();
                        let cache_c = cache.clone();
                        let seq_c = seq.clone();
                        let _ = ui_w.upgrade_in_event_loop(move |ui| {
                            set_mod_installing_state(&ui, &pid_done, &vid_done, false);
                            let mut c = ui.get_config();
                            c.keep_user_mods = true;
                            ui.set_config(c);
                            ui.set_mod_notice_text(format!("Installed {}", filename).into());
                            let filter_str = ui.get_mod_installed_filter().to_string();
                            let cat_str = ui.get_mod_installed_category().to_string();
                            refresh_installed_mods(ui_w2.clone(), &instance_dir, &filter_str, &cat_str, cache_c.clone());
                            trigger_modrinth_search(ui_w2, client_c, cache_c, seq_c, 0);
                        });
                    }
                    Err(e) => {
                        let err_str = e.to_string();
                        let _ = ui_w.upgrade_in_event_loop(move |ui| {
                            set_mod_installing_state(&ui, &pid_done, &vid_done, false);
                            ui.set_mod_notice_text(
                                format!("Failed to install: {}", err_str).into(),
                            );
                        });
                    }
                }
            });
        });
    }

    // --- Installed Content Filter, Category, Toggle & Delete Callbacks ---
    {
        let ui_w = ui_handle.clone();
        let cfg_arc = config.clone();
        let cache = icon_cache.clone();
        main_window.on_mod_installed_category_selected(move |cat| {
            let instance_dir = {
                let cfg = cfg_arc.lock().unwrap();
                PathBuf::from(&cfg.instance_dir)
            };
            let (filter_str, cat_str) = if let Some(ui) = ui_w.upgrade() {
                (ui.get_mod_installed_filter().to_string(), cat.to_string())
            } else {
                (String::new(), cat.to_string())
            };
            refresh_installed_mods(ui_w.clone(), &instance_dir, &filter_str, &cat_str, cache.clone());
        });
    }

    {
        let ui_w = ui_handle.clone();
        let cfg_arc = config.clone();
        let cache = icon_cache.clone();
        main_window.on_mod_installed_filter_edited(move |filter| {
            let instance_dir = {
                let cfg = cfg_arc.lock().unwrap();
                PathBuf::from(&cfg.instance_dir)
            };
            let cat_str = ui_w
                .upgrade()
                .map(|ui| ui.get_mod_installed_category().to_string())
                .unwrap_or_else(|| "all".to_string());
            refresh_installed_mods(ui_w.clone(), &instance_dir, filter.as_str(), &cat_str, cache.clone());
        });
    }

    {
        let ui_w = ui_handle.clone();
        let cfg_arc = config.clone();
        let cache = icon_cache.clone();
        main_window.on_mod_toggle_clicked(move |filename| {
            let f = filename.to_string();
            let ui_w = ui_w.clone();
            let cfg_arc = cfg_arc.clone();
            let cache = cache.clone();
            let (filter_str, cat_str) = ui_w
                .upgrade()
                .map(|ui| (ui.get_mod_installed_filter().to_string(), ui.get_mod_installed_category().to_string()))
                .unwrap_or_default();
            tokio::spawn(async move {
                let instance_dir = {
                    let cfg = cfg_arc.lock().unwrap();
                    PathBuf::from(&cfg.instance_dir)
                };
                let _ = toggle_or_delete_installed_mod(&instance_dir, &f, false);
                refresh_installed_mods(ui_w, &instance_dir, &filter_str, &cat_str, cache);
            });
        });
    }

    {
        let ui_w = ui_handle.clone();
        let cfg_arc = config.clone();
        let cache = icon_cache.clone();
        main_window.on_mod_delete_clicked(move |filename| {
            let f = filename.to_string();
            let ui_w = ui_w.clone();
            let cfg_arc = cfg_arc.clone();
            let cache = cache.clone();
            let (filter_str, cat_str) = ui_w
                .upgrade()
                .map(|ui| (ui.get_mod_installed_filter().to_string(), ui.get_mod_installed_category().to_string()))
                .unwrap_or_default();
            tokio::spawn(async move {
                let instance_dir = {
                    let cfg = cfg_arc.lock().unwrap();
                    PathBuf::from(&cfg.instance_dir)
                };
                let _ = toggle_or_delete_installed_mod(&instance_dir, &f, true);
                refresh_installed_mods(ui_w, &instance_dir, &filter_str, &cat_str, cache);
            });
        });
    }

    // --- Skin & Cape Callbacks ---
    struct SkinViewState {
        scene: Character3DScene,
        skin_image: image::RgbaImage,
        cape_image: Option<image::RgbaImage>,
        selected_skin_path: String,
        selected_skin_bytes: Option<Vec<u8>>,
        active_variant: String,
        active_cape_id: String,
        capes: Vec<auth::PlayerCape>,
    }

    fn default_steve_skin() -> image::RgbaImage {
        let mut img = image::RgbaImage::new(64, 64);
        for y in 0..64 {
            for x in 0..64 {
                let color = if y < 16 && x < 32 {
                    if y < 8 {
                        [77, 52, 37, 255]
                    } else if (8..10).contains(&y) {
                        [77, 52, 37, 255]
                    } else if (12..14).contains(&y) && (x == 10 || x == 13) {
                        [44, 53, 128, 255]
                    } else if (12..14).contains(&y) && (x == 9 || x == 14) {
                        [255, 255, 255, 255]
                    } else if y == 14 && (11..=12).contains(&x) {
                        [105, 50, 30, 255]
                    } else {
                        [196, 145, 108, 255]
                    }
                } else if (16..32).contains(&y) && (16..40).contains(&x) {
                    [0, 168, 178, 255]
                } else if (16..32).contains(&y) && (x < 16 || x >= 40) {
                    if y < 20 {
                        [0, 168, 178, 255]
                    } else {
                        [196, 145, 108, 255]
                    }
                } else if (32..48).contains(&y) {
                    if y >= 46 {
                        [100, 100, 100, 255]
                    } else {
                        [43, 56, 143, 255]
                    }
                } else {
                    [0, 0, 0, 0]
                };
                img.put_pixel(x, y, image::Rgba(color));
            }
        }
        img
    }

    fn update_character_preview(ui_w: &slint::Weak<MainWindow>, state: &SkinViewState) {
        let buf = character_renderer::render_minecraft_character(
            290,
            370,
            &state.scene,
            &state.skin_image,
            state.cape_image.as_ref(),
        );
        let _ = ui_w.upgrade_in_event_loop(move |ui| {
            let img = Image::from_rgba8(buf);
            ui.set_skin_preview_image(img);
        });
    }

    let skin_state = Arc::new(Mutex::new(SkinViewState {
        scene: Character3DScene::default(),
        skin_image: default_steve_skin(),
        cape_image: None,
        selected_skin_path: String::new(),
        selected_skin_bytes: None,
        active_variant: "CLASSIC".to_string(),
        active_cape_id: String::new(),
        capes: Vec::new(),
    }));

    {
        let ui_w = ui_handle.clone();
        let cfg_arc = config.clone();
        let client = http_client.clone();
        let state_arc = skin_state.clone();
        main_window.on_open_skin_dialog_clicked(move || {
            let ui_w = ui_w.clone();
            let cfg_arc = cfg_arc.clone();
            let client = client.clone();
            let state_arc = state_arc.clone();

            let acc_opt = {
                let cfg = cfg_arc.lock().unwrap();
                cfg.account.clone()
            };

            let Some(acc) = acc_opt else {
                if let Some(ui) = ui_w.upgrade() {
                    ui.set_status_banner_type("warn".into());
                    ui.set_status_banner_text(
                        "Please sign in with Microsoft to customize skin & capes.".into(),
                    );
                }
                return;
            };

            if let Some(ui) = ui_w.upgrade() {
                ui.set_skin_open(true);
                ui.set_skin_status("Loading skin & capes from Mojang...".into());
            }

            tokio::spawn(async move {
                match auth::fetch_player_profile_details(&client, &acc.access_token).await {
                    Ok(details) => {
                        let mut downloaded_skin = None;
                        let mut skin_variant = "CLASSIC".to_string();

                        if let Some(skin) = details
                            .skins
                            .into_iter()
                            .find(|s| s.state.eq_ignore_ascii_case("ACTIVE"))
                        {
                            skin_variant = if skin.variant.eq_ignore_ascii_case("SLIM") {
                                "SLIM".to_string()
                            } else {
                                "CLASSIC".to_string()
                            };
                            if let Ok(resp) = client.get(&skin.url).send().await {
                                if let Ok(bytes) = resp.bytes().await {
                                    if let Ok(dyn_img) = image::load_from_memory(&bytes) {
                                        downloaded_skin = Some(dyn_img.to_rgba8());
                                    }
                                }
                            }
                        }

                        let mut active_cid = String::new();
                        let mut downloaded_cape = None;
                        let mut cape_items = Vec::new();

                        for c in &details.capes {
                            let is_act = c.state.eq_ignore_ascii_case("ACTIVE");
                            if is_act {
                                active_cid = c.id.clone();
                                if let Ok(resp) = client.get(&c.url).send().await {
                                    if let Ok(bytes) = resp.bytes().await {
                                        if let Ok(dyn_img) = image::load_from_memory(&bytes) {
                                            downloaded_cape = Some(dyn_img.to_rgba8());
                                        }
                                    }
                                }
                            }
                            cape_items.push(CapeData {
                                id: c.id.clone().into(),
                                alias: c.alias.clone().into(),
                                is_active: is_act,
                            });
                        }

                        {
                            let mut st = state_arc.lock().unwrap();
                            if let Some(img) = downloaded_skin {
                                st.skin_image = img;
                            }
                            st.cape_image = downloaded_cape;
                            st.active_variant = skin_variant.clone();
                            st.scene.model = match skin_variant.as_str() {
                                "SLIM" => ArmModel::Slim,
                                _ => ArmModel::Classic,
                            };
                            st.active_cape_id = active_cid.clone();
                            st.capes = details.capes;
                            st.selected_skin_path.clear();
                            st.selected_skin_bytes = None;
                            update_character_preview(&ui_w, &st);
                        }

                        let _ = ui_w.upgrade_in_event_loop(move |ui| {
                            ui.set_skin_active_variant(skin_variant.into());
                            ui.set_skin_capes(ModelRc::new(VecModel::from(cape_items)));
                            ui.set_skin_active_cape_id(active_cid.into());
                            ui.set_skin_selected_path("".into());
                            ui.set_skin_status("".into());
                        });
                    }
                    Err(e) => {
                        let err_msg = e.to_string();
                        let _ = ui_w.upgrade_in_event_loop(move |ui| {
                            ui.set_skin_status(format!("Error loading profile: {}", err_msg).into());
                        });
                    }
                }
            });
        });
    }

    {
        let ui_w = ui_handle.clone();
        let state_arc = skin_state.clone();
        main_window.on_skin_rotate(move |dx, dy| {
            let mut st = state_arc.lock().unwrap();
            st.scene.yaw += dx * 0.015;
            st.scene.pitch = (st.scene.pitch - dy * 0.015).clamp(-0.8, 0.8);
            update_character_preview(&ui_w, &st);
        });
    }

    {
        let ui_w = ui_handle.clone();
        let state_arc = skin_state.clone();
        main_window.on_skin_reset_view(move || {
            let mut st = state_arc.lock().unwrap();
            st.scene.yaw = -0.4;
            st.scene.pitch = 0.15;
            update_character_preview(&ui_w, &st);
        });
    }

    {
        let ui_w = ui_handle.clone();
        let state_arc = skin_state.clone();
        main_window.on_skin_set_variant(move |v| {
            let v_str = v.to_string();
            let mut st = state_arc.lock().unwrap();
            st.active_variant = v_str.clone();
            st.scene.model = match v_str.as_str() {
                "SLIM" => ArmModel::Slim,
                _ => ArmModel::Classic,
            };
            update_character_preview(&ui_w, &st);
            if let Some(ui) = ui_w.upgrade() {
                ui.set_skin_active_variant(v);
            }
        });
    }

    {
        let ui_w = ui_handle.clone();
        let state_arc = skin_state.clone();
        main_window.on_skin_select_file(move || {
            if let Some(file) = rfd::FileDialog::new()
                .add_filter("Minecraft Skin (*.png)", &["png"])
                .pick_file()
            {
                if let Ok(bytes) = std::fs::read(&file) {
                    if let Ok(dyn_img) = image::load_from_memory(&bytes) {
                        let rgba = dyn_img.to_rgba8();
                        let path_name = file
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string();
                        let mut st = state_arc.lock().unwrap();
                        st.skin_image = rgba;
                        st.selected_skin_bytes = Some(bytes);
                        st.selected_skin_path = path_name.clone();
                        update_character_preview(&ui_w, &st);
                        if let Some(ui) = ui_w.upgrade() {
                            ui.set_skin_selected_path(path_name.into());
                            ui.set_skin_status(
                                "Local skin previewed. Click 'UPLOAD SKIN' to apply.".into(),
                            );
                        }
                    }
                }
            }
        });
    }

    {
        let ui_w = ui_handle.clone();
        let cfg_arc = config.clone();
        let client = http_client.clone();
        let state_arc = skin_state.clone();
        main_window.on_skin_upload(move || {
            let ui_w = ui_w.clone();
            let cfg_arc = cfg_arc.clone();
            let client = client.clone();
            let state_arc = state_arc.clone();

            let (bytes_opt, variant) = {
                let st = state_arc.lock().unwrap();
                (st.selected_skin_bytes.clone(), st.active_variant.clone())
            };

            let Some(bytes) = bytes_opt else {
                return;
            };
            let acc_opt = {
                let cfg = cfg_arc.lock().unwrap();
                cfg.account.clone()
            };
            let Some(acc) = acc_opt else {
                return;
            };

            if let Some(ui) = ui_w.upgrade() {
                ui.set_skin_is_uploading(true);
                ui.set_skin_status("Uploading skin to Minecraft Services...".into());
            }

            tokio::spawn(async move {
                match auth::upload_player_skin(&client, &acc.access_token, &bytes, &variant).await {
                    Ok(_) => {
                        let _ = ui_w.upgrade_in_event_loop(|ui| {
                            ui.set_skin_is_uploading(false);
                            ui.set_skin_status("✓ Skin uploaded and applied successfully!".into());
                            ui.set_skin_selected_path("".into());
                            ui.set_status_banner_type("info".into());
                            ui.set_status_banner_text(
                                "Minecraft skin updated successfully.".into(),
                            );
                        });
                    }
                    Err(e) => {
                        let err_msg = e.to_string();
                        let _ = ui_w.upgrade_in_event_loop(move |ui| {
                            ui.set_skin_is_uploading(false);
                            ui.set_skin_status(format!("Upload failed: {}", err_msg).into());
                        });
                    }
                }
            });
        });
    }

    {
        let ui_w = ui_handle.clone();
        let cfg_arc = config.clone();
        let client = http_client.clone();
        let state_arc = skin_state.clone();
        main_window.on_skin_reset(move || {
            let ui_w = ui_w.clone();
            let cfg_arc = cfg_arc.clone();
            let client = client.clone();
            let state_arc = state_arc.clone();

            let acc_opt = {
                let cfg = cfg_arc.lock().unwrap();
                cfg.account.clone()
            };
            let Some(acc) = acc_opt else {
                return;
            };

            tokio::spawn(async move {
                if auth::reset_player_skin(&client, &acc.access_token)
                    .await
                    .is_ok()
                {
                    let mut st = state_arc.lock().unwrap();
                    st.skin_image = default_steve_skin();
                    st.selected_skin_bytes = None;
                    st.selected_skin_path.clear();
                    update_character_preview(&ui_w, &st);
                    let _ = ui_w.upgrade_in_event_loop(|ui| {
                        ui.set_skin_status("Skin reset to default.".into());
                        ui.set_skin_selected_path("".into());
                    });
                }
            });
        });
    }

    {
        let ui_w = ui_handle.clone();
        let cfg_arc = config.clone();
        let client = http_client.clone();
        let state_arc = skin_state.clone();
        main_window.on_skin_equip_cape(move |cape_id| {
            let ui_w = ui_w.clone();
            let cfg_arc = cfg_arc.clone();
            let client = client.clone();
            let state_arc = state_arc.clone();
            let cid = cape_id.to_string();

            let acc_opt = {
                let cfg = cfg_arc.lock().unwrap();
                cfg.account.clone()
            };
            let Some(acc) = acc_opt else {
                return;
            };

            tokio::spawn(async move {
                if auth::set_player_cape(&client, &acc.access_token, Some(&cid))
                    .await
                    .is_ok()
                {
                    let cape_url = {
                        let st = state_arc.lock().unwrap();
                        st.capes.iter().find(|c| c.id == cid).map(|c| c.url.clone())
                    };
                    let mut new_cape_img = None;
                    if let Some(url) = cape_url {
                        if let Ok(resp) = client.get(&url).send().await {
                            if let Ok(b) = resp.bytes().await {
                                if let Ok(dyn_img) = image::load_from_memory(&b) {
                                    new_cape_img = Some(dyn_img.to_rgba8());
                                }
                            }
                        }
                    }
                    {
                        let mut st = state_arc.lock().unwrap();
                        st.active_cape_id = cid.clone();
                        st.cape_image = new_cape_img;
                        update_character_preview(&ui_w, &st);
                    }
                    let cid_c = cid.clone();
                    let _ = ui_w.upgrade_in_event_loop(move |ui| {
                        ui.set_skin_active_cape_id(cid_c.into());
                        ui.set_skin_status("Cape equipped!".into());
                    });
                }
            });
        });
    }

    {
        let ui_w = ui_handle.clone();
        let cfg_arc = config.clone();
        let client = http_client.clone();
        let state_arc = skin_state.clone();
        main_window.on_skin_unequip_cape(move || {
            let ui_w = ui_w.clone();
            let cfg_arc = cfg_arc.clone();
            let client = client.clone();
            let state_arc = state_arc.clone();

            let acc_opt = {
                let cfg = cfg_arc.lock().unwrap();
                cfg.account.clone()
            };
            let Some(acc) = acc_opt else {
                return;
            };

            tokio::spawn(async move {
                if auth::set_player_cape(&client, &acc.access_token, None)
                    .await
                    .is_ok()
                {
                    {
                        let mut st = state_arc.lock().unwrap();
                        st.active_cape_id.clear();
                        st.cape_image = None;
                        update_character_preview(&ui_w, &st);
                    }
                    let _ = ui_w.upgrade_in_event_loop(|ui| {
                        ui.set_skin_active_cape_id("".into());
                        ui.set_skin_status("Cape unequipped.".into());
                    });
                }
            });
        });
    }

    // --- Launch Game (Play) Callback ---
    {
        let ui_w = ui_handle.clone();
        let cfg_arc = config.clone();
        let client = http_client.clone();
        let logs_arc = logs.clone();
        let discord_arc = discord.clone();
        main_window.on_play_clicked(move || {
            let ui_w = ui_w.clone();
            let cfg_arc = cfg_arc.clone();
            let client = client.clone();
            let logs_arc = logs_arc.clone();
            let discord_arc = discord_arc.clone();

            tokio::spawn(async move {
                launch_game_flow(ui_w, cfg_arc, client, logs_arc, discord_arc).await;
            });
        });
    }

    // Run Slint Event Loop
    main_window.run()?;
    Ok(())
}

fn update_ui_accounts(
    ui: &MainWindow,
    cfg: &LauncherConfig,
    client: &Client,
    ui_handle: slint::Weak<MainWindow>,
) {
    if let Some(acc) = &cfg.account {
        ui.set_account(AccountData {
            logged_in: true,
            username: acc.username.clone().into(),
            uuid: acc.uuid.clone().into(),
            avatar: Image::default(),
            has_avatar: false,
        });
        spawn_load_account_avatar(
            ui_handle.clone(),
            client.clone(),
            acc.skin_url.clone(),
            acc.uuid.clone(),
            acc.username.clone(),
        );
    } else {
        ui.set_account(AccountData {
            logged_in: false,
            username: "".into(),
            uuid: "".into(),
            avatar: Image::default(),
            has_avatar: false,
        });
    }

    let active_uuid = cfg.account.as_ref().map(|a| a.uuid.as_str()).unwrap_or("");
    let stored_list: Vec<StoredAccountData> = cfg
        .accounts
        .iter()
        .map(|a| {
            spawn_load_account_avatar(
                ui_handle.clone(),
                client.clone(),
                a.skin_url.clone(),
                a.uuid.clone(),
                a.username.clone(),
            );
            StoredAccountData {
                username: a.username.clone().into(),
                uuid: a.uuid.clone().into(),
                is_active: a.uuid == active_uuid,
                avatar: Image::default(),
                has_avatar: false,
            }
        })
        .collect();
    ui.set_all_accounts(ModelRc::new(VecModel::from(stored_list)));
}

fn spawn_load_account_avatar(
    ui_w: slint::Weak<MainWindow>,
    client: Client,
    skin_url: Option<String>,
    uuid: String,
    username: String,
) {
    tokio::spawn(async move {
        // 1. Direct authoritative Mojang skin texture URL from profile
        if let Some(url) = skin_url {
            if !url.is_empty() {
                if let Ok(resp) = client.get(&url).send().await {
                    if resp.status().is_success() {
                        if let Ok(bytes) = resp.bytes().await {
                            if let Some(buf) = extract_head_from_skin(&bytes, 32) {
                                let u_clone = uuid.clone();
                                let _ = ui_w.upgrade_in_event_loop(move |ui| {
                                    let mut acc = ui.get_account();
                                    if acc.uuid.as_str() == u_clone {
                                        acc.avatar = Image::from_rgba8(buf.clone());
                                        acc.has_avatar = true;
                                        ui.set_account(acc);
                                    }
                                    let all = ui.get_all_accounts();
                                    for i in 0..all.row_count() {
                                        if let Some(mut r) = all.row_data(i) {
                                            if r.uuid.as_str() == u_clone {
                                                r.avatar = Image::from_rgba8(buf.clone());
                                                r.has_avatar = true;
                                                all.set_row_data(i, r);
                                                break;
                                            }
                                        }
                                    }
                                });
                                return;
                            }
                        }
                    }
                }
            }
        }

        // 2. Fallbacks: UUID-based render endpoints (clean UUID without hyphens), then username
        let clean_uuid = uuid.replace('-', "");
        let mut urls = Vec::new();
        if !clean_uuid.is_empty() {
            urls.push(format!("https://mc-heads.net/avatar/{}/64", clean_uuid));
            urls.push(format!("https://minotar.net/helm/{}/64.png", clean_uuid));
        }
        if !username.is_empty() {
            urls.push(format!("https://mc-heads.net/avatar/{}/64", username));
            urls.push(format!("https://minotar.net/helm/{}/64.png", username));
        }

        for url in urls {
            if let Ok(resp) = client.get(&url).send().await {
                if resp.status().is_success() {
                    if let Ok(bytes) = resp.bytes().await {
                        if let Some(buf) = decode_image_to_buffer(&bytes, 32) {
                            let u_clone = uuid.clone();
                            let _ = ui_w.upgrade_in_event_loop(move |ui| {
                                let mut acc = ui.get_account();
                                if acc.uuid.as_str() == u_clone {
                                    acc.avatar = Image::from_rgba8(buf.clone());
                                    acc.has_avatar = true;
                                    ui.set_account(acc);
                                }
                                let all = ui.get_all_accounts();
                                for i in 0..all.row_count() {
                                    if let Some(mut r) = all.row_data(i) {
                                        if r.uuid.as_str() == u_clone {
                                            r.avatar = Image::from_rgba8(buf.clone());
                                            r.has_avatar = true;
                                            all.set_row_data(i, r);
                                            break;
                                        }
                                    }
                                }
                            });
                            return;
                        }
                    }
                }
            }
        }
    });
}

fn trigger_modrinth_search(
    ui_w: slint::Weak<MainWindow>,
    client: Client,
    icon_cache: IconCache,
    search_seq: Arc<AtomicU64>,
    debounce_ms: u64,
) {
    let my_seq = search_seq.fetch_add(1, Ordering::SeqCst) + 1;

    tokio::spawn(async move {
        if debounce_ms > 0 {
            tokio::time::sleep(Duration::from_millis(debounce_ms)).await;
            if search_seq.load(Ordering::SeqCst) != my_seq {
                return;
            }
        }
        run_modrinth_search_from_ui(ui_w, client, icon_cache, search_seq, my_seq).await;
    });
}

async fn run_modrinth_search_from_ui(
    ui_w: slint::Weak<MainWindow>,
    client: Client,
    icon_cache: IconCache,
    search_seq: Arc<AtomicU64>,
    my_seq: u64,
) {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = ui_w.upgrade_in_event_loop(move |ui| {
        ui.set_mod_is_searching(true);
        let q = ui.get_mod_search_query().to_string();
        let pt = ui.get_mod_filter_project_type().to_string();
        let mc = ui.get_mod_filter_mc_version().to_string();
        let ld = ui.get_mod_filter_loader().to_string();
        let st = ui.get_mod_filter_sort().to_string();
        let inst_dir = ui.get_config().instance_dir.to_string();
        let _ = tx.send((q, pt, mc, ld, st, inst_dir));
    });

    let Ok((query, project_type, mc_ver, loader, sort, inst_dir)) = rx.await else {
        return;
    };

    let hits = search_modrinth_mods(
        &client,
        &query,
        Some(project_type.as_str()),
        Some(mc_ver.as_str()),
        Some(loader.as_str()),
        Some(sort.as_str()),
    )
    .await
    .unwrap_or_default();

    if search_seq.load(Ordering::SeqCst) != my_seq {
        return;
    }

    let installed_map = get_installed_modrinth_projects(Path::new(&inst_dir));

    // Collect icon URLs that need downloading
    let mut missing_icons: Vec<(String, String)> = Vec::new();
    let cached_snapshot: HashMap<String, IconBuffer> = {
        let guard = icon_cache.lock().unwrap();
        let mut map = HashMap::new();
        for h in &hits {
            if let Some(url) = &h.icon_url {
                if let Some(buf) = guard.get(url) {
                    map.insert(h.project_id.clone(), buf.clone());
                } else {
                    missing_icons.push((h.project_id.clone(), url.clone()));
                }
            }
        }
        map
    };

    let ui_w_hits = ui_w.clone();
    let seq_check = search_seq.clone();
    let _ = ui_w_hits.upgrade_in_event_loop(move |ui| {
        if seq_check.load(Ordering::SeqCst) != my_seq {
            return;
        }
        let items: Vec<ModHitData> = hits
            .into_iter()
            .map(|h| {
                let (icon, has_icon) = if let Some(buf) = cached_snapshot.get(&h.project_id) {
                    (Image::from_rgba8(buf.clone()), true)
                } else {
                    (Image::default(), false)
                };
                let (is_installed, installed_version, installed_filename) =
                    if let Some(inst) = installed_map.get(&h.project_id) {
                        (true, inst.version_number.clone(), inst.filename.clone())
                    } else {
                        (false, String::new(), String::new())
                    };

                ModHitData {
                    project_id: h.project_id.into(),
                    slug: h.slug.into(),
                    title: h.title.into(),
                    description: h.description.into(),
                    author: h.author.into(),
                    downloads: format_downloads(h.downloads).into(),
                    icon,
                    has_icon,
                    installing: false,
                    versions_expanded: false,
                    project_type: h.project_type.unwrap_or_else(|| "mod".to_string()).into(),
                    is_installed,
                    installed_version: installed_version.into(),
                    installed_filename: installed_filename.into(),
                }
            })
            .collect();
        ui.set_mod_search_results(ModelRc::new(VecModel::from(items)));
        ui.set_mod_is_searching(false);
    });

    // Asynchronously download & decode missing mod icons (bounded concurrency)
    let sem = Arc::new(tokio::sync::Semaphore::new(6));
    for (project_id, url) in missing_icons {
        if search_seq.load(Ordering::SeqCst) != my_seq {
            break;
        }
        let sem_c = sem.clone();
        let client_c = client.clone();
        let cache_c = icon_cache.clone();
        let ui_c = ui_w.clone();
        let seq_c = search_seq.clone();

        tokio::spawn(async move {
            let Ok(_permit) = sem_c.acquire().await else {
                return;
            };
            if seq_c.load(Ordering::SeqCst) != my_seq {
                return;
            }
            if let Ok(resp) = client_c.get(&url).send().await {
                if resp.status().is_success() {
                    if let Ok(bytes) = resp.bytes().await {
                        if let Some(buf) = decode_image_to_buffer(&bytes, 44) {
                            {
                                let mut guard = cache_c.lock().unwrap();
                                if guard.len() >= 160 {
                                    guard.clear();
                                }
                                guard.insert(url, buf.clone());
                            }
                            let _ = ui_c.upgrade_in_event_loop(move |ui| {
                                let model = ui.get_mod_search_results();
                                for i in 0..model.row_count() {
                                    if let Some(mut row) = model.row_data(i) {
                                        if row.project_id.as_str() == project_id {
                                            row.icon = Image::from_rgba8(buf.clone());
                                            row.has_icon = true;
                                            model.set_row_data(i, row);
                                            break;
                                        }
                                    }
                                }
                            });
                        }
                    }
                }
            }
        });
    }
}

fn set_mod_installing_state(ui: &MainWindow, project_id: &str, version_id: &str, installing: bool) {
    let model = ui.get_mod_search_results();
    for i in 0..model.row_count() {
        if let Some(mut row) = model.row_data(i) {
            if row.project_id.as_str() == project_id {
                row.installing = installing;
                model.set_row_data(i, row);
                break;
            }
        }
    }
    let vmodel = ui.get_mod_expanded_versions();
    for i in 0..vmodel.row_count() {
        if let Some(mut vrow) = vmodel.row_data(i) {
            if !version_id.is_empty() && vrow.id.as_str() == version_id {
                vrow.installing = installing;
                if !installing {
                    vrow.is_selected = true;
                }
                vmodel.set_row_data(i, vrow);
            } else if !installing && !version_id.is_empty() {
                if vrow.is_selected {
                    vrow.is_selected = false;
                    vmodel.set_row_data(i, vrow);
                }
            }
        }
    }
}

fn refresh_console_logs_view(
    ui_w: &slint::Weak<MainWindow>,
    logs_arc: &Arc<Mutex<Vec<GameLogEntry>>>,
    filter_level: &str,
) {
    let (filtered, err_count) = {
        let guard = logs_arc.lock().unwrap();
        let errs = guard.iter().filter(|l| l.level == "ERROR").count() as i32;
        let list: Vec<LogEntryData> = guard
            .iter()
            .filter(|l| filter_level == "ALL" || l.level == filter_level)
            .map(|l| LogEntryData {
                timestamp: l.timestamp.clone().into(),
                level: l.level.clone().into(),
                line: l.line.clone().into(),
            })
            .collect();
        (list, errs)
    };

    let _ = ui_w.upgrade_in_event_loop(move |ui| {
        ui.set_console_logs(ModelRc::new(VecModel::from(filtered)));
        ui.set_console_error_count(err_count);
    });
}

async fn refresh_dashboard_and_ping(
    ui_w: slint::Weak<MainWindow>,
    cfg_arc: Arc<Mutex<LauncherConfig>>,
    client: Client,
    icon_cache: IconCache,
    mod_filters_initialized: Arc<AtomicBool>,
) {
    let _ = ui_w.upgrade_in_event_loop(|ui| {
        ui.set_is_refreshing(true);
    });

    let (manifest_url, host, port, instance_dir) = {
        let cfg = cfg_arc.lock().unwrap();
        (
            cfg.manifest_url.clone(),
            cfg.server_host.clone(),
            cfg.server_port,
            PathBuf::from(&cfg.instance_dir),
        )
    };

    let check =
        fetch_or_fallback_manifest(&client, &manifest_url, &host, port, &instance_dir).await;
    let s_status =
        ping_minecraft_server(&check.manifest.server_host, check.manifest.server_port).await;

    let init_filters = !mod_filters_initialized.swap(true, Ordering::SeqCst);
    let ui_w2 = ui_w.clone();
    let cache_clone = icon_cache.clone();

    let _ = ui_w.upgrade_in_event_loop(move |ui| {
        ui.set_is_refreshing(false);

        ui.set_server_status(ServerStatusData {
            online: s_status.online,
            host: s_status.host.into(),
            port: s_status.port as i32,
            motd: s_status.motd.into(),
            players_online: s_status.players_online as i32,
            players_max: s_status.players_max as i32,
            latency_ms: s_status.latency_ms.unwrap_or(0) as i32,
            has_latency: s_status.latency_ms.is_some(),
        });

        let loader_str = format!("{:?}", check.manifest.loader);
        if init_filters {
            ui.set_mod_filter_mc_version(check.manifest.minecraft_version.clone().into());
            ui.set_mod_filter_loader(loader_str.to_lowercase().into());
        }

        if check.is_offline_fallback {
            if let Some(w) = &check.offline_warning {
                ui.set_status_banner_type("warn".into());
                ui.set_status_banner_text(w.clone().into());
            }
        }

        ui.set_modpack(ModpackData {
            name: check.manifest.name.into(),
            version: check.manifest.version.into(),
            mc_version: check.manifest.minecraft_version.into(),
            loader: loader_str.to_uppercase().into(),
            loader_version: check.manifest.loader_version.into(),
            server_host: check.manifest.server_host.into(),
            server_port: check.manifest.server_port as i32,
            update_available: check.update_available,
            is_offline_fallback: check.is_offline_fallback,
            is_mrpack: check.manifest.mrpack_url.is_some(),
            news: check
                .manifest
                .news
                .unwrap_or_else(|| "No announcement posted in the remote manifest.".to_string())
                .into(),
        });

        let filter_str = ui.get_mod_installed_filter().to_string();
        let cat_str = ui.get_mod_installed_category().to_string();
        refresh_installed_mods(ui_w2, &instance_dir, &filter_str, &cat_str, cache_clone);
    });
}

struct RawInstalledModEntry {
    filename: String,
    name: String,
    version: String,
    size_kb: i32,
    enabled: bool,
    is_user_mod: bool,
    category: String,
    icon_buf: Option<IconBuffer>,
}

fn refresh_installed_mods(
    ui_w: slint::Weak<MainWindow>,
    instance_dir: &Path,
    filter: &str,
    category_filter: &str,
    icon_cache: IconCache,
) {
    if let Ok(mods) = list_installed_mods(instance_dir) {
        let needle = filter.trim().to_lowercase();
        let cat = category_filter.trim().to_lowercase();
        let mut user_items: Vec<RawInstalledModEntry> = Vec::new();
        let mut pack_items: Vec<RawInstalledModEntry> = Vec::new();

        let cached_map = {
            let guard = icon_cache.lock().unwrap();
            guard.clone()
        };

        let mut to_extract: Vec<(PathBuf, String)> = Vec::new();

        for m in mods {
            if cat != "all" && m.category != cat {
                continue;
            }
            if !needle.is_empty()
                && !m.filename.to_lowercase().contains(&needle)
                && !m.display_name.to_lowercase().contains(&needle)
                && !m.mod_version.to_lowercase().contains(&needle)
            {
                continue;
            }

            let cache_key = format!("archive://{}/{}", m.category, m.filename);
            let icon_buf = if let Some(buf) = cached_map.get(&cache_key) {
                Some(buf.clone())
            } else {
                let file_path = instance_dir.join(&m.category).join(&m.filename);
                to_extract.push((file_path, cache_key.clone()));
                None
            };

            let item = RawInstalledModEntry {
                filename: m.filename,
                name: m.display_name,
                version: m.mod_version,
                size_kb: m.size_kb as i32,
                enabled: m.enabled,
                is_user_mod: m.is_user_mod,
                category: m.category,
                icon_buf,
            };
            if item.is_user_mod {
                user_items.push(item);
            } else {
                pack_items.push(item);
            }
        }

        let _ = ui_w.upgrade_in_event_loop(move |ui| {
            let map_fn = |m: RawInstalledModEntry| {
                let (icon, has_icon) = if let Some(buf) = m.icon_buf {
                    (Image::from_rgba8(buf), true)
                } else {
                    (Image::default(), false)
                };
                InstalledModData {
                    filename: m.filename.into(),
                    name: m.name.into(),
                    version: m.version.into(),
                    size_kb: m.size_kb,
                    enabled: m.enabled,
                    is_user_mod: m.is_user_mod,
                    category: m.category.into(),
                    icon,
                    has_icon,
                }
            };
            let u_data: Vec<InstalledModData> = user_items.into_iter().map(map_fn).collect();
            let p_data: Vec<InstalledModData> = pack_items.into_iter().map(map_fn).collect();
            ui.set_user_mods(ModelRc::new(VecModel::from(u_data)));
            ui.set_pack_mods(ModelRc::new(VecModel::from(p_data)));
        });

        // Spawn background task to extract icons for archives not yet in cache
        if !to_extract.is_empty() {
            let ui_w2 = ui_w.clone();
            let cache_arc = icon_cache.clone();
            tokio::task::spawn_blocking(move || {
                for (path, key) in to_extract {
                    if let Some(bytes) = extract_icon_from_archive(&path) {
                        if let Some(buf) = decode_image_to_buffer(&bytes, 32) {
                            {
                                let mut guard = cache_arc.lock().unwrap();
                                guard.insert(key.clone(), buf.clone());
                            }
                            let key_clone = key.clone();
                            let _ = ui_w2.upgrade_in_event_loop(move |ui| {
                                let umods = ui.get_user_mods();
                                for i in 0..umods.row_count() {
                                    if let Some(mut row) = umods.row_data(i) {
                                        let row_key = format!("archive://{}/{}", row.category, row.filename);
                                        if row_key == key_clone {
                                            row.icon = Image::from_rgba8(buf.clone());
                                            row.has_icon = true;
                                            umods.set_row_data(i, row);
                                            break;
                                        }
                                    }
                                }
                                let pmods = ui.get_pack_mods();
                                for i in 0..pmods.row_count() {
                                    if let Some(mut row) = pmods.row_data(i) {
                                        let row_key = format!("archive://{}/{}", row.category, row.filename);
                                        if row_key == key_clone {
                                            row.icon = Image::from_rgba8(buf.clone());
                                            row.has_icon = true;
                                            pmods.set_row_data(i, row);
                                            break;
                                        }
                                    }
                                }
                            });
                        }
                    }
                }
            });
        }
    }
}

async fn launch_game_flow(
    ui_w: slint::Weak<MainWindow>,
    cfg_arc: Arc<Mutex<LauncherConfig>>,
    client: Client,
    logs_arc: Arc<Mutex<Vec<GameLogEntry>>>,
    discord_arc: Arc<DiscordRpcManager>,
) {
    let mut cfg = {
        let c = cfg_arc.lock().unwrap();
        c.clone()
    };

    let mut account = match &cfg.account {
        Some(a) => a.clone(),
        None => {
            let _ = ui_w.upgrade_in_event_loop(|ui| {
                ui.set_is_launching(false);
                ui.set_status_banner_type("warn".into());
                ui.set_status_banner_text(
                    "Please sign in with your Microsoft account before launching.".into(),
                );
                ui.set_auth_open(true);
            });
            return;
        }
    };

    let _ = ui_w.upgrade_in_event_loop(|ui| {
        ui.set_is_launching(true);
        ui.set_status_banner_text("".into());
        ui.set_progress(ProgressData {
            active: true,
            stage: "Preparing launch sequence...".into(),
            current_file: "".into(),
            completed: 0,
            total: 1,
            percent: 2,
        });
    });

    // Helper logger pushing to memory and Slint UI
    let ui_log = ui_w.clone();
    let logs_mem = logs_arc.clone();
    let logs_for_view = logs_arc.clone();
    let on_log = Arc::new(move |entry: GameLogEntry| {
        let _ = entry;
        let ui_log_c = ui_log.clone();
        let logs_c = logs_for_view.clone();
        let _ = ui_log.upgrade_in_event_loop(move |ui| {
            let filter_lvl = ui.get_console_filter().to_string();
            refresh_console_logs_view(&ui_log_c, &logs_c, &filter_lvl);
        });
    });

    // Check token expiration
    if Utc::now().timestamp() >= account.expires_at && !account.refresh_token.is_empty() {
        push_log(
            &*on_log,
            &logs_mem,
            "INFO",
            "Refreshing Microsoft authentication session...".to_string(),
        );
        if let Ok(refreshed) =
            auth::refresh_minecraft_session(&client, &cfg.azure_client_id, &account.refresh_token)
                .await
        {
            account = refreshed.clone();
            cfg.account = Some(refreshed);
            let _ = cfg.save();
            let mut guard = cfg_arc.lock().unwrap();
            *guard = cfg.clone();
        }
    }

    let instance_dir = PathBuf::from(&cfg.instance_dir);
    let shared_dir = PathBuf::from(&cfg.shared_dir);

    // Fetch manifest & check updates
    let mut check = fetch_or_fallback_manifest(
        &client,
        &cfg.manifest_url,
        &cfg.server_host,
        cfg.server_port,
        &instance_dir,
    )
    .await;

    if check.is_offline_fallback {
        if let Some(warn) = &check.offline_warning {
            push_log(
                &*on_log,
                &logs_mem,
                "WARN",
                format!("Offline fallback: {}", warn),
            );
        }
    } else if check.update_available {
        let ui_sync = ui_w.clone();
        let sync_res = synchronize_modpack(
            &client,
            &mut check.manifest,
            &instance_dir,
            cfg.keep_user_mods,
            move |p| {
                let _ = ui_sync.upgrade_in_event_loop(move |ui| {
                    ui.set_progress(ProgressData {
                        active: true,
                        stage: p.stage.into(),
                        current_file: p.current_file.into(),
                        completed: p.completed as i32,
                        total: p.total as i32,
                        percent: p.percent as i32,
                    });
                });
            },
        )
        .await;

        if let Err(e) = sync_res {
            let err_str = e.to_string();
            let _ = ui_w.upgrade_in_event_loop(move |ui| {
                ui.set_is_launching(false);
                let mut p = ui.get_progress();
                p.active = false;
                ui.set_progress(p);
                ui.set_status_banner_type("error".into());
                ui.set_status_banner_text(format!("Modpack sync failed: {}", err_str).into());
            });
            return;
        }

        check.update_available = false;
        let _ = ui_w.upgrade_in_event_loop(|ui| {
            let mut m = ui.get_modpack();
            m.update_available = false;
            ui.set_modpack(m);
        });
    } else {
        // Pack is already up to date; load cached manifest so loader/MC versions from .mrpack are preserved
        let cached_path = instance_dir.join("cached-manifest.json");
        if let Ok(raw) = std::fs::read_to_string(&cached_path) {
            if let Ok(cached_manifest) = serde_json::from_str(&raw) {
                check.manifest = cached_manifest;
            }
        }
        push_log(
            &*on_log,
            &logs_mem,
            "INFO",
            format!(
                "Modpack {} v{} is already up to date. Skipping sync.",
                check.manifest.name, check.manifest.version
            ),
        );
    }

    // Resolve / download Java runtime
    let ui_prog = ui_w.clone();
    let mc_ver_label = check.manifest.minecraft_version.clone();
    let _ = ui_prog.upgrade_in_event_loop(move |ui| {
        ui.set_progress(ProgressData {
            active: true,
            stage: "Verifying Java Runtime (JRE)".into(),
            current_file: format!("Minecraft {}", mc_ver_label).into(),
            completed: 1,
            total: 2,
            percent: 50,
        });
    });

    let java_bin = match java::resolve_or_download_java(
        &client,
        &check.manifest.minecraft_version,
        &shared_dir,
        cfg.java_path.as_deref(),
    )
    .await
    {
        Ok(bin) => bin,
        Err(e) => {
            let err_str = e.to_string();
            let _ = ui_w.upgrade_in_event_loop(move |ui| {
                ui.set_is_launching(false);
                let mut p = ui.get_progress();
                p.active = false;
                ui.set_progress(p);
                ui.set_status_banner_type("error".into());
                ui.set_status_banner_text(format!("Java runtime error: {}", err_str).into());
            });
            return;
        }
    };

    discord_arc.update_status(
        cfg.discord_rpc_enabled,
        &format!("Playing {}", check.manifest.name),
        &format!("{} on {}", account.username, check.manifest.server_host),
    );

    let ui_prog2 = ui_w.clone();
    let ui_win = ui_w.clone();
    let ui_exit = ui_w.clone();

    let launch_res = loxy_core::launcher::prepare_and_launch_minecraft(
        &client,
        &cfg,
        &account,
        &check.manifest,
        &java_bin,
        logs_mem,
        move |p| {
            let _ = ui_prog2.upgrade_in_event_loop(move |ui| {
                ui.set_progress(ProgressData {
                    active: true,
                    stage: p.stage.into(),
                    current_file: p.current_file.into(),
                    completed: p.completed as i32,
                    total: p.total as i32,
                    percent: p.percent as i32,
                });
            });
        },
        on_log,
        Arc::new(move |action| match action {
            WindowAction::HideMain => {
                let _ = ui_win.upgrade_in_event_loop(|ui| {
                    let _ = ui.hide();
                });
            }
            WindowAction::ShowAndFocusMain => {
                let _ = ui_win.upgrade_in_event_loop(|ui| {
                    let _ = ui.show();
                });
            }
            WindowAction::ExitLauncher => {
                std::process::exit(0);
            }
        }),
        Arc::new(move |exit_ev: loxy_core::launcher::GameExitEvent| {
            let _ = ui_exit.upgrade_in_event_loop(move |ui| {
                let _ = ui.show();
                ui.set_is_launching(false);
                let mut p = ui.get_progress();
                p.active = false;
                ui.set_progress(p);
                if exit_ev.crashed {
                    let code = exit_ev.exit_code.to_string();
                    ui.set_status_banner_type("error".into());
                    ui.set_status_banner_text(
                        format!(
                            "Minecraft exited with code {}. Inspect the Console for crash details.",
                            code
                        )
                        .into(),
                    );
                    ui.set_console_open(true);
                } else {
                    ui.set_status_banner_type("info".into());
                    ui.set_status_banner_text("Minecraft closed cleanly.".into());
                }
            });
        }),
    )
    .await;

    if let Err(e) = launch_res {
        let err_str = e.to_string();
        let _ = ui_w.upgrade_in_event_loop(move |ui| {
            ui.set_is_launching(false);
            let mut p = ui.get_progress();
            p.active = false;
            ui.set_progress(p);
            ui.set_status_banner_type("error".into());
            ui.set_status_banner_text(format!("Launch error: {}", err_str).into());
            ui.set_console_open(true);
        });
    }
}
