use anyhow::{anyhow, bail, Context, Result};
use futures_util::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha512};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ModLoaderType {
    Fabric,
    #[serde(alias = "neoforge")]
    NeoForge,
    Forge,
    Vanilla,
}

impl Default for ModLoaderType {
    fn default() -> Self {
        Self::Fabric
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestFileEntry {
    /// Relative path inside the instance directory, e.g. "mods/sodium.jar"
    pub path: String,
    /// Download URL(s) (primary + fallbacks)
    pub url: String,
    #[serde(default)]
    pub sha1: Option<String>,
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default)]
    pub sha512: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteManifest {
    pub name: String,
    pub version: String,
    pub minecraft_version: String,
    #[serde(default)]
    pub loader: ModLoaderType,
    pub loader_version: String,
    pub server_host: String,
    #[serde(default = "default_server_port")]
    pub server_port: u16,
    #[serde(default)]
    pub mrpack_url: Option<String>,
    #[serde(default)]
    pub mrpack_sha256: Option<String>,
    #[serde(default)]
    pub files: Vec<ManifestFileEntry>,
    #[serde(default)]
    pub news: Option<String>,
}

fn default_server_port() -> u16 {
    25565
}

impl Default for RemoteManifest {
    fn default() -> Self {
        Self {
            name: "Loxy Private Pack".to_string(),
            version: "1.0.0".to_string(),
            minecraft_version: "1.21.1".to_string(),
            loader: ModLoaderType::Fabric,
            loader_version: "0.16.9".to_string(),
            server_host: "play.loxy.gg".to_string(),
            server_port: 25565,
            mrpack_url: None,
            mrpack_sha256: None,
            files: Vec::new(),
            news: Some(
                "Welcome to LoxyLauncher! Configure your remote manifest URL in Settings to sync your private modpack from GitHub & Modrinth."
                    .to_string(),
            ),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModrinthIndex {
    #[serde(rename = "formatVersion")]
    pub format_version: u32,
    pub game: String,
    #[serde(rename = "versionId")]
    pub version_id: String,
    pub name: String,
    #[serde(default)]
    pub dependencies: HashMap<String, String>,
    #[serde(default)]
    pub files: Vec<ModrinthFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModrinthFile {
    pub path: String,
    pub hashes: ModrinthHashes,
    #[serde(default)]
    pub env: Option<ModrinthEnv>,
    pub downloads: Vec<String>,
    #[serde(rename = "fileSize", default)]
    pub file_size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModrinthHashes {
    #[serde(default)]
    pub sha1: Option<String>,
    #[serde(default)]
    pub sha512: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModrinthEnv {
    #[serde(default)]
    pub client: Option<String>,
    #[serde(default)]
    pub server: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestCheckResult {
    pub manifest: RemoteManifest,
    pub is_offline_fallback: bool,
    pub offline_warning: Option<String>,
    pub update_available: bool,
    pub installed_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncProgressEvent {
    pub stage: String,
    pub current_file: String,
    pub completed: usize,
    pub total: usize,
    pub percent: u8,
}

pub fn sanitize_relative_path(rel: &str) -> Result<PathBuf> {
    let normalized = rel.replace('\\', "/");
    let path = Path::new(&normalized);
    let mut clean = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::Normal(c) => clean.push(c),
            Component::CurDir => {}
            _ => bail!("Unsafe relative path in modpack: {}", rel),
        }
    }
    if clean.as_os_str().is_empty() {
        bail!("Empty relative path in modpack");
    }
    Ok(clean)
}

pub fn compute_file_hashes(path: &Path) -> Result<(String, String, String)> {
    let mut file = File::open(path)?;
    let mut sha1 = Sha1::new();
    let mut sha256 = Sha256::new();
    let mut sha512 = Sha512::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        sha1.update(&buf[..n]);
        sha256.update(&buf[..n]);
        sha512.update(&buf[..n]);
    }
    Ok((
        hex::encode(sha1.finalize()),
        hex::encode(sha256.finalize()),
        hex::encode(sha512.finalize()),
    ))
}

pub fn file_matches_hashes(
    path: &Path,
    expected_sha1: Option<&str>,
    expected_sha256: Option<&str>,
    expected_sha512: Option<&str>,
) -> bool {
    if !path.exists() {
        return false;
    }
    if expected_sha1.is_none() && expected_sha256.is_none() && expected_sha512.is_none() {
        return true;
    }
    let Ok((actual_sha1, actual_sha256, actual_sha512)) = compute_file_hashes(path) else {
        return false;
    };
    if let Some(exp1) = expected_sha1 {
        if !actual_sha1.eq_ignore_ascii_case(exp1.trim()) {
            return false;
        }
    }
    if let Some(exp256) = expected_sha256 {
        if !actual_sha256.eq_ignore_ascii_case(exp256.trim()) {
            return false;
        }
    }
    if let Some(exp512) = expected_sha512 {
        if !actual_sha512.eq_ignore_ascii_case(exp512.trim()) {
            return false;
        }
    }
    true
}

pub async fn fetch_or_fallback_manifest(
    client: &Client,
    manifest_url: &str,
    instance_dir: &Path,
) -> ManifestCheckResult {
    let cached_manifest_path = instance_dir.join("cached-manifest.json");
    let installed_version_path = instance_dir.join(".installed-version");
    let installed_version = fs::read_to_string(&installed_version_path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    match client.get(manifest_url).send().await {
        Ok(resp) if resp.status().is_success() => match resp.json::<RemoteManifest>().await {
            Ok(remote) => {
                let _ = fs::create_dir_all(instance_dir);
                if let Ok(json_str) = serde_json::to_string_pretty(&remote) {
                    let _ = fs::write(&cached_manifest_path, json_str);
                }
                let update_available = installed_version.as_deref() != Some(remote.version.as_str());
                ManifestCheckResult {
                    manifest: remote,
                    is_offline_fallback: false,
                    offline_warning: None,
                    update_available,
                    installed_version,
                }
            }
            Err(e) => load_cached_or_default(
                &cached_manifest_path,
                installed_version,
                format!("Remote manifest invalid JSON ({}). Using cached pack state.", e),
            ),
        },
        Ok(resp) => load_cached_or_default(
            &cached_manifest_path,
            installed_version,
            format!(
                "Remote manifest returned HTTP {}. Offline fallback active.",
                resp.status()
            ),
        ),
        Err(e) => load_cached_or_default(
            &cached_manifest_path,
            installed_version,
            format!("Could not reach manifest server ({}). Offline fallback active.", e),
        ),
    }
}

fn load_cached_or_default(
    cached_path: &Path,
    installed_version: Option<String>,
    reason: String,
) -> ManifestCheckResult {
    if let Ok(raw) = fs::read_to_string(cached_path) {
        if let Ok(manifest) = serde_json::from_str::<RemoteManifest>(&raw) {
            return ManifestCheckResult {
                manifest,
                is_offline_fallback: true,
                offline_warning: Some(reason),
                update_available: false,
                installed_version,
            };
        }
    }
    ManifestCheckResult {
        manifest: RemoteManifest::default(),
        is_offline_fallback: true,
        offline_warning: Some(reason),
        update_available: false,
        installed_version,
    }
}

pub async fn download_and_verify_file(
    client: &Client,
    url: &str,
    dest: &Path,
    expected_sha1: Option<&str>,
    expected_sha256: Option<&str>,
    expected_sha512: Option<&str>,
) -> Result<()> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp_dest = dest.with_extension("loxy.part");

    let resp = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("Failed to download {}", url))?;

    if !resp.status().is_success() {
        bail!("HTTP {} when downloading {}", resp.status(), url);
    }

    let mut file = File::create(&tmp_dest)?;
    let mut sha1 = Sha1::new();
    let mut sha256 = Sha256::new();
    let mut sha512 = Sha512::new();

    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let bytes = chunk.with_context(|| format!("Stream interrupted while downloading {}", url))?;
        sha1.update(&bytes);
        sha256.update(&bytes);
        sha512.update(&bytes);
        file.write_all(&bytes)?;
    }
    file.flush()?;
    drop(file);

    let actual_sha1 = hex::encode(sha1.finalize());
    let actual_sha256 = hex::encode(sha256.finalize());
    let actual_sha512 = hex::encode(sha512.finalize());

    if let Some(exp1) = expected_sha1 {
        if !actual_sha1.eq_ignore_ascii_case(exp1.trim()) {
            let _ = fs::remove_file(&tmp_dest);
            bail!(
                "SHA-1 mismatch for {}: expected {}, got {}",
                url,
                exp1,
                actual_sha1
            );
        }
    }
    if let Some(exp256) = expected_sha256 {
        if !actual_sha256.eq_ignore_ascii_case(exp256.trim()) {
            let _ = fs::remove_file(&tmp_dest);
            bail!(
                "SHA-256 mismatch for {}: expected {}, got {}",
                url,
                exp256,
                actual_sha256
            );
        }
    }
    if let Some(exp512) = expected_sha512 {
        if !actual_sha512.eq_ignore_ascii_case(exp512.trim()) {
            let _ = fs::remove_file(&tmp_dest);
            bail!(
                "SHA-512 mismatch for {}: expected {}, got {}",
                url,
                exp512,
                actual_sha512
            );
        }
    }

    fs::rename(&tmp_dest, dest)?;
    Ok(())
}

pub fn extract_mrpack_and_parse_index(
    mrpack_path: &Path,
    instance_dir: &Path,
) -> Result<ModrinthIndex> {
    let file = File::open(mrpack_path)
        .with_context(|| format!("Failed to open .mrpack at {:?}", mrpack_path))?;
    let mut archive = zip::ZipArchive::new(file)
        .with_context(|| "Failed to read .mrpack zip archive")?;

    let mut index_opt: Option<ModrinthIndex> = None;

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let raw_name = entry.name().replace('\\', "/");

        if raw_name == "modrinth.index.json" {
            let mut contents = String::new();
            entry.read_to_string(&mut contents)?;
            let parsed: ModrinthIndex = serde_json::from_str(&contents)
                .with_context(|| "Failed to parse modrinth.index.json inside .mrpack")?;
            index_opt = Some(parsed);
            continue;
        }

        let override_rel = raw_name
            .strip_prefix("overrides/")
            .or_else(|| raw_name.strip_prefix("client-overrides/"));

        if let Some(rel) = override_rel {
            if rel.is_empty() || entry.is_dir() {
                continue;
            }
            let safe_rel = sanitize_relative_path(rel)?;
            let out_path = instance_dir.join(safe_rel);
            if let Some(parent) = out_path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut out_file = File::create(&out_path)?;
            std::io::copy(&mut entry, &mut out_file)?;
        }
    }

    index_opt.ok_or_else(|| anyhow!("Missing modrinth.index.json in .mrpack archive"))
}

pub async fn synchronize_modpack<F>(
    client: &Client,
    manifest: &mut RemoteManifest,
    instance_dir: &Path,
    mut on_progress: F,
) -> Result<()>
where
    F: FnMut(SyncProgressEvent),
{
    fs::create_dir_all(instance_dir)?;
    let cache_dir = instance_dir.join(".cache");
    fs::create_dir_all(&cache_dir)?;

    let mut all_files: Vec<ManifestFileEntry> = manifest.files.clone();

    // 1. Download & unpack .mrpack if configured
    if let Some(mrpack_url) = manifest.mrpack_url.clone() {
        on_progress(SyncProgressEvent {
            stage: "Fetching .mrpack archive".to_string(),
            current_file: format!("{}-{}.mrpack", manifest.name, manifest.version),
            completed: 0,
            total: 1,
            percent: 5,
        });

        let mrpack_dest = cache_dir.join(format!("pack-{}.mrpack", manifest.version));
        let needs_mrpack_dl = !file_matches_hashes(
            &mrpack_dest,
            None,
            manifest.mrpack_sha256.as_deref(),
            None,
        ) || (manifest.mrpack_sha256.is_none() && !mrpack_dest.exists());

        if needs_mrpack_dl {
            download_and_verify_file(
                client,
                &mrpack_url,
                &mrpack_dest,
                None,
                manifest.mrpack_sha256.as_deref(),
                None,
            )
            .await?;
        }

        on_progress(SyncProgressEvent {
            stage: "Extracting .mrpack overrides".to_string(),
            current_file: "modrinth.index.json".to_string(),
            completed: 1,
            total: 1,
            percent: 15,
        });

        let mr_index = extract_mrpack_and_parse_index(&mrpack_dest, instance_dir)?;

        // Update Minecraft version & Mod Loader dynamically from modrinth.index.json dependencies if present
        if let Some(mc_ver) = mr_index.dependencies.get("minecraft") {
            manifest.minecraft_version = mc_ver.clone();
        }
        if let Some(fabric_ver) = mr_index.dependencies.get("fabric-loader") {
            manifest.loader = ModLoaderType::Fabric;
            manifest.loader_version = fabric_ver.clone();
        } else if let Some(neoforge_ver) = mr_index.dependencies.get("neoforge") {
            manifest.loader = ModLoaderType::NeoForge;
            manifest.loader_version = neoforge_ver.clone();
        } else if let Some(forge_ver) = mr_index.dependencies.get("forge") {
            manifest.loader = ModLoaderType::Forge;
            manifest.loader_version = forge_ver.clone();
        }

        for mf in mr_index.files {
            if let Some(env) = &mf.env {
                if env.client.as_deref() == Some("unsupported") {
                    continue;
                }
            }
            if let Some(primary_url) = mf.downloads.first() {
                all_files.push(ManifestFileEntry {
                    path: mf.path,
                    url: primary_url.clone(),
                    sha1: mf.hashes.sha1,
                    sha256: None,
                    sha512: mf.hashes.sha512,
                    size: Some(mf.file_size),
                });
            }
        }
    }

    // 2. Hash check all expected files and build differential download queue
    let mut expected_mod_filenames = HashSet::new();
    let mut download_queue = Vec::new();
    let total_entries = all_files.len();

    for (idx, entry) in all_files.iter().enumerate() {
        let safe_rel = sanitize_relative_path(&entry.path)?;
        if let Some(first_comp) = safe_rel.components().next() {
            if first_comp.as_os_str() == "mods" {
                if let Some(fname) = safe_rel.file_name() {
                    expected_mod_filenames.insert(fname.to_string_lossy().to_string());
                }
            }
        }

        let target_path = instance_dir.join(&safe_rel);
        on_progress(SyncProgressEvent {
            stage: "Verifying local file hashes".to_string(),
            current_file: entry.path.clone(),
            completed: idx + 1,
            total: total_entries.max(1),
            percent: 15 + (((idx + 1) * 20) / total_entries.max(1)) as u8,
        });

        let matches = file_matches_hashes(
            &target_path,
            entry.sha1.as_deref(),
            entry.sha256.as_deref(),
            entry.sha512.as_deref(),
        );

        if !matches {
            download_queue.push((safe_rel, entry.clone()));
        }
    }

    // 3. Prune obsolete mods in instance/mods/ if manifest manages mods
    if !expected_mod_filenames.is_empty() {
        let mods_dir = instance_dir.join("mods");
        if mods_dir.exists() {
            if let Ok(read_dir) = fs::read_dir(&mods_dir) {
                for item in read_dir.flatten() {
                    let path = item.path();
                    if path.is_file()
                        && path.extension().and_then(|e| e.to_str()) == Some("jar")
                    {
                        if let Some(fname) = path.file_name().map(|n| n.to_string_lossy().to_string()) {
                            if !expected_mod_filenames.contains(&fname) {
                                let _ = fs::remove_file(&path);
                            }
                        }
                    }
                }
            }
        }
    }

    // 4. Download missing or modified files with SHA verification
    let dl_total = download_queue.len();
    for (idx, (safe_rel, entry)) in download_queue.into_iter().enumerate() {
        let dest_path = instance_dir.join(&safe_rel);
        let pct = 35 + (((idx + 1) * 60) / dl_total.max(1)) as u8;
        on_progress(SyncProgressEvent {
            stage: "Downloading modpack files".to_string(),
            current_file: entry.path.clone(),
            completed: idx + 1,
            total: dl_total,
            percent: pct.min(95),
        });

        download_and_verify_file(
            client,
            &entry.url,
            &dest_path,
            entry.sha1.as_deref(),
            entry.sha256.as_deref(),
            entry.sha512.as_deref(),
        )
        .await?;
    }

    // 5. Record installed version marker
    fs::write(instance_dir.join(".installed-version"), &manifest.version)?;

    on_progress(SyncProgressEvent {
        stage: "Modpack synchronized".to_string(),
        current_file: manifest.name.clone(),
        completed: dl_total,
        total: dl_total,
        percent: 100,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_relative_path() {
        assert!(sanitize_relative_path("mods/sodium-fabric.jar").is_ok());
        assert!(sanitize_relative_path("../etc/passwd").is_err());
        assert!(sanitize_relative_path("/abs/path.jar").is_err());
    }
}
