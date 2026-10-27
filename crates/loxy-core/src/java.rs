use crate::modpack::download_and_verify_file;
use anyhow::{anyhow, bail, Context, Result};
use flate2::read::GzDecoder;
use reqwest::Client;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::Command;
use tar::Archive;

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
        // New year-based versioning or unknown modern versions default to 21
        [major, ..] if *major >= 21 => 21,
        _ => 21,
    }
}

/// Parses the major Java version from `java -version` stderr/stdout output.
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
    let output = Command::new(java_bin).arg("-version").output().ok()?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_java_version_output(&stderr).or_else(|| parse_java_version_output(&stdout))
}

fn find_java_in_dir(dir: &Path) -> Option<PathBuf> {
    if !dir.exists() {
        return None;
    }
    let bin_name = if cfg!(target_os = "windows") {
        "javaw.exe"
    } else {
        "java"
    };

    let direct = dir.join("bin").join(bin_name);
    if direct.exists() {
        return Some(direct);
    }

    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                let sub_bin = p.join("bin").join(bin_name);
                if sub_bin.exists() {
                    return Some(sub_bin);
                }
                // macOS Contents/Home/bin/java layout
                let mac_bin = p.join("Contents").join("Home").join("bin").join(bin_name);
                if mac_bin.exists() {
                    return Some(mac_bin);
                }
            }
        }
    }
    None
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
    let runtime_dir = shared_dir
        .join("runtimes")
        .join(format!("java-{}", required_major));

    // 2. Check already-downloaded isolated runtime
    if let Some(existing_bin) = find_java_in_dir(&runtime_dir) {
        return Ok(existing_bin);
    }

    // 3. Check system `java` in PATH if its major version matches `required_major`
    let sys_java = PathBuf::from(if cfg!(target_os = "windows") {
        "javaw.exe"
    } else {
        "java"
    });
    if let Some(sys_ver) = check_java_binary_version(&sys_java) {
        if sys_ver == required_major {
            return Ok(sys_java);
        }
    }

    // 4. Download matching Eclipse Temurin (Adoptium) JRE for current OS & architecture
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
}
