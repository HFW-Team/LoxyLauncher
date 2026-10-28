use anyhow::{anyhow, bail, Context, Result};
use futures_util::stream::{self, StreamExt};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha512};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// Number of concurrent HTTP downloads for modpack files and libraries.
pub const MODPACK_DOWNLOAD_CONCURRENCY: usize = 16;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ModLoaderType {
    Fabric,
    #[serde(alias = "neoforge")]
    NeoForge,
    Forge,
    Vanilla,
}

impl ModLoaderType {
    pub fn as_modrinth_category(&self) -> &'static str {
        match self {
            ModLoaderType::Fabric => "fabric",
            ModLoaderType::NeoForge => "neoforge",
            ModLoaderType::Forge => "forge",
            ModLoaderType::Vanilla => "datapack",
        }
    }
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
    /// Download URL(s)
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
                "Welcome to LoxyLauncher! In Settings, enter your Modrinth Modpack Slug/URL, direct .mrpack link, or custom JSON manifest URL."
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

#[derive(Debug, Deserialize)]
struct ModrinthApiProject {
    title: String,
    description: String,
}

#[derive(Debug, Deserialize)]
struct ModrinthApiVersion {
    id: String,
    version_number: String,
    name: String,
    changelog: Option<String>,
    date_published: Option<String>,
    game_versions: Vec<String>,
    loaders: Vec<String>,
    files: Vec<ModrinthApiVersionFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModrinthVersionItem {
    pub id: String,
    pub version_number: String,
    pub name: String,
    pub game_versions: Vec<String>,
    pub loaders: Vec<String>,
    pub date_published: String,
    pub filename: String,
}

#[derive(Debug, Deserialize)]
struct ModrinthApiVersionFile {
    url: String,
    filename: String,
    primary: Option<bool>,
    #[serde(default)]
    hashes: Option<ModrinthHashes>,
}

/// Official Modrinth v2 `/search` response item (`https://docs.modrinth.com/api/`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModrinthSearchHit {
    pub project_id: String,
    pub slug: String,
    pub title: String,
    pub author: String,
    pub description: String,
    pub icon_url: Option<String>,
    pub downloads: u64,
    #[serde(default)]
    pub follows: u64,
}

#[derive(Debug, Deserialize)]
struct ModrinthSearchResponse {
    hits: Vec<ModrinthSearchHit>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledModEntry {
    pub filename: String,
    pub enabled: bool,
    pub is_user_mod: bool,
    pub size_kb: u64,
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

pub fn parse_modrinth_slug(source: &str) -> Option<(String, Option<String>)> {
    let trimmed = source.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }

    if let Some(rest) = trimmed.strip_prefix("modrinth:") {
        let parts: Vec<&str> = rest.split('@').collect();
        let slug = parts[0].trim().to_string();
        let ver = parts.get(1).map(|v| v.trim().to_string());
        if !slug.is_empty() {
            return Some((slug, ver));
        }
    }

    for prefix in [
        "https://modrinth.com/modpack/",
        "https://www.modrinth.com/modpack/",
        "https://modrinth.com/project/",
        "https://www.modrinth.com/project/",
    ] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            let segments: Vec<&str> = rest.split('/').collect();
            let slug = segments.first()?.trim().to_string();
            let ver = if segments.len() >= 3 && segments[1] == "version" {
                Some(segments[2].trim().to_string())
            } else {
                None
            };
            if !slug.is_empty() {
                return Some((slug, ver));
            }
        }
    }

    if !trimmed.starts_with("http://")
        && !trimmed.starts_with("https://")
        && !trimmed.ends_with(".json")
    {
        let parts: Vec<&str> = trimmed.split('@').collect();
        let slug = parts[0].trim().to_string();
        let ver = parts.get(1).map(|v| v.trim().to_string());
        return Some((slug, ver));
    }

    None
}

async fn resolve_modrinth_project_manifest(
    client: &Client,
    slug: &str,
    pinned_version: Option<&str>,
    default_server_host: &str,
    default_server_port: u16,
) -> Result<RemoteManifest> {
    let proj_url = format!("https://api.modrinth.com/v2/project/{}", slug);
    let proj_resp = client
        .get(&proj_url)
        .send()
        .await
        .with_context(|| format!("Failed to reach Modrinth API for project '{}'", slug))?;

    if !proj_resp.status().is_success() {
        bail!(
            "Modrinth project '{}' not found (HTTP {})",
            slug,
            proj_resp.status()
        );
    }

    let project: ModrinthApiProject = proj_resp.json().await?;

    let ver_url = format!("https://api.modrinth.com/v2/project/{}/version", slug);
    let ver_resp = client.get(&ver_url).send().await?;
    if !ver_resp.status().is_success() {
        bail!(
            "Failed to query versions for Modrinth project '{}' (HTTP {})",
            slug,
            ver_resp.status()
        );
    }

    let versions: Vec<ModrinthApiVersion> = ver_resp.json().await?;
    let selected_ver = if let Some(target_v) = pinned_version {
        versions
            .into_iter()
            .find(|v| v.version_number == target_v || v.name == target_v)
            .ok_or_else(|| {
                anyhow!(
                    "Version '{}' not found for Modrinth project '{}'",
                    target_v,
                    slug
                )
            })?
    } else {
        versions
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("No published versions found for Modrinth project '{}'", slug))?
    };

    let mrpack_file = selected_ver
        .files
        .iter()
        .find(|f| f.filename.ends_with(".mrpack") && f.primary == Some(true))
        .or_else(|| {
            selected_ver
                .files
                .iter()
                .find(|f| f.filename.ends_with(".mrpack"))
        })
        .ok_or_else(|| {
            anyhow!(
                "Modrinth version '{}' does not contain a .mrpack file",
                selected_ver.version_number
            )
        })?;

    let mc_version = selected_ver
        .game_versions
        .first()
        .cloned()
        .unwrap_or_else(|| "1.21.1".to_string());

    let loader_str = selected_ver
        .loaders
        .first()
        .map(|s| s.to_lowercase())
        .unwrap_or_else(|| "fabric".to_string());

    let (loader, default_loader_ver) = match loader_str.as_str() {
        "neoforge" => (ModLoaderType::NeoForge, "latest".to_string()),
        "forge" => (ModLoaderType::Forge, "latest".to_string()),
        _ => (ModLoaderType::Fabric, "0.16.9".to_string()),
    };

    let news_text = if let Some(cl) = &selected_ver.changelog {
        if !cl.trim().is_empty() {
            format!(
                "{}\n\nLatest Release ({}):\n{}",
                project.description, selected_ver.version_number, cl
            )
        } else {
            project.description
        }
    } else {
        project.description
    };

    Ok(RemoteManifest {
        name: project.title,
        version: selected_ver.version_number,
        minecraft_version: mc_version,
        loader,
        loader_version: default_loader_ver,
        server_host: default_server_host.to_string(),
        server_port: default_server_port,
        mrpack_url: Some(mrpack_file.url.clone()),
        mrpack_sha256: None,
        files: Vec::new(),
        news: Some(news_text),
    })
}

/// Searches Modrinth's official v2 `/search` endpoint for mods with optional version, loader, and sort filters.
pub async fn search_modrinth_mods(
    client: &Client,
    query: &str,
    mc_version: Option<&str>,
    loader: Option<&str>,
    sort_index: Option<&str>,
) -> Result<Vec<ModrinthSearchHit>> {
    let mut facet_groups = vec!["[\"project_type:mod\"]".to_string()];

    if let Some(ver) = mc_version {
        let v = ver.trim();
        if !v.is_empty() && !v.eq_ignore_ascii_case("all") {
            facet_groups.push(format!("[\"versions:{}\"]", v));
        }
    }

    if let Some(ldr) = loader {
        let l = ldr.trim().to_lowercase();
        if !l.is_empty() && l != "all" && l != "any" && l != "vanilla" {
            facet_groups.push(format!("[\"categories:{}\"]", l));
        }
    }

    let facets = format!("[{}]", facet_groups.join(","));
    let index = sort_index.unwrap_or("relevance");

    let resp = client
        .get("https://api.modrinth.com/v2/search")
        .query(&[
            ("query", query),
            ("facets", &facets),
            ("limit", "25"),
            ("index", index),
        ])
        .send()
        .await
        .context("Failed to query Modrinth search API")?;

    if !resp.status().is_success() {
        bail!("Modrinth search returned HTTP {}", resp.status());
    }

    let parsed: ModrinthSearchResponse = resp
        .json()
        .await
        .context("Failed to parse Modrinth search results")?;

    Ok(parsed.hits)
}

/// Fetches available versions for a Modrinth mod so the user can pick a specific/older version from the dropdown.
pub async fn get_modrinth_mod_versions(
    client: &Client,
    project_id: &str,
    mc_version: Option<&str>,
    loader: Option<&str>,
) -> Result<Vec<ModrinthVersionItem>> {
    let ver_url = format!("https://api.modrinth.com/v2/project/{}/version", project_id);
    let resp = client
        .get(&ver_url)
        .send()
        .await
        .with_context(|| format!("Failed to fetch versions for Modrinth mod {}", project_id))?;

    if !resp.status().is_success() {
        bail!("Modrinth version lookup failed with HTTP {}", resp.status());
    }

    let versions: Vec<ModrinthApiVersion> = resp.json().await?;
    let ver_filter = mc_version
        .map(|s| s.trim())
        .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("all"));
    let loader_filter = loader
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty() && s != "all" && s != "any" && s != "vanilla");

    let mut items = Vec::new();
    for v in versions {
        if let Some(vf) = ver_filter {
            if !v.game_versions.iter().any(|g| g == vf) {
                continue;
            }
        }
        if let Some(ref lf) = loader_filter {
            if !v.loaders.iter().any(|l| l.eq_ignore_ascii_case(lf)) {
                continue;
            }
        }
        let Some(jar_file) = v
            .files
            .iter()
            .find(|f| f.filename.ends_with(".jar") && f.primary == Some(true))
            .or_else(|| v.files.iter().find(|f| f.filename.ends_with(".jar")))
        else {
            continue;
        };

        items.push(ModrinthVersionItem {
            id: v.id,
            version_number: v.version_number,
            name: v.name,
            game_versions: v.game_versions,
            loaders: v.loaders,
            date_published: v
                .date_published
                .unwrap_or_default()
                .chars()
                .take(10)
                .collect(),
            filename: jar_file.filename.clone(),
        });
    }

    Ok(items)
}

/// Installs a mod from Modrinth (`GET /v2/project/{id}/version`) directly into `<instance_dir>/mods/`.
/// If `version_id` is specified, installs that exact version; otherwise selects the latest matching version.
pub async fn install_modrinth_mod(
    client: &Client,
    project_id: &str,
    version_id: Option<&str>,
    mc_version: &str,
    loader: &str,
    instance_dir: &Path,
) -> Result<InstalledModEntry> {
    let loader_name = loader.trim().to_lowercase();
    let ver_url = format!("https://api.modrinth.com/v2/project/{}/version", project_id);

    let resp = client
        .get(&ver_url)
        .send()
        .await
        .with_context(|| format!("Failed to fetch versions for Modrinth mod {}", project_id))?;

    if !resp.status().is_success() {
        bail!(
            "Modrinth version lookup failed with HTTP {}",
            resp.status()
        );
    }

    let versions: Vec<ModrinthApiVersion> = resp.json().await?;

    let matched = if let Some(target_vid) = version_id.filter(|s| !s.trim().is_empty()) {
        versions
            .iter()
            .find(|v| v.id == target_vid || v.version_number == target_vid)
            .ok_or_else(|| anyhow!("Requested version {} not found on Modrinth", target_vid))?
    } else {
        let check_ver = !mc_version.trim().is_empty() && !mc_version.eq_ignore_ascii_case("all");
        let check_ldr =
            !loader_name.is_empty() && loader_name != "all" && loader_name != "any" && loader_name != "vanilla";

        versions
            .iter()
            .find(|v| {
                (!check_ver || v.game_versions.iter().any(|g| g == mc_version))
                    && (!check_ldr || v.loaders.iter().any(|l| l.eq_ignore_ascii_case(&loader_name)))
            })
            .or_else(|| {
                versions
                    .iter()
                    .find(|v| !check_ver || v.game_versions.iter().any(|g| g == mc_version))
            })
            .or_else(|| versions.first())
            .ok_or_else(|| {
                anyhow!(
                    "No compatible version found on Modrinth for Minecraft {} ({})",
                    mc_version,
                    loader_name
                )
            })?
    };

    let jar_file = matched
        .files
        .iter()
        .find(|f| f.filename.ends_with(".jar") && f.primary == Some(true))
        .or_else(|| matched.files.iter().find(|f| f.filename.ends_with(".jar")))
        .ok_or_else(|| anyhow!("No .jar file found in Modrinth release {}", matched.version_number))?;

    let mods_dir = instance_dir.join("mods");
    fs::create_dir_all(&mods_dir)?;

    let safe_fname = sanitize_relative_path(&jar_file.filename)?;
    let dest_path = mods_dir.join(&safe_fname);

    let sha1 = jar_file
        .hashes
        .as_ref()
        .and_then(|h| h.sha1.as_deref());
    let sha512 = jar_file
        .hashes
        .as_ref()
        .and_then(|h| h.sha512.as_deref());

    download_and_verify_file(client, &jar_file.url, &dest_path, sha1, None, sha512).await?;

    // Record in `.user-mods.json` so we know this mod was explicitly added by the user
    let user_mods_file = instance_dir.join(".user-mods.json");
    let mut user_mods: HashSet<String> = fs::read_to_string(&user_mods_file)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    user_mods.insert(jar_file.filename.clone());
    if let Ok(serialized) = serde_json::to_string_pretty(&user_mods) {
        let _ = fs::write(&user_mods_file, serialized);
    }

    let size_kb = fs::metadata(&dest_path)
        .map(|m| m.len() / 1024)
        .unwrap_or(0);

    Ok(InstalledModEntry {
        filename: jar_file.filename.clone(),
        enabled: true,
        is_user_mod: true,
        size_kb,
    })
}

pub fn list_installed_mods(instance_dir: &Path) -> Result<Vec<InstalledModEntry>> {
    let mods_dir = instance_dir.join("mods");
    if !mods_dir.exists() {
        return Ok(Vec::new());
    }

    let user_mods_file = instance_dir.join(".user-mods.json");
    let user_mods: HashSet<String> = fs::read_to_string(&user_mods_file)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();

    let manifest_mods_file = instance_dir.join(".manifest-mods.json");
    let manifest_mods: HashSet<String> = fs::read_to_string(&manifest_mods_file)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();

    let mut list = Vec::new();
    for entry in fs::read_dir(&mods_dir)?.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let fname = entry.file_name().to_string_lossy().to_string();
        if !fname.ends_with(".jar") && !fname.ends_with(".jar.disabled") {
            continue;
        }
        let enabled = fname.ends_with(".jar");
        let base_jar_name = fname.trim_end_matches(".disabled");
        let is_user_mod = user_mods.contains(base_jar_name) || !manifest_mods.contains(base_jar_name);
        let size_kb = entry.metadata().map(|m| m.len() / 1024).unwrap_or(0);

        list.push(InstalledModEntry {
            filename: fname,
            enabled,
            is_user_mod,
            size_kb,
        });
    }

    list.sort_by(|a, b| {
        b.is_user_mod
            .cmp(&a.is_user_mod)
            .then_with(|| a.filename.to_lowercase().cmp(&b.filename.to_lowercase()))
    });
    Ok(list)
}

pub fn toggle_or_delete_installed_mod(
    instance_dir: &Path,
    filename: &str,
    delete: bool,
) -> Result<()> {
    let safe_name = sanitize_relative_path(filename)?;
    let mods_dir = instance_dir.join("mods");
    let target = mods_dir.join(&safe_name);
    if !target.exists() {
        bail!("Mod file not found: {}", filename);
    }

    if delete {
        fs::remove_file(&target)?;
        let base_name = filename.trim_end_matches(".disabled");
        let user_mods_file = instance_dir.join(".user-mods.json");
        if let Ok(raw) = fs::read_to_string(&user_mods_file) {
            if let Ok(mut set) = serde_json::from_str::<HashSet<String>>(&raw) {
                set.remove(base_name);
                if let Ok(s) = serde_json::to_string_pretty(&set) {
                    let _ = fs::write(&user_mods_file, s);
                }
            }
        }
    } else if filename.ends_with(".jar.disabled") {
        let new_name = filename.trim_end_matches(".disabled");
        fs::rename(&target, mods_dir.join(new_name))?;
    } else if filename.ends_with(".jar") {
        let new_name = format!("{}.disabled", filename);
        fs::rename(&target, mods_dir.join(new_name))?;
    }

    Ok(())
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
    manifest_source: &str,
    default_server_host: &str,
    default_server_port: u16,
    instance_dir: &Path,
) -> ManifestCheckResult {
    let cached_manifest_path = instance_dir.join("cached-manifest.json");
    let installed_version_path = instance_dir.join(".installed-version");
    let installed_version = fs::read_to_string(&installed_version_path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let trimmed_source = manifest_source.trim();

    // Case 1: Modrinth Project Slug or Modrinth Web URL
    if let Some((slug, pinned_ver)) = parse_modrinth_slug(trimmed_source) {
        match resolve_modrinth_project_manifest(
            client,
            &slug,
            pinned_ver.as_deref(),
            default_server_host,
            default_server_port,
        )
        .await
        {
            Ok(remote) => {
                let _ = fs::create_dir_all(instance_dir);
                if let Ok(json_str) = serde_json::to_string_pretty(&remote) {
                    let _ = fs::write(&cached_manifest_path, json_str);
                }
                let update_available =
                    installed_version.as_deref() != Some(remote.version.as_str());
                return ManifestCheckResult {
                    manifest: remote,
                    is_offline_fallback: false,
                    offline_warning: None,
                    update_available,
                    installed_version,
                };
            }
            Err(e) => {
                return load_cached_or_default(
                    &cached_manifest_path,
                    default_server_host,
                    default_server_port,
                    installed_version,
                    format!("Modrinth lookup error ({}). Using cached pack state.", e),
                );
            }
        }
    }

    // Case 2: Direct .mrpack URL
    if trimmed_source.ends_with(".mrpack") {
        let filename = trimmed_source
            .rsplit('/')
            .next()
            .unwrap_or("modpack.mrpack")
            .trim_end_matches(".mrpack");
        let remote = RemoteManifest {
            name: filename.to_string(),
            version: filename.to_string(),
            minecraft_version: "1.21.1".to_string(),
            loader: ModLoaderType::Fabric,
            loader_version: "0.16.9".to_string(),
            server_host: default_server_host.to_string(),
            server_port: default_server_port,
            mrpack_url: Some(trimmed_source.to_string()),
            mrpack_sha256: None,
            files: Vec::new(),
            news: Some(format!("Direct .mrpack archive: {}", trimmed_source)),
        };
        let update_available = installed_version.as_deref() != Some(remote.version.as_str());
        return ManifestCheckResult {
            manifest: remote,
            is_offline_fallback: false,
            offline_warning: None,
            update_available,
            installed_version,
        };
    }

    // Case 3: Custom Remote JSON Manifest URL
    match client.get(trimmed_source).send().await {
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
                default_server_host,
                default_server_port,
                installed_version,
                format!("Remote manifest invalid JSON ({}). Using cached pack state.", e),
            ),
        },
        Ok(resp) => load_cached_or_default(
            &cached_manifest_path,
            default_server_host,
            default_server_port,
            installed_version,
            format!(
                "Remote manifest returned HTTP {}. Configure your Modrinth pack or manifest URL in Settings.",
                resp.status()
            ),
        ),
        Err(e) => load_cached_or_default(
            &cached_manifest_path,
            default_server_host,
            default_server_port,
            installed_version,
            format!("Could not reach manifest server ({}). Offline fallback active.", e),
        ),
    }
}

fn load_cached_or_default(
    cached_path: &Path,
    default_server_host: &str,
    default_server_port: u16,
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
    let mut def = RemoteManifest::default();
    def.server_host = default_server_host.to_string();
    def.server_port = default_server_port;
    ManifestCheckResult {
        manifest: def,
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
    let tmp_dest = dest.with_extension(format!("loxy.{}.part", uuid::Uuid::new_v4().simple()));

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
        let bytes =
            chunk.with_context(|| format!("Stream interrupted while downloading {}", url))?;
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
    keep_user_mods: bool,
    on_progress: F,
) -> Result<()>
where
    F: FnMut(SyncProgressEvent) + Send + 'static,
{
    fs::create_dir_all(instance_dir)?;
    let cache_dir = instance_dir.join(".cache");
    fs::create_dir_all(&cache_dir)?;

    let progress_cb = Arc::new(Mutex::new(on_progress));
    let emit_prog = |ev: SyncProgressEvent| {
        if let Ok(mut cb) = progress_cb.lock() {
            cb(ev);
        }
    };

    let mut all_files: Vec<ManifestFileEntry> = manifest.files.clone();

    // 1. Download & unpack .mrpack if configured
    if let Some(mrpack_url) = manifest.mrpack_url.clone() {
        emit_prog(SyncProgressEvent {
            stage: "Fetching .mrpack archive".to_string(),
            current_file: format!("{}-{}.mrpack", manifest.name, manifest.version),
            completed: 0,
            total: 1,
            percent: 5,
        });

        let safe_ver: String = manifest
            .version
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let mrpack_dest = cache_dir.join(format!("pack-{}.mrpack", safe_ver));
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

        emit_prog(SyncProgressEvent {
            stage: "Extracting .mrpack overrides".to_string(),
            current_file: "modrinth.index.json".to_string(),
            completed: 1,
            total: 1,
            percent: 15,
        });

        let mr_index = extract_mrpack_and_parse_index(&mrpack_dest, instance_dir)?;

        if !mr_index.name.is_empty() && manifest.name == "Loxy Private Pack" {
            manifest.name = mr_index.name;
        }
        if manifest.version.is_empty() && !mr_index.version_id.is_empty() {
            manifest.version = mr_index.version_id;
        }
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
        // Also skip downloading if user explicitly disabled this pack mod (.jar.disabled)
        let disabled_path = PathBuf::from(format!("{}.disabled", target_path.to_string_lossy()));
        if disabled_path.exists() {
            continue;
        }

        if idx % 10 == 0 || idx + 1 == total_entries {
            emit_prog(SyncProgressEvent {
                stage: "Verifying local file hashes".to_string(),
                current_file: entry.path.clone(),
                completed: idx + 1,
                total: total_entries.max(1),
                percent: 15 + (((idx + 1) * 15) / total_entries.max(1)) as u8,
            });
        }

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

    // 3. Handle mods folder pruning according to `keep_user_mods`
    let manifest_mods_file = instance_dir.join(".manifest-mods.json");
    let previous_manifest_mods: HashSet<String> = fs::read_to_string(&manifest_mods_file)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();

    if !expected_mod_filenames.is_empty() {
        let mods_dir = instance_dir.join("mods");
        if mods_dir.exists() {
            if let Ok(read_dir) = fs::read_dir(&mods_dir) {
                for item in read_dir.flatten() {
                    let path = item.path();
                    if path.is_file()
                        && path.extension().and_then(|e| e.to_str()) == Some("jar")
                    {
                        if let Some(fname) =
                            path.file_name().map(|n| n.to_string_lossy().to_string())
                        {
                            if !expected_mod_filenames.contains(&fname) {
                                if !keep_user_mods {
                                    // Strict mode: remove any mod not in manifest
                                    let _ = fs::remove_file(&path);
                                } else if previous_manifest_mods.contains(&fname) {
                                    // Keep user mods mode: only remove mods that belonged to an older version
                                    // of the modpack manifest and were removed/updated in the new manifest!
                                    let _ = fs::remove_file(&path);
                                }
                            }
                        }
                    }
                }
            }
        }
        if let Ok(serialized) = serde_json::to_string_pretty(&expected_mod_filenames) {
            let _ = fs::write(&manifest_mods_file, serialized);
        }
    }

    // 4. Concurrent Async Downloads (up to 16 files in parallel!)
    let dl_total = download_queue.len();
    if dl_total > 0 {
        let completed_counter = Arc::new(AtomicUsize::new(0));
        let prog_arc = progress_cb.clone();
        let instance_arc = Arc::new(instance_dir.to_path_buf());

        let results: Vec<Result<()>> = stream::iter(download_queue.into_iter())
            .map(|(safe_rel, entry)| {
                let client = client.clone();
                let instance_dir = instance_arc.clone();
                let counter = completed_counter.clone();
                let prog = prog_arc.clone();

                async move {
                    let dest_path = instance_dir.join(&safe_rel);
                    download_and_verify_file(
                        &client,
                        &entry.url,
                        &dest_path,
                        entry.sha1.as_deref(),
                        entry.sha256.as_deref(),
                        entry.sha512.as_deref(),
                    )
                    .await?;

                    let done = counter.fetch_add(1, Ordering::SeqCst) + 1;
                    let pct = 30 + ((done * 65) / dl_total.max(1)) as u8;
                    if let Ok(mut cb) = prog.lock() {
                        cb(SyncProgressEvent {
                            stage: format!(
                                "Downloading modpack files ({}/{})",
                                done, dl_total
                            ),
                            current_file: entry.path,
                            completed: done,
                            total: dl_total,
                            percent: pct.min(95),
                        });
                    }
                    Ok(())
                }
            })
            .buffer_unordered(MODPACK_DOWNLOAD_CONCURRENCY)
            .collect()
            .await;

        for res in results {
            res?;
        }
    }

    // 5. Save updated manifest cache & record installed version marker
    if let Ok(json_str) = serde_json::to_string_pretty(&manifest) {
        let _ = fs::write(instance_dir.join("cached-manifest.json"), json_str);
    }
    fs::write(instance_dir.join(".installed-version"), &manifest.version)?;

    emit_prog(SyncProgressEvent {
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

    #[test]
    fn test_parse_modrinth_slug() {
        assert_eq!(
            parse_modrinth_slug("fabulously-optimized"),
            Some(("fabulously-optimized".to_string(), None))
        );
        assert_eq!(
            parse_modrinth_slug("https://modrinth.com/modpack/fabulously-optimized"),
            Some(("fabulously-optimized".to_string(), None))
        );
        assert_eq!(
            parse_modrinth_slug("https://modrinth.com/modpack/fabulously-optimized/version/6.4.0"),
            Some(("fabulously-optimized".to_string(), Some("6.4.0".to_string())))
        );
        assert_eq!(
            parse_modrinth_slug("https://example.com/launcher-manifest.json"),
            None
        );
    }
}
