export type CloseBehavior = "hide_and_reopen" | "keep_open" | "close_launcher";

export type ModLoaderType = "fabric" | "neoforge" | "forge" | "vanilla";

export interface MinecraftAccount {
  uuid: string;
  username: string;
  access_token: string;
  refresh_token: string;
  expires_at: number;
  skin_url: string | null;
}

export interface LauncherConfig {
  instance_dir: string;
  shared_dir: string;
  min_ram_mb: number;
  max_ram_mb: number;
  jvm_args: string;
  java_path: string | null;
  resolution_width: number;
  resolution_height: number;
  fullscreen: boolean;
  close_behavior: CloseBehavior;
  discord_rpc_enabled: boolean;
  keep_user_mods: boolean;
  manifest_url: string;
  server_host: string;
  server_port: number;
  azure_client_id: string;
  account: MinecraftAccount | null;
}

export interface SystemMemoryInfo {
  total_ram_mb: number;
  recommended_max_mb: number;
}

export interface DetectedJava {
  path: string;
  major_version: number;
  label: string;
}

export interface InitialStateResponse {
  config: LauncherConfig;
  system_memory: SystemMemoryInfo;
  detected_javas: DetectedJava[];
}

export interface ManifestFileEntry {
  path: string;
  url: string;
  sha1?: string | null;
  sha256?: string | null;
  sha512?: string | null;
  size?: number | null;
}

export interface RemoteManifest {
  name: string;
  version: string;
  minecraft_version: string;
  loader: ModLoaderType;
  loader_version: string;
  server_host: string;
  server_port: number;
  mrpack_url: string | null;
  mrpack_sha256: string | null;
  files: ManifestFileEntry[];
  news: string | null;
}

export interface ManifestCheckResult {
  manifest: RemoteManifest;
  is_offline_fallback: boolean;
  offline_warning: string | null;
  update_available: boolean;
  installed_version: string | null;
}

export interface ServerStatus {
  online: boolean;
  host: string;
  port: number;
  players_online: number;
  players_max: number;
  motd: string;
  version: string | null;
  latency_ms: number | null;
}

export interface DashboardStatus {
  manifest_check: ManifestCheckResult;
  server_status: ServerStatus;
}

export interface DeviceCodeResponse {
  device_code: string;
  user_code: string;
  verification_uri: string;
  expires_in: number;
  interval: number;
  message: string;
}

export interface SyncProgressEvent {
  stage: string;
  current_file: string;
  completed: number;
  total: number;
  percent: number;
}

export interface GameLogEntry {
  timestamp: string;
  level: "INFO" | "WARN" | "ERROR" | string;
  line: string;
}

export interface GameExitEvent {
  exit_code: number;
  crashed: boolean;
}

export interface ModrinthSearchHit {
  project_id: string;
  slug: string;
  title: string;
  author: string;
  description: string;
  icon_url: string | null;
  downloads: number;
  follows: number;
}

export interface ModrinthVersionItem {
  id: string;
  version_number: string;
  name: string;
  game_versions: string[];
  loaders: string[];
  date_published: string;
  filename: string;
}

export interface InstalledModEntry {
  filename: string;
  enabled: boolean;
  is_user_mod: boolean;
  size_kb: number;
}
