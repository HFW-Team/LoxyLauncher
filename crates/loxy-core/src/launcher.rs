use crate::config::{CloseBehavior, LauncherConfig, MinecraftAccount};
use crate::modpack::{
    download_and_verify_file, file_matches_hashes, ModLoaderType, RemoteManifest, SyncProgressEvent,
};
use anyhow::{anyhow, bail, Context, Result};
use chrono::Local;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command as TokioCommand;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameLogEntry {
    pub timestamp: String,
    pub level: String,
    pub line: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameExitEvent {
    pub exit_code: i32,
    pub crashed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowAction {
    HideMain,
    ShowAndFocusMain,
    ExitLauncher,
}

#[derive(Debug, Deserialize)]
struct MojangVersionManifest {
    versions: Vec<MojangVersionSummary>,
}

#[derive(Debug, Deserialize)]
struct MojangVersionSummary {
    id: String,
    url: String,
    sha1: Option<String>,
}

/// Converts a Maven coordinate `group:artifact:version[:classifier]` into a relative path.
pub fn maven_coord_to_path(coord: &str) -> Result<String> {
    let parts: Vec<&str> = coord.split(':').collect();
    if parts.len() < 3 {
        bail!("Invalid Maven coordinate: {}", coord);
    }
    let group_path = parts[0].replace('.', "/");
    let artifact = parts[1];
    let version = parts[2];
    let file_name = if parts.len() >= 4 {
        format!("{}-{}-{}.jar", artifact, version, parts[3])
    } else {
        format!("{}-{}.jar", artifact, version)
    };
    Ok(format!("{}/{}/{}/{}", group_path, artifact, version, file_name))
}

fn current_mojang_os_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "osx"
    } else {
        "linux"
    }
}

pub fn rules_allow_current_os(rules: Option<&Vec<Value>>) -> bool {
    let Some(rule_list) = rules else {
        return true;
    };
    if rule_list.is_empty() {
        return true;
    }

    let target_os = current_mojang_os_name();
    let mut allowed = false;

    for rule in rule_list {
        let action = rule
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("allow");

        let os_matches = if let Some(os_obj) = rule.get("os") {
            if let Some(os_name) = os_obj.get("name").and_then(|v| v.as_str()) {
                os_name == target_os
            } else {
                true
            }
        } else {
            true
        };

        if os_matches {
            allowed = action == "allow";
        }
    }

    allowed
}

fn classify_log_level(line: &str) -> &'static str {
    if line.contains("/ERROR]")
        || line.contains("Exception")
        || line.contains("STDERR]")
        || line.contains("FATAL")
    {
        "ERROR"
    } else if line.contains("/WARN]") || line.contains("WARNING") {
        "WARN"
    } else {
        "INFO"
    }
}

pub fn push_log<F>(
    on_log: &F,
    log_buffer: &Arc<Mutex<Vec<GameLogEntry>>>,
    level: &str,
    line: String,
) where
    F: Fn(GameLogEntry) + ?Sized,
{
    let entry = GameLogEntry {
        timestamp: Local::now().format("%H:%M:%S").to_string(),
        level: level.to_string(),
        line,
    };
    if let Ok(mut guard) = log_buffer.lock() {
        if guard.len() >= 2500 {
            guard.drain(0..500);
        }
        guard.push(entry.clone());
    }
    on_log(entry);
}

#[allow(clippy::too_many_arguments)]
pub async fn prepare_and_launch_minecraft<FProg, FLog, FWin, FExit>(
    client: &Client,
    config: &LauncherConfig,
    account: &MinecraftAccount,
    manifest: &RemoteManifest,
    java_bin: &Path,
    log_buffer: Arc<Mutex<Vec<GameLogEntry>>>,
    mut on_progress: FProg,
    on_log: Arc<FLog>,
    on_window_action: Arc<FWin>,
    on_exit: Arc<FExit>,
) -> Result<()>
where
    FProg: FnMut(SyncProgressEvent),
    FLog: Fn(GameLogEntry) + Send + Sync + 'static,
    FWin: Fn(WindowAction) + Send + Sync + 'static,
    FExit: Fn(GameExitEvent) + Send + Sync + 'static,
{
    let instance_dir = PathBuf::from(&config.instance_dir);
    let shared_dir = PathBuf::from(&config.shared_dir);
    let versions_dir = shared_dir.join("versions");
    let libraries_dir = shared_dir.join("libraries");
    let assets_dir = shared_dir.join("assets");
    let natives_dir = instance_dir.join("natives");

    fs::create_dir_all(&versions_dir)?;
    fs::create_dir_all(&libraries_dir)?;
    fs::create_dir_all(&assets_dir)?;
    fs::create_dir_all(&natives_dir)?;

    let mc_ver = &manifest.minecraft_version;

    on_progress(SyncProgressEvent {
        stage: "Resolving Minecraft version metadata".to_string(),
        current_file: mc_ver.clone(),
        completed: 0,
        total: 4,
        percent: 10,
    });

    // 1. Fetch or load Mojang version JSON
    let mc_ver_dir = versions_dir.join(mc_ver);
    fs::create_dir_all(&mc_ver_dir)?;
    let mc_json_path = mc_ver_dir.join(format!("{}.json", mc_ver));

    if !mc_json_path.exists() {
        let vm_resp = client
            .get("https://piston-meta.mojang.com/mc/game/version_manifest_v2.json")
            .send()
            .await
            .context("Failed to fetch Mojang version_manifest_v2.json")?;
        let vm: MojangVersionManifest = vm_resp.json().await?;
        let entry = vm
            .versions
            .into_iter()
            .find(|v| v.id == *mc_ver)
            .ok_or_else(|| anyhow!("Minecraft version {} not found in Mojang manifest", mc_ver))?;

        download_and_verify_file(
            client,
            &entry.url,
            &mc_json_path,
            entry.sha1.as_deref(),
            None,
            None,
        )
        .await?;
    }

    let mc_json_raw = fs::read_to_string(&mc_json_path)?;
    let mc_json: Value = serde_json::from_str(&mc_json_raw)?;

    // 2. Download vanilla client JAR
    let client_jar_path = mc_ver_dir.join(format!("{}.jar", mc_ver));
    if let Some(client_dl) = mc_json.get("downloads").and_then(|d| d.get("client")) {
        let url = client_dl
            .get("url")
            .and_then(|u| u.as_str())
            .ok_or_else(|| anyhow!("Missing client jar download URL"))?;
        let sha1 = client_dl.get("sha1").and_then(|s| s.as_str());

        if !file_matches_hashes(&client_jar_path, sha1, None, None) {
            on_progress(SyncProgressEvent {
                stage: "Downloading Minecraft client JAR".to_string(),
                current_file: format!("{}.jar", mc_ver),
                completed: 1,
                total: 4,
                percent: 25,
            });
            download_and_verify_file(client, url, &client_jar_path, sha1, None, None).await?;
        }
    }

    // 3. Resolve & download Vanilla libraries
    let mut classpath_entries: Vec<PathBuf> = Vec::new();
    let mut seen_libs = HashSet::new();

    if let Some(libs) = mc_json.get("libraries").and_then(|l| l.as_array()) {
        let total_libs = libs.len().max(1);
        for (idx, lib) in libs.iter().enumerate() {
            let rules = lib.get("rules").and_then(|r| r.as_array());
            if !rules_allow_current_os(rules) {
                continue;
            }

            if let Some(artifact) = lib.get("downloads").and_then(|d| d.get("artifact")) {
                if let (Some(rel_path), Some(url)) = (
                    artifact.get("path").and_then(|p| p.as_str()),
                    artifact.get("url").and_then(|u| u.as_str()),
                ) {
                    let sha1 = artifact.get("sha1").and_then(|s| s.as_str());
                    let dest = libraries_dir.join(rel_path);
                    if !file_matches_hashes(&dest, sha1, None, None) {
                        on_progress(SyncProgressEvent {
                            stage: "Downloading Minecraft libraries".to_string(),
                            current_file: rel_path.to_string(),
                            completed: idx + 1,
                            total: total_libs,
                            percent: 25 + (((idx + 1) * 30) / total_libs) as u8,
                        });
                        download_and_verify_file(client, url, &dest, sha1, None, None).await?;
                    }
                    if seen_libs.insert(dest.clone()) {
                        classpath_entries.push(dest);
                    }
                }
            }
        }
    }

    // 4. Download Asset Index & Required Assets
    let asset_index_id = mc_json
        .get("assetIndex")
        .and_then(|a| a.get("id"))
        .and_then(|i| i.as_str())
        .unwrap_or(mc_ver)
        .to_string();

    if let Some(asset_idx_obj) = mc_json.get("assetIndex") {
        if let Some(url) = asset_idx_obj.get("url").and_then(|u| u.as_str()) {
            let sha1 = asset_idx_obj.get("sha1").and_then(|s| s.as_str());
            let idx_dest = assets_dir
                .join("indexes")
                .join(format!("{}.json", asset_index_id));
            if !file_matches_hashes(&idx_dest, sha1, None, None) {
                download_and_verify_file(client, url, &idx_dest, sha1, None, None).await?;
            }

            // Verify / download missing asset objects
            if let Ok(idx_raw) = fs::read_to_string(&idx_dest) {
                if let Ok(idx_json) = serde_json::from_str::<Value>(&idx_raw) {
                    if let Some(objects) = idx_json.get("objects").and_then(|o| o.as_object()) {
                        let total_objs = objects.len().max(1);
                        for (i, (_name, obj_val)) in objects.iter().enumerate() {
                            if let Some(hash) = obj_val.get("hash").and_then(|h| h.as_str()) {
                                if hash.len() >= 2 {
                                    let prefix = &hash[0..2];
                                    let obj_path =
                                        assets_dir.join("objects").join(prefix).join(hash);
                                    if !obj_path.exists() {
                                        let obj_url = format!(
                                            "https://resources.download.minecraft.net/{}/{}",
                                            prefix, hash
                                        );
                                        if i % 25 == 0 {
                                            on_progress(SyncProgressEvent {
                                                stage: "Downloading game assets".to_string(),
                                                current_file: format!("{}/{}", prefix, hash),
                                                completed: i + 1,
                                                total: total_objs,
                                                percent: 55 + (((i + 1) * 25) / total_objs) as u8,
                                            });
                                        }
                                        let _ = download_and_verify_file(
                                            client,
                                            &obj_url,
                                            &obj_path,
                                            Some(hash),
                                            None,
                                            None,
                                        )
                                        .await;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // 5. Resolve Mod Loader (Fabric, NeoForge, Forge, Vanilla)
    let mut main_class = mc_json
        .get("mainClass")
        .and_then(|m| m.as_str())
        .unwrap_or("net.minecraft.client.main.Main")
        .to_string();

    let mut extra_jvm_args: Vec<String> = Vec::new();
    let mut extra_game_args: Vec<String> = Vec::new();

    match manifest.loader {
        ModLoaderType::Fabric => {
            on_progress(SyncProgressEvent {
                stage: "Resolving Fabric Loader".to_string(),
                current_file: format!("fabric-loader-{}", manifest.loader_version),
                completed: 3,
                total: 4,
                percent: 85,
            });

            let fabric_profile_path = versions_dir.join(format!(
                "fabric-{}-{}.json",
                mc_ver, manifest.loader_version
            ));

            if !fabric_profile_path.exists() {
                let fabric_url = format!(
                    "https://meta.fabricmc.net/v2/versions/loader/{}/{}/profile/json",
                    mc_ver, manifest.loader_version
                );
                download_and_verify_file(client, &fabric_url, &fabric_profile_path, None, None, None)
                    .await
                    .context("Failed to fetch Fabric Loader profile metadata")?;
            }

            let fabric_raw = fs::read_to_string(&fabric_profile_path)?;
            let fabric_json: Value = serde_json::from_str(&fabric_raw)?;

            if let Some(mc) = fabric_json.get("mainClass").and_then(|m| m.as_str()) {
                main_class = mc.to_string();
            }

            if let Some(libs) = fabric_json.get("libraries").and_then(|l| l.as_array()) {
                for lib in libs {
                    if let Some(name) = lib.get("name").and_then(|n| n.as_str()) {
                        let base_url = lib
                            .get("url")
                            .and_then(|u| u.as_str())
                            .unwrap_or("https://maven.fabricmc.net/");
                        let rel_path = maven_coord_to_path(name)?;
                        let full_url = format!(
                            "{}/{}",
                            base_url.trim_end_matches('/'),
                            rel_path
                        );
                        let dest = libraries_dir.join(&rel_path);
                        let sha1 = lib.get("sha1").and_then(|s| s.as_str());
                        if !file_matches_hashes(&dest, sha1, None, None) {
                            download_and_verify_file(client, &full_url, &dest, sha1, None, None)
                                .await?;
                        }
                        if seen_libs.insert(dest.clone()) {
                            classpath_entries.insert(0, dest);
                        }
                    }
                }
            }
        }
        ModLoaderType::NeoForge | ModLoaderType::Forge => {
            on_progress(SyncProgressEvent {
                stage: format!("Preparing {:?} Loader", manifest.loader),
                current_file: manifest.loader_version.clone(),
                completed: 3,
                total: 4,
                percent: 85,
            });

            let custom_loader_json = instance_dir.join("loader-profile.json");
            if custom_loader_json.exists() {
                let raw = fs::read_to_string(&custom_loader_json)?;
                let ljson: Value = serde_json::from_str(&raw)?;
                if let Some(mc) = ljson.get("mainClass").and_then(|m| m.as_str()) {
                    main_class = mc.to_string();
                }
                if let Some(args) = ljson.get("arguments") {
                    if let Some(jvm) = args.get("jvm").and_then(|j| j.as_array()) {
                        for v in jvm.iter().filter_map(|x| x.as_str()) {
                            extra_jvm_args.push(v.to_string());
                        }
                    }
                    if let Some(game) = args.get("game").and_then(|g| g.as_array()) {
                        for v in game.iter().filter_map(|x| x.as_str()) {
                            extra_game_args.push(v.to_string());
                        }
                    }
                }
            }
        }
        ModLoaderType::Vanilla => {}
    }

    classpath_entries.push(client_jar_path);

    let cp_sep = if cfg!(target_os = "windows") { ";" } else { ":" };
    let classpath_str = classpath_entries
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join(cp_sep);

    // 6. Construct JVM & Game Arguments
    let mut cmd_args: Vec<String> = Vec::new();
    cmd_args.push(format!("-Xms{}M", config.min_ram_mb));
    cmd_args.push(format!("-Xmx{}M", config.max_ram_mb));

    for flag in config.jvm_args.split_whitespace() {
        if !flag.is_empty() {
            cmd_args.push(flag.to_string());
        }
    }

    #[cfg(target_os = "macos")]
    cmd_args.push("-XstartOnFirstThread".to_string());

    cmd_args.push(format!(
        "-Djava.library.path={}",
        natives_dir.to_string_lossy()
    ));
    cmd_args.push("-Dminecraft.launcher.brand=LoxyLauncher".to_string());
    cmd_args.push("-Dminecraft.launcher.version=0.1.0".to_string());

    for arg in extra_jvm_args {
        let interpolated = arg
            .replace("${library_directory}", &libraries_dir.to_string_lossy())
            .replace("${classpath_separator}", cp_sep)
            .replace("${version_name}", mc_ver);
        cmd_args.push(interpolated);
    }

    cmd_args.push("-cp".to_string());
    cmd_args.push(classpath_str);
    cmd_args.push(main_class.clone());

    cmd_args.extend([
        "--username".to_string(),
        account.username.clone(),
        "--version".to_string(),
        manifest.name.clone(),
        "--gameDir".to_string(),
        instance_dir.to_string_lossy().to_string(),
        "--assetsDir".to_string(),
        assets_dir.to_string_lossy().to_string(),
        "--assetIndex".to_string(),
        asset_index_id,
        "--uuid".to_string(),
        account.uuid.clone(),
        "--accessToken".to_string(),
        account.access_token.clone(),
        "--userType".to_string(),
        "msa".to_string(),
        "--versionType".to_string(),
        "LoxyLauncher".to_string(),
        "--width".to_string(),
        config.resolution_width.to_string(),
        "--height".to_string(),
        config.resolution_height.to_string(),
    ]);

    if config.fullscreen {
        cmd_args.push("--fullscreen".to_string());
    }

    cmd_args.extend(extra_game_args);

    on_progress(SyncProgressEvent {
        stage: "Starting Minecraft process".to_string(),
        current_file: main_class,
        completed: 4,
        total: 4,
        percent: 100,
    });

    push_log(
        &*on_log,
        &log_buffer,
        "INFO",
        format!(
            "Launching {} (MC {} / {:?} {}) with {} MB RAM...",
            manifest.name, mc_ver, manifest.loader, manifest.loader_version, config.max_ram_mb
        ),
    );

    let mut child = TokioCommand::new(java_bin)
        .args(&cmd_args)
        .current_dir(&instance_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("Failed to spawn Java process using {:?}", java_bin))?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let close_behavior = config.close_behavior.clone();
    if close_behavior == CloseBehavior::HideAndReopen {
        on_window_action(WindowAction::HideMain);
    } else if close_behavior == CloseBehavior::CloseLauncher {
        on_window_action(WindowAction::ExitLauncher);
        return Ok(());
    }

    let log_out = on_log.clone();
    let buf_out = log_buffer.clone();
    if let Some(out) = stdout {
        tokio::spawn(async move {
            let mut reader = BufReader::new(out).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                let lvl = classify_log_level(&line);
                push_log(&*log_out, &buf_out, lvl, line);
            }
        });
    }

    let log_err = on_log.clone();
    let buf_err = log_buffer.clone();
    if let Some(err) = stderr {
        tokio::spawn(async move {
            let mut reader = BufReader::new(err).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                push_log(&*log_err, &buf_err, "ERROR", line);
            }
        });
    }

    let log_wait = on_log.clone();
    let buf_wait = log_buffer.clone();
    let win_wait = on_window_action.clone();
    let exit_wait = on_exit.clone();
    tokio::spawn(async move {
        let status = child.wait().await;
        let code = status.ok().and_then(|s| s.code()).unwrap_or(-1);
        let crashed = code != 0;

        push_log(
            &*log_wait,
            &buf_wait,
            if crashed { "ERROR" } else { "INFO" },
            format!("Minecraft process exited with code {}", code),
        );

        if close_behavior == CloseBehavior::HideAndReopen {
            win_wait(WindowAction::ShowAndFocusMain);
        }

        exit_wait(GameExitEvent {
            exit_code: code,
            crashed,
        });
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_maven_coord_to_path() {
        assert_eq!(
            maven_coord_to_path("net.fabricmc:fabric-loader:0.16.9").unwrap(),
            "net/fabricmc/fabric-loader/0.16.9/fabric-loader-0.16.9.jar"
        );
        assert_eq!(
            maven_coord_to_path("org.lwjgl:lwjgl:3.3.3:natives-linux").unwrap(),
            "org/lwjgl/lwjgl/3.3.3/lwjgl-3.3.3-natives-linux.jar"
        );
    }

    #[test]
    fn test_rules_allow_current_os() {
        assert!(rules_allow_current_os(None));
        let rules = vec![json!({"action": "allow"})];
        assert!(rules_allow_current_os(Some(&rules)));
    }
}
