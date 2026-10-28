slint::include_modules!();

use anyhow::Result;
use chrono::Utc;
use loxy_core::{
    auth,
    config::{CloseBehavior, LauncherConfig},
    discord::DiscordRpcManager,
    java,
    launcher::{push_log, GameLogEntry, WindowAction},
    modpack::{
        fetch_or_fallback_manifest, install_modrinth_mod, list_installed_mods,
        search_modrinth_mods, synchronize_modpack, toggle_or_delete_installed_mod,
    },
    ping::ping_minecraft_server,
};
use reqwest::Client;
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<()> {
    // 1. Force Pure Software Rendering to guarantee ~15-25MB RAM and 0 GPU/LLVM overhead
    std::env::set_var("SLINT_BACKEND", "winit-software");

    let main_window = MainWindow::new()?;
    let ui_handle = main_window.as_weak();

    // 2. Setup Shared Backend State
    let config = Arc::new(Mutex::new(LauncherConfig::load().unwrap_or_default()));
    let logs = Arc::new(Mutex::new(Vec::<GameLogEntry>::new()));
    let http_client = Client::builder()
        .user_agent("LoxyLauncher-Native/0.1.0 (https://github.com/HFW-Team/LoxyLauncher)")
        .pool_max_idle_per_host(32)
        .tcp_nodelay(true)
        .timeout(Duration::from_secs(120))
        .build()?;
    let discord = Arc::new(DiscordRpcManager::new());

    // 3. Initialize UI with Current Configuration & System Stats
    let sys_mem = LauncherConfig::system_memory_info();
    {
        let cfg = config.lock().unwrap();
        discord.update_status(cfg.discord_rpc_enabled, "In Launcher Menu", "Ready to launch");

        let ui_cfg = ConfigData {
            instance_dir: cfg.instance_dir.clone().into(),
            shared_dir: cfg.shared_dir.clone().into(),
            min_ram_mb: cfg.min_ram_mb as i32,
            max_ram_mb: cfg.max_ram_mb as i32,
            jvm_args: cfg.jvm_args.clone().into(),
            java_path: cfg.java_path.clone().unwrap_or_default().into(),
            resolution_width: cfg.resolution_width as i32,
            resolution_height: cfg.resolution_height as i32,
            fullscreen: cfg.fullscreen,
            close_behavior: match cfg.close_behavior {
                CloseBehavior::HideAndReopen => 0,
                CloseBehavior::KeepOpen => 1,
                CloseBehavior::CloseLauncher => 2,
            },
            discord_rpc: cfg.discord_rpc_enabled,
            keep_user_mods: cfg.keep_user_mods,
            azure_client_id: cfg.azure_client_id.clone().into(),
            total_system_ram_mb: sys_mem.total_ram_mb as i32,
        };
        main_window.set_config(ui_cfg);

        if let Some(acc) = &cfg.account {
            main_window.set_account(AccountData {
                logged_in: true,
                username: acc.username.clone().into(),
                uuid: acc.uuid.clone().into(),
            });
        }
    }

    // 4. Background Startup Task: Initial Ping, Manifest Check & Installed Mods List
    {
        let ui_w = ui_handle.clone();
        let cfg_arc = config.clone();
        let client = http_client.clone();
        tokio::spawn(async move {
            refresh_dashboard_and_ping(ui_w, cfg_arc, client).await;
        });
    }

    // 5. Connect Slint Callbacks
    // --- Ping / Refresh ---
    {
        let ui_w = ui_handle.clone();
        let cfg_arc = config.clone();
        let client = http_client.clone();
        main_window.on_ping_clicked(move || {
            let ui_w = ui_w.clone();
            let cfg_arc = cfg_arc.clone();
            let client = client.clone();
            tokio::spawn(async move {
                refresh_dashboard_and_ping(ui_w, cfg_arc, client).await;
            });
        });
    }

    // --- Save RAM ---
    {
        let cfg_arc = config.clone();
        main_window.on_save_ram_clicked(move |max_ram| {
            if let Ok(mut cfg) = cfg_arc.lock() {
                cfg.max_ram_mb = max_ram as u32;
                let _ = cfg.save();
            }
        });
    }

    // --- Save Settings ---
    {
        let cfg_arc = config.clone();
        let ui_w = ui_handle.clone();
        main_window.on_save_settings_clicked(move || {
            if let Some(ui) = ui_w.upgrade() {
                let ui_cfg = ui.get_config();
                if let Ok(mut cfg) = cfg_arc.lock() {
                    cfg.instance_dir = ui_cfg.instance_dir.to_string();
                    cfg.shared_dir = ui_cfg.shared_dir.to_string();
                    cfg.min_ram_mb = ui_cfg.min_ram_mb as u32;
                    cfg.max_ram_mb = ui_cfg.max_ram_mb as u32;
                    cfg.jvm_args = ui_cfg.jvm_args.to_string();
                    cfg.java_path = if ui_cfg.java_path.is_empty() {
                        None
                    } else {
                        Some(ui_cfg.java_path.to_string())
                    };
                    cfg.fullscreen = ui_cfg.fullscreen;
                    cfg.close_behavior = match ui_cfg.close_behavior {
                        1 => CloseBehavior::KeepOpen,
                        2 => CloseBehavior::CloseLauncher,
                        _ => CloseBehavior::HideAndReopen,
                    };
                    cfg.discord_rpc_enabled = ui_cfg.discord_rpc;
                    cfg.keep_user_mods = ui_cfg.keep_user_mods;
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
        main_window.on_browse_shared_clicked(move || {
            if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                if let Some(ui) = ui_w.upgrade() {
                    let mut c = ui.get_config();
                    c.shared_dir = folder.to_string_lossy().to_string().into();
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
                    ui.set_auth_status("Initiating Microsoft login...".into());
                    ui.set_auth_open(true);
                });

                match auth::request_device_code(&client, &client_id).await {
                    Ok(dev_code) => {
                        let dev_clone = dev_code.clone();
                        let _ = ui_w.upgrade_in_event_loop(move |ui| {
                            ui.set_auth_user_code(dev_clone.user_code.into());
                            ui.set_auth_verification_uri(dev_clone.verification_uri.into());
                            ui.set_auth_status("Waiting for browser login approval...".into());
                            ui.set_auth_code_copied(false);
                        });

                        // Poll device code flow
                        match auth::poll_for_microsoft_token(&client, &client_id, &dev_code.device_code, dev_code.interval, dev_code.expires_in).await {
                            Ok(account) => {
                                {
                                    let mut cfg = cfg_arc.lock().unwrap();
                                    cfg.account = Some(account.clone());
                                    let _ = cfg.save();
                                }
                                let _ = ui_w.upgrade_in_event_loop(move |ui| {
                                    ui.set_account(AccountData {
                                        logged_in: true,
                                        username: account.username.into(),
                                        uuid: account.uuid.into(),
                                    });
                                    ui.set_auth_open(false);
                                    ui.set_status_banner_text("Signed in successfully!".into());
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
        main_window.on_logout_clicked(move || {
            {
                let mut cfg = cfg_arc.lock().unwrap();
                cfg.account = None;
                let _ = cfg.save();
            }
            if let Some(ui) = ui_w.upgrade() {
                ui.set_account(AccountData {
                    logged_in: false,
                    username: "".into(),
                    uuid: "".into(),
                });
                ui.set_status_banner_text("Signed out.".into());
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

    // --- Mod Manager Callbacks ---
    {
        let ui_w = ui_handle.clone();
        let client = http_client.clone();
        main_window.on_mod_search_triggered(move |q| {
            let q_str = q.to_string();
            let ui_w = ui_w.clone();
            let client = client.clone();
            tokio::spawn(async move {
                let _ = ui_w.upgrade_in_event_loop(|ui| {
                    ui.set_mod_is_searching(true);
                });

                let hits = search_modrinth_mods(&client, &q_str, None, None, None)
                    .await
                    .unwrap_or_default();

                let _ = ui_w.upgrade_in_event_loop(move |ui| {
                    let items: Vec<ModHitData> = hits
                        .into_iter()
                        .map(|h| ModHitData {
                            project_id: h.project_id.into(),
                            slug: h.slug.into(),
                            title: h.title.into(),
                            description: h.description.into(),
                            author: h.author.into(),
                            downloads: h.downloads as i32,
                            installed: false,
                        })
                        .collect();
                    ui.set_mod_search_results(ModelRc::new(VecModel::from(items)));
                    ui.set_mod_is_searching(false);
                });
            });
        });
    }

    {
        let ui_w = ui_handle.clone();
        let client = http_client.clone();
        let cfg_arc = config.clone();
        main_window.on_mod_install_clicked(move |project_id| {
            let pid = project_id.to_string();
            let ui_w = ui_w.clone();
            let client = client.clone();
            let cfg_arc = cfg_arc.clone();
            tokio::spawn(async move {
                let (instance_dir, mc_ver, loader) = {
                    let cfg = cfg_arc.lock().unwrap();
                    let inst = PathBuf::from(&cfg.instance_dir);
                    let cached_path = inst.join("cached-manifest.json");
                    let (mc, ld) = if let Ok(raw) = std::fs::read_to_string(&cached_path) {
                        if let Ok(m) = serde_json::from_str::<serde_json::Value>(&raw) {
                            (
                                m.get("minecraft_version").and_then(|v| v.as_str()).unwrap_or("1.21.1").to_string(),
                                m.get("loader").and_then(|v| v.as_str()).unwrap_or("fabric").to_string(),
                            )
                        } else {
                            ("1.21.1".to_string(), "fabric".to_string())
                        }
                    } else {
                        ("1.21.1".to_string(), "fabric".to_string())
                    };
                    (inst, mc, ld)
                };

                let _ = ui_w.upgrade_in_event_loop(|ui| {
                    ui.set_status_banner_text("Installing mod from Modrinth...".into());
                });

                match install_modrinth_mod(&client, &pid, None, &mc_ver, &loader, &instance_dir).await {
                    Ok(entry) => {
                        let filename = entry.filename;
                        let _ = ui_w.upgrade_in_event_loop(move |ui| {
                            ui.set_status_banner_text(format!("Installed {}", filename).into());
                        });
                        refresh_installed_mods(ui_w, &instance_dir);
                    }
                    Err(e) => {
                        let err_str = e.to_string();
                        let _ = ui_w.upgrade_in_event_loop(move |ui| {
                            ui.set_status_banner_text(format!("Failed to install mod: {}", err_str).into());
                        });
                    }
                }
            });
        });
    }

    {
        let ui_w = ui_handle.clone();
        let cfg_arc = config.clone();
        main_window.on_mod_toggle_clicked(move |filename| {
            let f = filename.to_string();
            let ui_w = ui_w.clone();
            let cfg_arc = cfg_arc.clone();
            tokio::spawn(async move {
                let instance_dir = {
                    let cfg = cfg_arc.lock().unwrap();
                    PathBuf::from(&cfg.instance_dir)
                };
                let _ = toggle_or_delete_installed_mod(&instance_dir, &f, false);
                refresh_installed_mods(ui_w, &instance_dir);
            });
        });
    }

    {
        let ui_w = ui_handle.clone();
        let cfg_arc = config.clone();
        main_window.on_mod_delete_clicked(move |filename| {
            let f = filename.to_string();
            let ui_w = ui_w.clone();
            let cfg_arc = cfg_arc.clone();
            tokio::spawn(async move {
                let instance_dir = {
                    let cfg = cfg_arc.lock().unwrap();
                    PathBuf::from(&cfg.instance_dir)
                };
                let _ = toggle_or_delete_installed_mod(&instance_dir, &f, true);
                refresh_installed_mods(ui_w, &instance_dir);
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

async fn refresh_dashboard_and_ping(
    ui_w: slint::Weak<MainWindow>,
    cfg_arc: Arc<Mutex<LauncherConfig>>,
    client: Client,
) {
    let (manifest_url, host, port, instance_dir) = {
        let cfg = cfg_arc.lock().unwrap();
        (
            cfg.manifest_url.clone(),
            cfg.server_host.clone(),
            cfg.server_port,
            PathBuf::from(&cfg.instance_dir),
        )
    };

    let check = fetch_or_fallback_manifest(&client, &manifest_url, &host, port, &instance_dir).await;
    let s_status = ping_minecraft_server(&check.manifest.server_host, check.manifest.server_port).await;

    let _ = ui_w.upgrade_in_event_loop(move |ui| {
        ui.set_server_status(ServerStatusData {
            online: s_status.online,
            motd: s_status.motd.into(),
            players_online: s_status.players_online as i32,
            players_max: s_status.players_max as i32,
            latency_ms: s_status.latency_ms.unwrap_or(0) as i32,
        });

        ui.set_modpack(ModpackData {
            name: check.manifest.name.into(),
            version: check.manifest.version.into(),
            mc_version: check.manifest.minecraft_version.into(),
            loader: format!("{:?}", check.manifest.loader).into(),
            loader_version: check.manifest.loader_version.into(),
            server_host: check.manifest.server_host.into(),
            server_port: check.manifest.server_port as i32,
            update_available: check.update_available,
        });
    });

    refresh_installed_mods(ui_w, &instance_dir);
}

fn refresh_installed_mods(ui_w: slint::Weak<MainWindow>, instance_dir: &Path) {
    if let Ok(mods) = list_installed_mods(instance_dir) {
        let items: Vec<InstalledModData> = mods
            .into_iter()
            .map(|m| {
                let disp = m.filename.trim_end_matches(".disabled").to_string();
                InstalledModData {
                    filename: m.filename.into(),
                    display_name: disp.into(),
                    is_disabled: !m.enabled,
                    is_user_added: m.is_user_mod,
                }
            })
            .collect();
        let _ = ui_w.upgrade_in_event_loop(move |ui| {
            ui.set_installed_mods(ModelRc::new(VecModel::from(items)));
        });
    }
}

async fn launch_game_flow(
    ui_w: slint::Weak<MainWindow>,
    cfg_arc: Arc<Mutex<LauncherConfig>>,
    client: Client,
    logs_arc: Arc<Mutex<Vec<GameLogEntry>>>,
    discord_arc: Arc<DiscordRpcManager>,
) {
    let _ = ui_w.upgrade_in_event_loop(|ui| {
        ui.set_is_launching(true);
        ui.set_status_banner_text("Preparing Minecraft launch...".into());
    });

    let mut cfg = {
        let c = cfg_arc.lock().unwrap();
        c.clone()
    };

    let mut account = match &cfg.account {
        Some(a) => a.clone(),
        None => {
            let _ = ui_w.upgrade_in_event_loop(|ui| {
                ui.set_is_launching(false);
                ui.set_status_banner_text("Please sign in with Microsoft before launching.".into());
                ui.set_auth_open(true);
            });
            return;
        }
    };

    // Helper logger pushing to memory and Slint UI
    let ui_log = ui_w.clone();
    let logs_mem = logs_arc.clone();
    let on_log = Arc::new(move |entry: GameLogEntry| {
        let entry_clone = entry.clone();
        let _ = ui_log.upgrade_in_event_loop(move |ui| {
            let mut list: Vec<LogEntryData> = ui.get_console_logs().iter().collect();
            if list.len() >= 500 {
                list.drain(0..100);
            }
            list.push(LogEntryData {
                timestamp: entry_clone.timestamp.into(),
                level: entry_clone.level.into(),
                line: entry_clone.line.into(),
            });
            let errs = list.iter().filter(|l| l.level == "ERROR").count() as i32;
            ui.set_console_logs(ModelRc::new(VecModel::from(list)));
            ui.set_console_error_count(errs);
        });
    });

    // Check token expiration
    if Utc::now().timestamp() >= account.expires_at && !account.refresh_token.is_empty() {
        push_log(&*on_log, &logs_mem, "INFO", "Refreshing Microsoft session...".to_string());
        if let Ok(refreshed) = auth::refresh_minecraft_session(&client, &cfg.azure_client_id, &account.refresh_token).await {
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
    let mut check = fetch_or_fallback_manifest(&client, &cfg.manifest_url, &cfg.server_host, cfg.server_port, &instance_dir).await;

    if check.update_available {
        let ui_sync = ui_w.clone();
        let sync_res = synchronize_modpack(&client, &mut check.manifest, &instance_dir, cfg.keep_user_mods, move |p| {
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
        }).await;

        if let Err(e) = sync_res {
            let err_str = e.to_string();
            let _ = ui_w.upgrade_in_event_loop(move |ui| {
                ui.set_is_launching(false);
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
    }

    // Resolve / download Java runtime
    let ui_prog = ui_w.clone();
    let _ = ui_prog.upgrade_in_event_loop(|ui| {
        ui.set_progress(ProgressData {
            active: true,
            stage: "Verifying Java Runtime".into(),
            current_file: "JRE".into(),
            completed: 1,
            total: 2,
            percent: 50,
        });
    });

    let java_bin = match java::resolve_or_download_java(&client, &check.manifest.minecraft_version, &shared_dir, cfg.java_path.as_deref()).await {
        Ok(bin) => bin,
        Err(e) => {
            let err_str = e.to_string();
            let _ = ui_w.upgrade_in_event_loop(move |ui| {
                ui.set_is_launching(false);
                ui.set_status_banner_text(format!("Java error: {}", err_str).into());
            });
            return;
        }
    };

    discord_arc.update_status(cfg.discord_rpc_enabled, &format!("Playing {}", check.manifest.name), &format!("{} on {}", account.username, check.manifest.server_host));

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
                    ui.window().set_minimized(true);
                });
            }
            WindowAction::ShowAndFocusMain => {
                let _ = ui_win.upgrade_in_event_loop(|ui| {
                    ui.window().set_minimized(false);
                });
            }
            WindowAction::ExitLauncher => {
                std::process::exit(0);
            }
        }),
        Arc::new(move |exit_ev: loxy_core::launcher::GameExitEvent| {
            let _ = ui_exit.upgrade_in_event_loop(move |ui| {
                ui.set_is_launching(false);
                let mut p = ui.get_progress();
                p.active = false;
                ui.set_progress(p);
                if exit_ev.crashed {
                    let code = exit_ev.exit_code.to_string();
                    ui.set_status_banner_text(format!("Minecraft crashed with exit code {}.", code).into());
                    ui.set_console_open(true);
                } else {
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
            ui.set_status_banner_text(format!("Launch error: {}", err_str).into());
            ui.set_console_open(true);
        });
    }
}
