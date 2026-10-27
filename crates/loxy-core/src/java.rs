use crate::modpack::download_and_verify_file;
use anyhow::{anyhow, bail, Context, Result};
use flate2::read::GzDecoder;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::Command;
use tar::Archive;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectedJava {
    pub path: String,
    pub major_version: u32,
    pub label: String,
}

/// Determines the required Java major version (8, 17, or 21) for a given Minecraft version string.
pub fn required_java_version_for_mc(mc_version: &str) -> u32 {
    let parts: Vec<u32> = mc_version
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse::<u32>().ok())
        .collect();

    match parts.as_slice() {
        [1, minor, patch, ..] => {
            if *minor >= 21 || (*minor == 20 && *patch >= 5) {
                21
            } else if *minor >= 17 {
                17
            } else {
                8
            }
        }
        [1, minor] => {
            if *minor >= 21 {
                21
            } else if *minor >= 17 {
                17
            } else {
                8
            }
        }
        [major, ..] if *major >= 21 => 21,
        _ => 21,
    }
}

/// Parses the major Java version from `java -version` stderr/stdout output or a JDK `release` file.
pub fn parse_java_version_output(output: &str) -> Option<u32> {
    for line in output.lines() {
        if let Some(first_quote) = line.find('"') {
            let rest = &line[first_quote + 1..];
            if let Some(second_quote) = rest.find('"') {
                let ver_str = &rest[..second_quote];
                let nums: Vec<u32> = ver_str
                    .split(|c: char| !c.is_ascii_digit())
                    .filter(|s| !s.is_empty())
                    .filter_map(|s| s.parse::<u32>().ok())
                    .collect();
                if let Some(&first) = nums.first() {
                    if first == 1 {
                        return nums.get(1).copied();
                    }
                    return Some(first);
                }
            }
        }
    }
    None
}

fn check_java_binary_version(java_bin: &Path) -> Option<u32> {
    // Fast path: check `../release` file if present in standard JDK layout
    if let Some(bin_dir) = java_bin.parent() {
        if let Some(jdk_root) = bin_dir.parent() {
            let release_file = jdk_root.join("release");
            if let Ok(contents) = fs::read_to_string(&release_file) {
                for line in contents.lines() {
                    if line.starts_with("JAVA_VERSION=") {
                        if let Some(ver) = parse_java_version_output(line) {
                            return Some(ver);
                        }
                    }
                }
            }
        }
    }

    let output = Command::new(java_bin).arg("-version").output().ok()?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_java_version_output(&stderr).or_else(|| parse_java_version_output(&stdout))
}

fn find_java_in_dir(dir: &Path) -> Option<PathBuf> {
    if !dir.exists() {
        return None;
    }
    let bin_names: &[&str] = if cfg!(target_os = "windows") {
        &["javaw.exe", "java.exe"]
    } else {
        &["java"]
    };

    for &bin_name in bin_names {
        let direct = dir.join("bin").join(bin_name);
        if direct.is_file() {
            return Some(direct);
        }
        let jre_direct = dir.join("jre").join("bin").join(bin_name);
        if jre_direct.is_file() {
            return Some(jre_direct);
        }
        let mac_direct = dir.join("Contents").join("Home").join("bin").join(bin_name);
        if mac_direct.is_file() {
            return Some(mac_direct);
        }
    }

    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                for &bin_name in bin_names {
                    let sub_bin = p.join("bin").join(bin_name);
                    if sub_bin.is_file() {
                        return Some(sub_bin);
                    }
                    let mac_bin = p.join("Contents").join("Home").join("bin").join(bin_name);
                    if mac_bin.is_file() {
                        return Some(mac_bin);
                    }
                }
            }
        }
    }
    None
}

/// Scans standard OS locations (`/usr/lib/jvm`, `~/.jdks`, `JAVA_HOME`, `Program Files\Java`, etc.)
/// for installed Java runtimes so we reuse system JDKs (Java 8, 17, 21+) without redownloading.
pub fn detect_system_javas() -> Vec<DetectedJava> {
    let mut candidate_dirs: Vec<PathBuf> = Vec::new();

    if let Ok(java_home) = std::env::var("JAVA_HOME") {
        candidate_dirs.push(PathBuf::from(java_home));
    }

    // Standard Linux JVM roots
    for root in [
        "/usr/lib/jvm",
        "/usr/lib64/jvm",
        "/usr/java",
        "/opt/java",
        "/opt/jdk",
    ] {
        let p = PathBuf::from(root);
        if let Ok(entries) = fs::read_dir(&p) {
            for entry in entries.flatten() {
                let ep = entry.path();
                // Skip symlink aliases like `default` or `default-runtime` if we already scan the real folders,
                // or canonicalize later to deduplicate.
                if ep.is_dir() {
                    candidate_dirs.push(ep);
                }
            }
        }
    }

    // Standard macOS & Homebrew JVM roots
    for root in ["/Library/Java/JavaVirtualMachines", "/opt/homebrew/opt"] {
        let p = PathBuf::from(root);
        if let Ok(entries) = fs::read_dir(&p) {
            for entry in entries.flatten() {
                candidate_dirs.push(entry.path());
            }
        }
    }

    // Standard Windows JVM roots
    if cfg!(target_os = "windows") {
        for root in [
            r"C:\Program Files\Eclipse Adoptium",
            r"C:\Program Files\Java",
            r"C:\Program Files\Microsoft",
            r"C:\Program Files\BellSoft",
            r"C:\Program Files\Zulu",
        ] {
            let p = PathBuf::from(root);
            if let Ok(entries) = fs::read_dir(&p) {
                for entry in entries.flatten() {
                    candidate_dirs.push(entry.path());
                }
            }
        }
    }

    // User home JDK directories (~/.jdks, ~/.sdkman/candidates/java)
    if let Some(home) = dirs::home_dir() {
        for sub in [".jdks", ".sdkman/candidates/java"] {
            let p = home.join(sub);
            if let Ok(entries) = fs::read_dir(&p) {
                for entry in entries.flatten() {
                    candidate_dirs.push(entry.path());
                }
            }
        }
    }

    let mut seen_canonical = HashSet::new();
    let mut detected = Vec::new();

    for dir in candidate_dirs {
        if let Some(bin_path) = find_java_in_dir(&dir) {
            let canonical = fs::canonicalize(&bin_path).unwrap_or_else(|_| bin_path.clone());
            if !seen_canonical.insert(canonical.clone()) {
                continue;
            }
            if let Some(major) = check_java_binary_version(&canonical) {
                let folder_name = dir
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| format!("Java {}", major));
                let path_str = canonical.to_string_lossy().to_string();
                detected.push(DetectedJava {
                    label: format!("Java {} ({})", major, folder_name),
                    path: path_str,
                    major_version: major,
                });
            }
        }
    }

    // Also check `java` in PATH if not already discovered
    let path_java = PathBuf::from(if cfg!(target_os = "windows") {
        "javaw.exe"
    } else {
        "java"
    });
    if let Some(major) = check_java_binary_version(&path_java) {
        if !detected.iter().any(|d| d.major_version == major) {
            detected.push(DetectedJava {
                label: format!("Java {} (System PATH)", major),
                path: path_java.to_string_lossy().to_string(),
                major_version: major,
            });
        }
    }

    detected.sort_by_key(|d| d.major_version);
    detected
}

pub async fn resolve_or_download_java(
    client: &Client,
    mc_version: &str,
    shared_dir: &Path,
    custom_java_path: Option<&str>,
) -> Result<PathBuf> {
    // 1. Honor explicit user-configured Java binary path if valid
    if let Some(custom) = custom_java_path {
        let trimmed = custom.trim();
        if !trimmed.is_empty() {
            let custom_path = PathBuf::from(trimmed);
            if custom_path.exists() {
                return Ok(custom_path);
            }
            bail!(
                "Custom Java path does not exist: {}. Clear it in Settings to use automatic Java detection.",
                trimmed
            );
        }
    }

    let required_major = required_java_version_for_mc(mc_version);

    // 2. Scan all installed system JDKs (/usr/lib/jvm, JAVA_HOME, ~/.jdks, PATH, etc.)
    //    First prefer an exact major match (e.g., java-17-openjdk for MC 1.20.1, java-21-openjdk for MC 1.21.1).
    let sys_javas = detect_system_javas();
    if let Some(exact) = sys_javas.iter().find(|j| j.major_version == required_major) {
        return Ok(PathBuf::from(&exact.path));
    }

    // 3. Check already-downloaded isolated runtime in shared_dir
    let runtime_dir = shared_dir
        .join("runtimes")
        .join(format!("java-{}", required_major));
    if let Some(existing_bin) = find_java_in_dir(&runtime_dir) {
        return Ok(existing_bin);
    }

    // 4. If a newer compatible system Java is installed (e.g. JDK 21 when JDK 17 is required),
    //    use it instead of downloading a separate runtime.
    if let Some(compatible) = sys_javas.iter().find(|j| j.major_version > required_major) {
        return Ok(PathBuf::from(&compatible.path));
    }

    // 5. Fallback: Download matching Eclipse Temurin (Adoptium) JRE for current OS & architecture
    fs::create_dir_all(&runtime_dir)?;

    let os_name = if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "mac"
    } else {
        "linux"
    };

    let arch_name = if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x64"
    };

    let ext = if cfg!(target_os = "windows") {
        "zip"
    } else {
        "tar.gz"
    };

    let adoptium_url = format!(
        "https://api.adoptium.net/v3/binary/latest/{}/ga/{}/{}/jre/hotspot/normal/eclipse",
        required_major, os_name, arch_name
    );

    let archive_path = runtime_dir.join(format!("temurin-{}.{}", required_major, ext));
    download_and_verify_file(client, &adoptium_url, &archive_path, None, None, None)
        .await
        .with_context(|| format!("Failed to download Java {} runtime from Adoptium", required_major))?;

    if ext == "zip" {
        let zip_file = File::open(&archive_path)?;
        let mut archive = zip::ZipArchive::new(zip_file)?;
        archive.extract(&runtime_dir)?;
    } else {
        let tar_gz = File::open(&archive_path)?;
        let tar = GzDecoder::new(tar_gz);
        let mut archive = Archive::new(tar);
        archive.unpack(&runtime_dir)?;
    }

    let _ = fs::remove_file(&archive_path);

    let java_bin = find_java_in_dir(&runtime_dir).ok_or_else(|| {
        anyhow!(
            "Downloaded Java {} archive but could not locate java binary inside {:?}",
            required_major,
            runtime_dir
        )
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = fs::metadata(&java_bin) {
            let mut perms = meta.permissions();
            perms.set_mode(0o755);
            let _ = fs::set_permissions(&java_bin, perms);
        }
    }

    Ok(java_bin)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_required_java_version_for_mc() {
        assert_eq!(required_java_version_for_mc("1.21.1"), 21);
        assert_eq!(required_java_version_for_mc("1.20.6"), 21);
        assert_eq!(required_java_version_for_mc("1.20.4"), 17);
        assert_eq!(required_java_version_for_mc("1.18.2"), 17);
        assert_eq!(required_java_version_for_mc("1.16.5"), 8);
        assert_eq!(required_java_version_for_mc("1.12.2"), 8);
    }

    #[test]
    fn test_parse_java_version_output() {
        let out21 = "openjdk version \"21.0.4\" 2024-07-16 LTS\nOpenJDK Runtime Environment Temurin-21.0.4+7 (build 21.0.4+7-LTS)";
        assert_eq!(parse_java_version_output(out21), Some(21));

        let out8 = "openjdk version \"1.8.0_412\"\nOpenJDK Runtime Environment (build 1.8.0_412-b08)";
        assert_eq!(parse_java_version_output(out8), Some(8));
    }

    #[test]
    fn test_detect_system_javas() {
        let javas = detect_system_javas();
        // On the current system /usr/lib/jvm has Java 8, 17, 21, 25, 27
        assert!(!javas.is_empty());
    }
}
