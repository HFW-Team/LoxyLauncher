<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { open } from "@tauri-apps/plugin-dialog";
  import { openUrl } from "@tauri-apps/plugin-opener";
  import { check as checkLauncherUpdate } from "@tauri-apps/plugin-updater";
  import type {
    LauncherConfig,
    SystemMemoryInfo,
    DetectedJava,
    InitialStateResponse,
    DashboardStatus,
    ManifestCheckResult,
    ServerStatus,
    DeviceCodeResponse,
    SyncProgressEvent,
    GameLogEntry,
    GameExitEvent,
    ModrinthSearchHit,
    ModrinthVersionItem,
    InstalledModEntry,
  } from "./lib/types";

  // Reactive Application State
  let config = $state<LauncherConfig>({
    instance_dir: "~/.local/share/loxylauncher/instance",
    shared_dir: "~/.local/share/loxylauncher/shared",
    min_ram_mb: 2048,
    max_ram_mb: 4096,
    jvm_args:
      "-XX:+UseG1GC -XX:+ParallelRefProcEnabled -XX:MaxGCPauseMillis=200 -XX:+UnlockExperimentalVMOptions -XX:+DisableExplicitGC",
    java_path: null,
    resolution_width: 1280,
    resolution_height: 720,
    fullscreen: false,
    close_behavior: "hide_and_reopen",
    discord_rpc_enabled: true,
    keep_user_mods: true,
    manifest_url:
      "https://raw.githubusercontent.com/loxy-community/modpack-manifest/main/launcher-manifest.json",
    server_host: "play.loxy.gg",
    server_port: 25565,
    azure_client_id: "00000000441cc96b",
    account: null,
  });

  let systemMemory = $state<SystemMemoryInfo>({
    total_ram_mb: 16384,
    recommended_max_mb: 6144,
  });

  let detectedJavas = $state<DetectedJava[]>([]);

  let manifestCheck = $state<ManifestCheckResult>({
    manifest: {
      name: "Loxy Private Pack",
      version: "1.0.0",
      minecraft_version: "1.21.1",
      loader: "fabric",
      loader_version: "0.16.9",
      server_host: "play.loxy.gg",
      server_port: 25565,
      mrpack_url: null,
      mrpack_sha256: null,
      files: [],
      news: "Loading remote modpack manifest & server status...",
    },
    is_offline_fallback: false,
    offline_warning: null,
    update_available: false,
    installed_version: null,
  });

  let serverStatus = $state<ServerStatus>({
    online: false,
    host: "play.loxy.gg",
    port: 25565,
    players_online: 0,
    players_max: 20,
    motd: "Pinging server...",
    version: null,
    latency_ms: null,
  });

  let isRefreshing = $state(false);
  let isLaunching = $state(false);
  let syncProgress = $state<SyncProgressEvent | null>(null);
  let statusBanner = $state<{ type: "info" | "warn" | "error"; text: string } | null>(null);

  // Device Code Auth State
  let deviceCodeInfo = $state<DeviceCodeResponse | null>(null);
  let isAuthPolling = $state(false);
  let authError = $state<string | null>(null);
  let codeCopied = $state(false);

  // Console Logs State
  let logs = $state<GameLogEntry[]>([]);
  let logFilter = $state<"ALL" | "INFO" | "WARN" | "ERROR">("ALL");
  let logsCopied = $state(false);

  // Modrinth Browser & Installed Mods State
  let modsTab = $state<"browse" | "installed">("browse");
  let modSearchQuery = $state("");
  let modFilterVersion = $state("1.21.1");
  let modFilterLoader = $state("fabric");
  let modSortIndex = $state<"relevance" | "downloads" | "follows" | "newest" | "updated">("relevance");
  let modSearchFilterInitialized = $state(false);

  let modSearchResults = $state<ModrinthSearchHit[]>([]);
  let isSearchingMods = $state(false);
  let searchSeq = 0;
  let searchDebounceTimer: ReturnType<typeof setTimeout> | null = null;

  // Version Picker Dropdown State per Mod Project ID
  let expandedProjectVersions = $state<Record<string, boolean>>({});
  let loadingProjectVersions = $state<Record<string, boolean>>({});
  let projectVersionsMap = $state<Record<string, ModrinthVersionItem[]>>({});
  let installingProjectIds = $state<Record<string, boolean>>({});

  // Installed Mods & Collapsible Group State
  let installedMods = $state<InstalledModEntry[]>([]);
  let installedSearchFilter = $state("");
  let userModsCollapsed = $state(false);
  let packModsCollapsed = $state(false);
  let modsModalNotice = $state<string | null>(null);

  // Updater State
  let updaterState = $state<string>("v0.1.0");

  // Native <dialog> Element Refs
  let authDialog: HTMLDialogElement | undefined = $state();
  let settingsDialog: HTMLDialogElement | undefined = $state();
  let consoleDialog: HTMLDialogElement | undefined = $state();
  let ramDialog: HTMLDialogElement | undefined = $state();
  let modsDialog: HTMLDialogElement | undefined = $state();

  let filteredLogs = $derived(
    logFilter === "ALL" ? logs : logs.filter((l) => l.level === logFilter)
  );

  let errorCount = $derived(logs.filter((l) => l.level === "ERROR").length);

  let filteredInstalledMods = $derived(
    installedSearchFilter.trim() === ""
      ? installedMods
      : installedMods.filter((m) =>
          m.filename.toLowerCase().includes(installedSearchFilter.trim().toLowerCase())
        )
  );

  let userAddedMods = $derived(filteredInstalledMods.filter((m) => m.is_user_mod));
  let modpackMods = $derived(filteredInstalledMods.filter((m) => !m.is_user_mod));

  function attachDialogLightDismissFallback(dialogEl: HTMLDialogElement | undefined) {
    if (!dialogEl) return;
    if (!("closedBy" in HTMLDialogElement.prototype)) {
      const outside = (e: PointerEvent, d: HTMLDialogElement) => {
        const r = d.getBoundingClientRect();
        return (
          e.clientX < r.left ||
          e.clientX > r.right ||
          e.clientY < r.top ||
          e.clientY > r.bottom
        );
      };
      let pressedOutside = false;
      dialogEl.addEventListener("pointerdown", (e) => {
        pressedOutside = outside(e, dialogEl);
      });
      dialogEl.addEventListener("pointerup", (e) => {
        if (pressedOutside && outside(e, dialogEl)) {
          dialogEl.close();
        }
      });
    }
  }

  onMount(() => {
    attachDialogLightDismissFallback(authDialog);
    attachDialogLightDismissFallback(settingsDialog);
    attachDialogLightDismissFallback(consoleDialog);
    attachDialogLightDismissFallback(ramDialog);
    attachDialogLightDismissFallback(modsDialog);

    (async () => {
      try {
        const init = await invoke<InitialStateResponse>("get_initial_state");
        config = init.config;
        systemMemory = init.system_memory;
        detectedJavas = init.detected_javas ?? [];

        const existingLogs = await invoke<GameLogEntry[]>("get_game_logs");
        logs = existingLogs.slice(-500);

        await refreshManifestAndServer();
      } catch (err) {
        statusBanner = {
          type: "warn",
          text: `Running in UI preview mode (${String(err)})`,
        };
      }
    })();

    const unlistenSync = listen<SyncProgressEvent>("sync-progress", (ev) => {
      syncProgress = ev.payload;
    });

    const unlistenManifestSynced = listen<ManifestCheckResult>("manifest-synced", (ev) => {
      manifestCheck = ev.payload;
    });

    const unlistenLog = listen<GameLogEntry>("game-log", (ev) => {
      logs = [...logs.slice(-499), ev.payload];
    });

    const unlistenExit = listen<GameExitEvent>("game-exited", (ev) => {
      isLaunching = false;
      syncProgress = null;
      if (ev.payload.crashed) {
        statusBanner = {
          type: "error",
          text: `Minecraft exited with code ${ev.payload.exit_code}. Open Console to inspect the crash report.`,
        };
        consoleDialog?.showModal();
      } else {
        statusBanner = {
          type: "info",
          text: "Minecraft closed cleanly.",
        };
      }
    });

    return () => {
      unlistenSync.then((fn) => fn());
      unlistenManifestSynced.then((fn) => fn());
      unlistenLog.then((fn) => fn());
      unlistenExit.then((fn) => fn());
    };
  });

  async function refreshManifestAndServer() {
    isRefreshing = true;
    try {
      const dash = await invoke<DashboardStatus>("check_manifest_and_server");
      manifestCheck = dash.manifest_check;
      serverStatus = dash.server_status;
      if (!modSearchFilterInitialized) {
        modFilterVersion = dash.manifest_check.manifest.minecraft_version;
        modFilterLoader = dash.manifest_check.manifest.loader;
        modSearchFilterInitialized = true;
      }
      if (dash.manifest_check.is_offline_fallback && dash.manifest_check.offline_warning) {
        statusBanner = {
          type: "warn",
          text: dash.manifest_check.offline_warning,
        };
      } else {
        statusBanner = null;
      }
    } catch (e) {
      statusBanner = {
        type: "warn",
        text: `Failed to refresh status: ${String(e)}`,
      };
    } finally {
      isRefreshing = false;
    }
  }

  async function pingServerOnly() {
    isRefreshing = true;
    try {
      serverStatus = await invoke<ServerStatus>("ping_server", {
        host: manifestCheck.manifest.server_host,
        port: manifestCheck.manifest.server_port,
      });
    } catch (e) {
      statusBanner = {
        type: "warn",
        text: `Server ping error: ${String(e)}`,
      };
    } finally {
      isRefreshing = false;
    }
  }

  async function openSignInModal() {
    authError = null;
    deviceCodeInfo = null;
    codeCopied = false;
    authDialog?.showModal();

    try {
      const dc = await invoke<DeviceCodeResponse>("start_microsoft_device_auth");
      deviceCodeInfo = dc;
      isAuthPolling = true;

      const account = await invoke<LauncherConfig["account"]>(
        "complete_microsoft_device_auth",
        {
          deviceCode: dc.device_code,
          interval: dc.interval,
          expiresIn: dc.expires_in,
        }
      );

      config.account = account;
      isAuthPolling = false;
      authDialog?.close();
      statusBanner = {
        type: "info",
        text: `Signed in as ${account?.username}`,
      };
    } catch (err) {
      isAuthPolling = false;
      authError = String(err);
    }
  }

  async function copyCodeAndOpenLink() {
    if (!deviceCodeInfo) return;
    try {
      await navigator.clipboard.writeText(deviceCodeInfo.user_code);
      codeCopied = true;
    } catch {
      // Fallback if clipboard permission denied
    }
    try {
      await openUrl(deviceCodeInfo.verification_uri);
    } catch {
      window.open(deviceCodeInfo.verification_uri, "_blank");
    }
  }

  async function handleLogout() {
    try {
      await invoke("logout_account");
      config.account = null;
      statusBanner = {
        type: "info",
        text: "Signed out of Microsoft account.",
      };
    } catch (err) {
      statusBanner = {
        type: "error",
        text: `Sign out failed: ${String(err)}`,
      };
    }
  }

  async function handleSaveSettings() {
    try {
      if (config.min_ram_mb > config.max_ram_mb) {
        config.min_ram_mb = config.max_ram_mb;
      }
      const saved = await invoke<LauncherConfig>("save_launcher_config", {
        newConfig: config,
      });
      config = saved;
      settingsDialog?.close();
      ramDialog?.close();
      statusBanner = {
        type: "info",
        text: "Configuration saved.",
      };
      await refreshManifestAndServer();
    } catch (err) {
      statusBanner = {
        type: "error",
        text: `Failed to save settings: ${String(err)}`,
      };
    }
  }

  async function openModsModal() {
    modsModalNotice = null;
    if (!modSearchFilterInitialized) {
      modFilterVersion = manifestCheck.manifest.minecraft_version;
      modFilterLoader = manifestCheck.manifest.loader;
      modSearchFilterInitialized = true;
    }
    modsDialog?.showModal();
    await loadInstalledMods();
    if (modSearchResults.length === 0) {
      await handleSearchModrinth();
    }
  }

  async function loadInstalledMods() {
    try {
      installedMods = await invoke<InstalledModEntry[]>("get_installed_mods");
    } catch (err) {
      modsModalNotice = `Could not list mods: ${String(err)}`;
    }
  }

  function triggerRealtimeModSearch() {
    if (searchDebounceTimer) {
      clearTimeout(searchDebounceTimer);
    }
    searchDebounceTimer = setTimeout(() => {
      handleSearchModrinth();
    }, 180);
  }

  async function handleSearchModrinth(e?: Event) {
    if (e) e.preventDefault();
    const mySeq = ++searchSeq;
    isSearchingMods = true;
    try {
      const hits = await invoke<ModrinthSearchHit[]>("search_mods_on_modrinth", {
        query: modSearchQuery,
        mcVersion: modFilterVersion,
        loader: modFilterLoader,
        sortIndex: modSortIndex,
      });
      if (mySeq === searchSeq) {
        modSearchResults = hits;
      }
    } catch (err) {
      if (mySeq === searchSeq) {
        modsModalNotice = `Modrinth search error: ${String(err)}`;
      }
    } finally {
      if (mySeq === searchSeq) {
        isSearchingMods = false;
      }
    }
  }

  async function toggleModVersionDropdown(projectId: string) {
    const isCurrentlyOpen = !!expandedProjectVersions[projectId];
    if (isCurrentlyOpen) {
      expandedProjectVersions = { ...expandedProjectVersions, [projectId]: false };
      return;
    }

    expandedProjectVersions = { ...expandedProjectVersions, [projectId]: true };
    loadingProjectVersions = { ...loadingProjectVersions, [projectId]: true };

    try {
      const vers = await invoke<ModrinthVersionItem[]>("get_mod_versions_on_modrinth", {
        projectId,
        mcVersion: modFilterVersion,
        loader: modFilterLoader,
      });
      projectVersionsMap = { ...projectVersionsMap, [projectId]: vers };
    } catch (err) {
      modsModalNotice = `Could not load versions: ${String(err)}`;
    } finally {
      loadingProjectVersions = { ...loadingProjectVersions, [projectId]: false };
    }
  }

  async function handleInstallModrinthMod(hit: ModrinthSearchHit, versionId?: string) {
    const busyKey = versionId ? `${hit.project_id}:${versionId}` : hit.project_id;
    installingProjectIds = { ...installingProjectIds, [busyKey]: true, [hit.project_id]: true };
    modsModalNotice = null;
    try {
      if (!config.keep_user_mods) {
        config.keep_user_mods = true;
        await invoke("save_launcher_config", { newConfig: config });
      }
      const installed = await invoke<InstalledModEntry>("install_mod_from_modrinth", {
        projectId: hit.project_id,
        versionId: versionId ?? null,
        mcVersion: modFilterVersion || manifestCheck.manifest.minecraft_version,
        loader: modFilterLoader || manifestCheck.manifest.loader,
      });
      modsModalNotice = `Installed ${hit.title} (${installed.filename})`;
      await loadInstalledMods();
    } catch (err) {
      modsModalNotice = `Failed to install ${hit.title}: ${String(err)}`;
    } finally {
      installingProjectIds = { ...installingProjectIds, [busyKey]: false, [hit.project_id]: false };
    }
  }

  async function handleManageInstalledMod(filename: string, deleteMod: boolean) {
    try {
      installedMods = await invoke<InstalledModEntry[]>("manage_installed_mod", {
        filename,
        delete: deleteMod,
      });
    } catch (err) {
      modsModalNotice = `Mod action failed: ${String(err)}`;
    }
  }

  async function browseInstanceFolder() {
    try {
      const selected = await open({
        directory: true,
        multiple: false,
        defaultPath: config.instance_dir,
        title: "Select Modpack Instance Folder",
      });
      if (typeof selected === "string") {
        config.instance_dir = selected;
      }
    } catch (err) {
      statusBanner = {
        type: "warn",
        text: `Folder dialog unavailable: ${String(err)}`,
      };
    }
  }

  async function browseJavaPath() {
    try {
      const selected = await open({
        directory: false,
        multiple: false,
        title: "Select Java Executable (java / javaw.exe)",
      });
      if (typeof selected === "string") {
        config.java_path = selected;
      }
    } catch (err) {
      statusBanner = {
        type: "warn",
        text: `File dialog unavailable: ${String(err)}`,
      };
    }
  }

  async function handlePlay() {
    if (!config.account) {
      openSignInModal();
      return;
    }
    isLaunching = true;
    statusBanner = null;
    syncProgress = {
      stage: "Preparing launch sequence...",
      current_file: manifestCheck.manifest.name,
      completed: 0,
      total: 1,
      percent: 2,
    };

    try {
      await invoke("sync_and_launch_game");
      manifestCheck.update_available = false;
      manifestCheck.installed_version = manifestCheck.manifest.version;
    } catch (err) {
      isLaunching = false;
      syncProgress = null;
      statusBanner = {
        type: "error",
        text: String(err),
      };
    }
  }

  async function checkForLauncherUpdate() {
    updaterState = "Checking...";
    try {
      const update = await checkLauncherUpdate();
      if (update) {
        updaterState = `Update ${update.version} available! Installing...`;
        await update.downloadAndInstall();
        updaterState = "Updated! Restart launcher.";
      } else {
        updaterState = "Up to date (v0.1.0)";
      }
    } catch {
      updaterState = "v0.1.0 (Latest)";
    }
  }

  async function copyServerIp() {
    const ip =
      manifestCheck.manifest.server_port === 25565
        ? manifestCheck.manifest.server_host
        : `${manifestCheck.manifest.server_host}:${manifestCheck.manifest.server_port}`;
    await navigator.clipboard.writeText(ip);
    statusBanner = {
      type: "info",
      text: `Copied server IP: ${ip}`,
    };
  }

  async function copyAllLogs() {
    const text = logs
      .map((l) => `[${l.timestamp}] [${l.level}] ${l.line}`)
      .join("\n");
    await navigator.clipboard.writeText(text);
    logsCopied = true;
    setTimeout(() => {
      logsCopied = false;
    }, 2000);
  }

  async function clearLogs() {
    await invoke("clear_game_logs");
    logs = [];
  }
</script>

<div class="flex flex-col h-screen w-screen overflow-hidden select-none">
  <!-- TOP HEADER BAR -->
  <header class="mc-panel px-4 py-3 flex items-center justify-between z-10">
    <!-- Brand & Version -->
    <div class="flex items-center gap-3">
      <!-- Pixel Grass Block Icon -->
      <div
        class="w-10 h-10 mc-inset flex flex-col overflow-hidden shrink-0"
        aria-hidden="true"
      >
        <div class="h-3 bg-[#4caf50] border-b-2 border-[#388e3c]"></div>
        <div class="flex-1 bg-[#795548] flex items-center justify-center text-xs font-bold text-[#d7ccc8]">
          LX
        </div>
      </div>
      <div>
        <div class="flex items-center gap-2">
          <h1 class="font-mc-title text-lg tracking-wider text-[#66bb6a]">
            LOXYLAUNCHER
          </h1>
          <button
            type="button"
            onclick={checkForLauncherUpdate}
            class="px-2 py-0.5 text-xs bg-[#1d1d1d] border border-[#444] text-[#aaa] hover:text-white cursor-pointer"
            title="Click to check for launcher updates"
          >
            {updaterState}
          </button>
        </div>
        <p class="text-xs text-[#9e9e9e]">
          Dedicated Private Modpack Client
        </p>
      </div>
    </div>

    <!-- Account & Actions -->
    <div class="flex items-center gap-2.5">
      {#if config.account}
        <div class="mc-inset px-3 py-1.5 flex items-center gap-2.5">
          <img
            src={`https://mc-heads.net/avatar/${config.account.uuid}/28`}
            alt={config.account.username}
            class="w-7 h-7 border border-black bg-[#222]"
          />
          <div class="text-left">
            <div class="text-xs text-[#a5d6a7] font-bold leading-tight">
              {config.account.username}
            </div>
            <div class="text-[11px] text-[#888]">Microsoft Account</div>
          </div>
          <button
            type="button"
            onclick={handleLogout}
            class="mc-btn mc-btn-red px-2 py-1 text-xs ml-1"
            title="Sign out"
          >
            LOGOUT
          </button>
        </div>
      {:else}
        <button
          type="button"
          onclick={openSignInModal}
          class="mc-btn mc-btn-gold px-3.5 py-2 text-xs font-bold"
        >
          <span>MICROSOFT SIGN IN</span>
        </button>
      {/if}

      <button
        type="button"
        onclick={openModsModal}
        class="mc-btn px-3 py-2 text-xs"
        title="Search Modrinth & Manage Installed Mods"
      >
        <span>MODS</span>
      </button>

      <button
        type="button"
        onclick={() => consoleDialog?.showModal()}
        class="mc-btn px-3 py-2 text-xs"
      >
        <span>CONSOLE</span>
        {#if errorCount > 0}
          <span class="px-1.5 py-0.2 bg-[#d32f2f] text-white text-xs">
            {errorCount}
          </span>
        {/if}
      </button>

      <button
        type="button"
        onclick={() => settingsDialog?.showModal()}
        class="mc-btn px-3 py-2 text-xs"
      >
        <span>SETTINGS</span>
      </button>
    </div>
  </header>

  <!-- STATUS BANNER (IF PRESENT) -->
  {#if statusBanner}
    <div
      class="px-4 py-2 text-xs flex items-center justify-between border-b-2 border-black {statusBanner.type ===
      'error'
        ? 'bg-[#7f1d1d] text-[#fecaca]'
        : statusBanner.type === 'warn'
          ? 'bg-[#78350f] text-[#fde68a]'
          : 'bg-[#14532d] text-[#bbf7d0]'}"
    >
      <div class="flex items-center gap-2">
        <span class="font-bold uppercase">[{statusBanner.type}]</span>
        <span>{statusBanner.text}</span>
      </div>
      <button
        type="button"
        onclick={() => (statusBanner = null)}
        class="text-xs underline cursor-pointer ml-4"
      >
        DISMISS
      </button>
    </div>
  {/if}

  <!-- MAIN CONTENT GRID -->
  <main class="flex-1 p-5 grid grid-cols-12 gap-5 overflow-y-auto">
    <!-- LEFT COLUMN: MODPACK HERO & CHANGELOG (7 COLS) -->
    <div class="col-span-7 flex flex-col gap-4">
      <!-- Modpack Hero Card -->
      <section class="mc-panel p-4 flex flex-col gap-3">
        <div class="flex items-start justify-between">
          <div>
            <span class="text-xs uppercase tracking-widest text-[#81c784]">
              Featured Private Pack
            </span>
            <h2 class="font-mc-title text-xl text-white mt-1">
              {manifestCheck.manifest.name}
            </h2>
          </div>

          <div class="flex gap-2">
            <button
              type="button"
              onclick={openModsModal}
              class="mc-btn px-2.5 py-1 text-xs"
            >
              + ADD MODS
            </button>
            <button
              type="button"
              onclick={refreshManifestAndServer}
              disabled={isRefreshing || isLaunching}
              class="mc-btn px-2.5 py-1 text-xs"
            >
              {isRefreshing ? "SYNCING..." : "CHECK UPDATES"}
            </button>
          </div>
        </div>

        <!-- Badges Row -->
        <div class="flex flex-wrap items-center gap-2 pt-1">
          <span class="mc-inset px-2.5 py-1 text-xs text-[#ffd54f]">
            PACK v{manifestCheck.manifest.version}
          </span>
          <span class="mc-inset px-2.5 py-1 text-xs text-[#4fc3f7]">
            MC {manifestCheck.manifest.minecraft_version}
          </span>
          <span class="mc-inset px-2.5 py-1 text-xs text-[#ce93d8] uppercase">
            {manifestCheck.manifest.loader} {manifestCheck.manifest.loader_version}
          </span>
          {#if manifestCheck.is_offline_fallback}
            <span class="mc-inset px-2.5 py-1 text-xs text-[#ffb74d]">
              OFFLINE CACHE
            </span>
          {:else if manifestCheck.update_available}
            <span class="mc-inset px-2.5 py-1 text-xs text-[#66bb6a]">
              UPDATE READY
            </span>
          {:else}
            <span class="mc-inset px-2.5 py-1 text-xs text-[#a5d6a7]">
              VERIFIED & READY
            </span>
          {/if}
        </div>
      </section>

      <!-- News / Pack Manifest Details -->
      <section class="mc-panel p-4 flex-1 flex flex-col gap-2">
        <div class="flex items-center justify-between border-b-2 border-[#1b1b1b] pb-2">
          <h3 class="text-xs font-bold uppercase tracking-wider text-[#b0bec5]">
            Server News & Modpack Notes
          </h3>
          <span class="text-xs text-[#888]">
            {config.keep_user_mods ? "User Mods Allowed" : "Strict Pack Sync"}
          </span>
        </div>

        <div class="mc-inset p-3.5 flex-1 overflow-y-auto text-sm leading-relaxed text-[#dedede] space-y-2 whitespace-pre-line">
          <p>
            {manifestCheck.manifest.news ??
              "No announcement posted in the remote manifest."}
          </p>
          <div class="pt-3 border-t border-[#262626] text-xs text-[#999] space-y-1">
            <div>
              <strong class="text-[#bbb]">Remote Source:</strong>
              {manifestCheck.manifest.mrpack_url
                ? "Modrinth .mrpack + Parallel SHA Verification"
                : "Remote JSON Hash Manifest"}
            </div>
            <div class="truncate" title={config.instance_dir}>
              <strong class="text-[#bbb]">Instance Path:</strong>
              {config.instance_dir}
            </div>
          </div>
        </div>
      </section>
    </div>

    <!-- RIGHT COLUMN: SERVER PING & LAUNCHER TELEMETRY (5 COLS) -->
    <div class="col-span-5 flex flex-col gap-4">
      <!-- Live Server Status Card -->
      <section class="mc-panel p-4 flex flex-col gap-3">
        <div class="flex items-center justify-between">
          <div class="flex items-center gap-2">
            <span
              class="w-3 h-3 inline-block border border-black {serverStatus.online
                ? 'bg-[#00e676] shadow-[0_0_6px_#00e676]'
                : 'bg-[#ff1744]'}"
            ></span>
            <h3 class="text-xs font-bold uppercase tracking-wider">
              {serverStatus.online ? "SERVER ONLINE" : "SERVER OFFLINE"}
            </h3>
          </div>
          <button
            type="button"
            onclick={pingServerOnly}
            disabled={isRefreshing}
            class="mc-btn px-2.5 py-0.5 text-xs"
          >
            PING
          </button>
        </div>

        <!-- Server List Entry Box (Classic Minecraft Multiplayer Look) -->
        <div class="mc-inset p-3 flex flex-col gap-2">
          <div class="flex items-center justify-between text-xs">
            <span class="text-[#fff] font-bold truncate">
              {manifestCheck.manifest.server_host}:{manifestCheck.manifest.server_port}
            </span>
            <span class="text-[#81c784] text-xs">
              {serverStatus.latency_ms != null ? `${serverStatus.latency_ms} ms` : "---"}
            </span>
          </div>

          <p class="font-mc-mono text-sm text-[#bdbdbd] leading-snug">
            {serverStatus.motd}
          </p>

          <!-- Player Count Bar -->
          <div class="pt-1">
            <div class="flex justify-between text-xs text-[#aaa] mb-1">
              <span>PLAYERS ONLINE</span>
              <span class="text-white font-bold">
                {serverStatus.players_online} / {serverStatus.players_max}
              </span>
            </div>
            <div class="w-full h-3 bg-[#1b1b1b] border border-[#333]">
              <div
                class="h-full bg-[#4caf50] transition-all duration-300"
                style={`width: ${
                  serverStatus.players_max > 0
                    ? Math.min(
                        100,
                        Math.round(
                          (serverStatus.players_online / serverStatus.players_max) * 100
                        )
                      )
                    : 0
                }%`}
              ></div>
            </div>
          </div>
        </div>

        <button
          type="button"
          onclick={copyServerIp}
          class="mc-btn w-full py-1.5 text-xs"
        >
          COPY SERVER ADDRESS
        </button>
      </section>

      <!-- Active Configuration Summary Card -->
      <section class="mc-panel p-4 flex-1 flex flex-col justify-between gap-2">
        <h3 class="text-xs font-bold uppercase tracking-wider text-[#b0bec5] border-b-2 border-[#1b1b1b] pb-1.5">
          Client Profile Configuration
        </h3>

        <div class="grid grid-cols-2 gap-2.5 text-xs my-auto">
          <!-- CLICKABLE ALLOCATED RAM CONTAINER -->
          <button
            type="button"
            onclick={() => ramDialog?.showModal()}
            class="mc-inset p-2.5 text-left hover:border-[#66bb6a] cursor-pointer transition-colors group"
            title="Click to adjust Allocated RAM"
          >
            <div class="flex items-center justify-between text-[11px] text-[#999] group-hover:text-[#a5d6a7]">
              <span>ALLOCATED RAM</span>
              <span class="underline">EDIT</span>
            </div>
            <div class="text-sm text-[#66bb6a] font-bold mt-0.5">
              {config.max_ram_mb} MB
            </div>
          </button>

          <button
            type="button"
            onclick={() => settingsDialog?.showModal()}
            class="mc-inset p-2.5 text-left hover:border-[#4fc3f7] cursor-pointer transition-colors"
            title="Click to configure Java runtime"
          >
            <div class="text-[11px] text-[#999]">JAVA RUNTIME</div>
            <div class="text-sm text-[#4fc3f7] font-bold mt-0.5 truncate">
              {config.java_path
                ? "Custom JDK"
                : detectedJavas.length > 0
                  ? `System JDK (${detectedJavas.length})`
                  : "Auto JRE"}
            </div>
          </button>

          <button
            type="button"
            onclick={() => settingsDialog?.showModal()}
            class="mc-inset p-2.5 text-left hover:border-white cursor-pointer transition-colors"
            title="Click to change resolution & fullscreen"
          >
            <div class="text-[11px] text-[#999]">DISPLAY</div>
            <div class="text-sm text-white font-bold mt-0.5">
              {config.fullscreen
                ? "Fullscreen"
                : `${config.resolution_width}x${config.resolution_height}`}
            </div>
          </button>

          <button
            type="button"
            onclick={openModsModal}
            class="mc-inset p-2.5 text-left hover:border-[#ce93d8] cursor-pointer transition-colors"
            title="Click to manage user mods"
          >
            <div class="text-[11px] text-[#999]">USER MODS</div>
            <div class="text-sm text-[#ce93d8] font-bold mt-0.5">
              {config.keep_user_mods ? "ALLOWED" : "LOCKED"}
            </div>
          </button>
        </div>
      </section>
    </div>
  </main>

  <!-- BOTTOM DIRT LAUNCH DECK WITH CENTERED PLAY BUTTON -->
  <footer class="mc-dirt-bar px-6 py-3.5 grid grid-cols-3 items-center gap-4">
    <!-- Left Column: Sync / Hash Progress Status -->
    <div class="min-w-0">
      {#if syncProgress}
        <div class="flex justify-between text-xs mb-1">
          <span class="text-[#a5d6a7] font-bold truncate">{syncProgress.stage}</span>
          <span class="text-white ml-2">{syncProgress.percent}%</span>
        </div>
        <div class="w-full h-4 mc-inset p-0.5">
          <div
            class="h-full bg-[#43a047] transition-all duration-150"
            style={`width: ${syncProgress.percent}%`}
          ></div>
        </div>
        <div class="text-xs text-[#bdbdbd] mt-1 truncate">
          {syncProgress.current_file}
        </div>
      {:else}
        <div class="flex items-center gap-2.5">
          <div class="w-3 h-3 bg-[#66bb6a] border border-black shrink-0"></div>
          <div class="truncate">
            <div class="text-xs font-bold text-white truncate">
              {manifestCheck.update_available
                ? `Update v${manifestCheck.manifest.version} Ready`
                : `${manifestCheck.manifest.name} Ready`}
            </div>
            <div class="text-xs text-[#aaa] truncate">
              {config.account
                ? `Signed in as ${config.account.username}`
                : "Sign in with Microsoft to play"}
            </div>
          </div>
        </div>
      {/if}
    </div>

    <!-- Center Column: Centered Giant Minecraft Play Button -->
    <div class="flex justify-center">
      <button
        type="button"
        onclick={handlePlay}
        disabled={isLaunching}
        class="mc-btn mc-btn-green px-12 py-3.5 text-base font-mc-title tracking-wider"
      >
        {#if isLaunching}
          <span>LAUNCHING...</span>
        {:else if !config.account}
          <span>SIGN IN & PLAY</span>
        {:else if manifestCheck.update_available}
          <span>UPDATE & PLAY</span>
        {:else}
          <span>PLAY</span>
        {/if}
      </button>
    </div>

    <!-- Right Column: Modrinth Mods Quick Button -->
    <div class="flex items-center justify-end gap-2 text-xs text-[#bdbdbd]">
      <button
        type="button"
        onclick={openModsModal}
        class="mc-btn px-3.5 py-2 text-xs"
      >
        MODRINTH MODS
      </button>
    </div>
  </footer>
</div>

<!-- ========================================================================= -->
<!-- NATIVE MODAL 1: QUICK RAM ALLOCATION SLIDER DIALOG                        -->
<!-- ========================================================================= -->
<dialog
  bind:this={ramDialog}
  {...({ closedby: "any" } as any)}
  aria-labelledby="ram-dialog-title"
  class="mc-panel w-[460px] p-5 text-white"
>
  <div class="flex items-center justify-between border-b-2 border-[#181818] pb-3 mb-4">
    <h2 id="ram-dialog-title" class="font-mc-title text-base text-[#66bb6a]">
      ADJUST ALLOCATED RAM
    </h2>
    <form method="dialog">
      <button type="submit" class="mc-btn px-2.5 py-1 text-xs">X</button>
    </form>
  </div>

  <div class="mc-inset p-4 space-y-4 text-sm">
    <div class="flex justify-between items-center">
      <span class="text-[#a5d6a7] font-bold">Maximum JVM Heap (-Xmx)</span>
      <span class="text-base font-bold text-white">
        {config.max_ram_mb} MB ({(config.max_ram_mb / 1024).toFixed(1)} GB)
      </span>
    </div>

    <input
      type="range"
      min="2048"
      max={systemMemory.total_ram_mb}
      step="512"
      bind:value={config.max_ram_mb}
      aria-label="Maximum Memory Allocation Slider"
      class="mc-slider w-full"
    />

    <div class="flex justify-between text-xs text-[#999]">
      <span>2048 MB</span>
      <span>Recommended: {systemMemory.recommended_max_mb} MB</span>
      <span>Max: {systemMemory.total_ram_mb} MB</span>
    </div>

    <!-- Quick Presets -->
    <div class="pt-2 flex flex-wrap gap-2">
      {#each [4096, 6144, 8192, 12288, 16384].filter((v) => v <= systemMemory.total_ram_mb) as preset}
        <button
          type="button"
          onclick={() => (config.max_ram_mb = preset)}
          class="mc-btn px-3 py-1 text-xs {config.max_ram_mb === preset
            ? 'mc-btn-green'
            : ''}"
        >
          {preset / 1024} GB
        </button>
      {/each}
    </div>
  </div>

  <div class="mt-4 pt-3 border-t-2 border-[#181818] flex justify-end gap-2">
    <form method="dialog">
      <button type="submit" class="mc-btn px-4 py-1.5 text-xs">CANCEL</button>
    </form>
    <button
      type="button"
      onclick={handleSaveSettings}
      class="mc-btn mc-btn-green px-5 py-1.5 text-xs font-bold"
    >
      APPLY RAM
    </button>
  </div>
</dialog>

<!-- ========================================================================= -->
<!-- NATIVE MODAL 2: MODRINTH MOD BROWSER & INSTALLED MODS MANAGER             -->
<!-- ========================================================================= -->
<dialog
  bind:this={modsDialog}
  {...({ closedby: "any" } as any)}
  aria-labelledby="mods-dialog-title"
  class="mc-panel w-[820px] p-5 text-white"
>
  <div class="flex items-center justify-between border-b-2 border-[#181818] pb-3 mb-3">
    <div>
      <h2 id="mods-dialog-title" class="font-mc-title text-base text-[#66bb6a]">
        MODRINTH MOD BROWSER & INSTANCE MODS
      </h2>
      <p class="text-xs text-[#aaa]">
        Pack Target: <strong class="text-[#4fc3f7]">MC {manifestCheck.manifest.minecraft_version}</strong>
        • <strong class="text-[#ce93d8] uppercase">{manifestCheck.manifest.loader}</strong>
      </p>
    </div>

    <div class="flex items-center gap-2">
      <button
        type="button"
        onclick={() => (modsTab = "browse")}
        class="mc-btn px-3 py-1.5 text-xs {modsTab === 'browse' ? 'mc-btn-green' : ''}"
      >
        SEARCH MODRINTH
      </button>
      <button
        type="button"
        onclick={() => {
          modsTab = "installed";
          loadInstalledMods();
        }}
        class="mc-btn px-3 py-1.5 text-xs {modsTab === 'installed' ? 'mc-btn-green' : ''}"
      >
        INSTALLED ({installedMods.length})
      </button>
      <form method="dialog">
        <button type="submit" class="mc-btn px-2.5 py-1 text-xs ml-1">X</button>
      </form>
    </div>
  </div>

  <!-- Keep User Mods Setting Bar -->
  <div class="mc-inset px-3.5 py-2 mb-3 flex items-center justify-between text-xs">
    <label class="flex items-center gap-2 cursor-pointer">
      <input
        type="checkbox"
        bind:checked={config.keep_user_mods}
        onchange={() => invoke("save_launcher_config", { newConfig: config })}
      />
      <span class="font-bold text-[#a5d6a7]">
        Allow user-installed mods (preserve custom mods in mods/ folder during pack sync)
      </span>
    </label>
  </div>

  {#if modsModalNotice}
    <div class="mc-inset px-3 py-2 mb-3 text-xs text-[#fde68a] border border-[#f59e0b] flex justify-between items-center">
      <span>{modsModalNotice}</span>
      <button type="button" onclick={() => (modsModalNotice = null)} class="underline ml-2 cursor-pointer">
        DISMISS
      </button>
    </div>
  {/if}

  {#if modsTab === "browse"}
    <!-- Real-Time Search & Filters Bar (Aligned cleanly with results box) -->
    <div class="mc-inset p-3 mb-3 space-y-2.5">
      <div class="flex items-center gap-2">
        <input
          type="text"
          placeholder="Type to search Modrinth in real time (e.g. Sodium, Iris, Journeymap, Zoomify)..."
          bind:value={modSearchQuery}
          oninput={triggerRealtimeModSearch}
          class="flex-1 bg-[#1b1b1b] border border-[#444] px-3 py-2 text-sm text-white focus:border-[#66bb6a] outline-none"
        />
        {#if modSearchQuery.length > 0}
          <button
            type="button"
            onclick={() => {
              modSearchQuery = "";
              triggerRealtimeModSearch();
            }}
            class="mc-btn px-3 py-2 text-xs"
          >
            CLEAR
          </button>
        {/if}
      </div>

      <!-- Filter Controls: MC Version Override, Loader, Sort Order -->
      <div class="grid grid-cols-12 gap-2.5 items-center text-xs">
        <div class="col-span-5 flex items-center gap-1.5">
          <label for="mod-ver-override" class="text-[#aaa] shrink-0">MC Version:</label>
          <input
            id="mod-ver-override"
            type="text"
            placeholder="e.g. 1.21.1 or all"
            bind:value={modFilterVersion}
            oninput={() => {
              projectVersionsMap = {};
              triggerRealtimeModSearch();
            }}
            class="w-full bg-[#1b1b1b] border border-[#444] px-2 py-1 text-white font-mc-mono text-xs"
          />
          <button
            type="button"
            onclick={() => {
              modFilterVersion =
                modFilterVersion === "all"
                  ? manifestCheck.manifest.minecraft_version
                  : "all";
              projectVersionsMap = {};
              triggerRealtimeModSearch();
            }}
            class="mc-btn px-2 py-1 text-[11px] shrink-0"
            title="Toggle between pack MC version and All MC versions"
          >
            {modFilterVersion === "all" ? "PACK VER" : "ALL"}
          </button>
        </div>

        <div class="col-span-3 flex items-center gap-1.5">
          <label for="mod-loader-filter" class="text-[#aaa] shrink-0">Loader:</label>
          <select
            id="mod-loader-filter"
            bind:value={modFilterLoader}
            onchange={() => {
              projectVersionsMap = {};
              triggerRealtimeModSearch();
            }}
            class="w-full bg-[#1b1b1b] border border-[#444] px-2 py-1 text-white text-xs uppercase"
          >
            <option value="fabric">Fabric</option>
            <option value="neoforge">NeoForge</option>
            <option value="forge">Forge</option>
            <option value="all">All Loaders</option>
          </select>
        </div>

        <div class="col-span-4 flex items-center gap-1.5">
          <label for="mod-sort-filter" class="text-[#aaa] shrink-0">Sort:</label>
          <select
            id="mod-sort-filter"
            bind:value={modSortIndex}
            onchange={triggerRealtimeModSearch}
            class="w-full bg-[#1b1b1b] border border-[#444] px-2 py-1 text-white text-xs"
          >
            <option value="relevance">Relevance</option>
            <option value="downloads">Most Downloads</option>
            <option value="follows">Most Follows</option>
            <option value="updated">Recently Updated</option>
            <option value="newest">Newest</option>
          </select>
        </div>
      </div>
    </div>

    <!-- Search Results List -->
    <div class="mc-inset p-3 h-[350px] overflow-y-auto space-y-2.5">
      {#if isSearchingMods && modSearchResults.length === 0}
        <div class="text-center py-12 text-sm text-[#aaa]">
          Searching Modrinth...
        </div>
      {:else if modSearchResults.length === 0}
        <div class="text-center py-12 text-sm text-[#888]">
          No mods found matching your filters (MC: {modFilterVersion || "all"}, Loader: {modFilterLoader}).
        </div>
      {:else}
        {#each modSearchResults as hit}
          <div class="bg-[#1c1c1c] border border-[#333] p-3 flex flex-col gap-2">
            <div class="flex items-center justify-between gap-3">
              <div class="flex items-center gap-3 min-w-0 flex-1">
                {#if hit.icon_url}
                  <img
                    src={hit.icon_url}
                    alt={hit.title}
                    class="w-11 h-11 bg-[#111] border border-black shrink-0 object-cover"
                  />
                {:else}
                  <div class="w-11 h-11 bg-[#111] border border-black shrink-0 flex items-center justify-center text-xs text-[#666]">
                    MOD
                  </div>
                {/if}
                <div class="min-w-0 flex-1">
                  <div class="flex items-center gap-2">
                    <span class="font-bold text-sm text-white truncate">{hit.title}</span>
                    <span class="text-xs text-[#888] truncate">by {hit.author}</span>
                    <span class="text-[11px] text-[#66bb6a] ml-auto shrink-0">
                      {hit.downloads.toLocaleString()} dl
                    </span>
                  </div>
                  <p class="text-xs text-[#bdbdbd] line-clamp-2 mt-0.5">
                    {hit.description}
                  </p>
                </div>
              </div>

              <!-- Split Install Button + Slim Version Selector Arrow Button -->
              <div class="flex items-stretch shrink-0">
                <button
                  type="button"
                  disabled={installingProjectIds[hit.project_id]}
                  onclick={() => handleInstallModrinthMod(hit)}
                  class="mc-btn mc-btn-green px-3.5 py-2 text-xs"
                  title="Install latest matching version"
                >
                  {installingProjectIds[hit.project_id] ? "INSTALLING..." : "+ INSTALL"}
                </button>
                <button
                  type="button"
                  onclick={() => toggleModVersionDropdown(hit.project_id)}
                  class="mc-btn mc-btn-green px-2 py-2 text-xs border-l-0"
                  title="Select another or older version"
                  aria-label="Select Mod Version"
                >
                  {expandedProjectVersions[hit.project_id] ? "▲" : "▼"}
                </button>
              </div>
            </div>

            <!-- Expandable Version Picker Drawer -->
            {#if expandedProjectVersions[hit.project_id]}
              <div class="mc-inset p-2.5 mt-1 max-h-44 overflow-y-auto space-y-1.5 text-xs">
                <div class="text-[11px] text-[#aaa] font-bold border-b border-[#2b2b2b] pb-1">
                  Select Version for {hit.title} (Filter: MC {modFilterVersion || "all"} / {modFilterLoader}):
                </div>
                {#if loadingProjectVersions[hit.project_id]}
                  <div class="text-[#888] py-2 text-center">Loading versions from Modrinth...</div>
                {:else if !projectVersionsMap[hit.project_id] || projectVersionsMap[hit.project_id].length === 0}
                  <div class="text-[#888] py-2 text-center">
                    No versions matched filter. Try setting MC Version filter to "ALL" above.
                  </div>
                {:else}
                  {#each projectVersionsMap[hit.project_id] as ver}
                    <div class="flex items-center justify-between gap-2 bg-[#181818] px-2.5 py-1.5 border border-[#2c2c2c]">
                      <div class="min-w-0">
                        <div class="font-bold text-white truncate">
                          {ver.version_number}
                          <span class="font-normal text-[#999]">({ver.name})</span>
                        </div>
                        <div class="text-[11px] text-[#888] truncate">
                          MC: {ver.game_versions.join(", ")} • {ver.loaders.join(", ")} • {ver.date_published}
                        </div>
                      </div>
                      <button
                        type="button"
                        disabled={installingProjectIds[`${hit.project_id}:${ver.id}`]}
                        onclick={() => handleInstallModrinthMod(hit, ver.id)}
                        class="mc-btn px-2.5 py-1 text-xs shrink-0"
                      >
                        {installingProjectIds[`${hit.project_id}:${ver.id}`]
                          ? "..."
                          : `INSTALL v${ver.version_number}`}
                      </button>
                    </div>
                  {/each}
                {/if}
              </div>
            {/if}
          </div>
        {/each}
      {/if}
    </div>
  {:else}
    <!-- Installed Mods Tab with Filter & Collapsible "User Added" / "Modpack" Groups -->
    <div class="mb-2.5 flex items-center gap-2">
      <input
        type="text"
        placeholder="Filter installed mods by filename..."
        bind:value={installedSearchFilter}
        class="flex-1 bg-[#121212] border border-[#444] px-3 py-1.5 text-xs text-white"
      />
      <span class="text-xs text-[#888]">
        {userAddedMods.length} User • {modpackMods.length} Modpack
      </span>
    </div>

    <div class="mc-inset p-3 h-[380px] overflow-y-auto space-y-3">
      <!-- GROUP 1: USER ADDED MODS -->
      <div class="border border-[#2e5730] bg-[#141c14]">
        <button
          type="button"
          onclick={() => (userModsCollapsed = !userModsCollapsed)}
          class="w-full px-3 py-2 bg-[#1b301c] flex items-center justify-between text-xs font-bold text-[#a5d6a7] cursor-pointer hover:bg-[#223d24]"
        >
          <span>USER ADDED MODS ({userAddedMods.length})</span>
          <span>{userModsCollapsed ? "▶ EXPAND" : "▼ COLLAPSE"}</span>
        </button>

        {#if !userModsCollapsed}
          <div class="p-2 space-y-1.5">
            {#if userAddedMods.length === 0}
              <div class="text-xs text-[#777] py-3 text-center">
                No custom user-installed mods yet. Use the "Search Modrinth" tab to add mods!
              </div>
            {:else}
              {#each userAddedMods as mod}
                <div class="bg-[#1c1c1c] border border-[#333] px-3 py-2 flex items-center justify-between gap-3">
                  <div class="min-w-0">
                    <div class="flex items-center gap-2">
                      <span class="text-xs font-bold {mod.enabled ? 'text-white' : 'text-[#777] line-through'} truncate">
                        {mod.filename}
                      </span>
                      <span class="px-1.5 py-0.5 text-[10px] bg-[#1b5e20] text-[#a5d6a7] border border-[#388e3c]">
                        USER ADDED
                      </span>
                    </div>
                    <div class="text-[11px] text-[#888]">{mod.size_kb} KB</div>
                  </div>

                  <div class="flex items-center gap-2 shrink-0">
                    <button
                      type="button"
                      onclick={() => handleManageInstalledMod(mod.filename, false)}
                      class="mc-btn px-2.5 py-1 text-xs"
                    >
                      {mod.enabled ? "DISABLE" : "ENABLE"}
                    </button>
                    <button
                      type="button"
                      onclick={() => handleManageInstalledMod(mod.filename, true)}
                      class="mc-btn mc-btn-red px-2.5 py-1 text-xs"
                    >
                      DELETE
                    </button>
                  </div>
                </div>
              {/each}
            {/if}
          </div>
        {/if}
      </div>

      <!-- SEPARATOR -->
      <div class="border-t-2 border-[#2a2a2a]"></div>

      <!-- GROUP 2: MODPACK MODS -->
      <div class="border border-[#2d3e46] bg-[#13181b]">
        <button
          type="button"
          onclick={() => (packModsCollapsed = !packModsCollapsed)}
          class="w-full px-3 py-2 bg-[#1c282e] flex items-center justify-between text-xs font-bold text-[#90caf9] cursor-pointer hover:bg-[#24343c]"
        >
          <span>MODPACK MODS ({modpackMods.length})</span>
          <span>{packModsCollapsed ? "▶ EXPAND" : "▼ COLLAPSE"}</span>
        </button>

        {#if !packModsCollapsed}
          <div class="p-2 space-y-1.5">
            {#if modpackMods.length === 0}
              <div class="text-xs text-[#777] py-3 text-center">
                No modpack-managed mods in folder yet.
              </div>
            {:else}
              {#each modpackMods as mod}
                <div class="bg-[#1c1c1c] border border-[#333] px-3 py-2 flex items-center justify-between gap-3">
                  <div class="min-w-0">
                    <div class="flex items-center gap-2">
                      <span class="text-xs font-bold {mod.enabled ? 'text-white' : 'text-[#777] line-through'} truncate">
                        {mod.filename}
                      </span>
                      <span class="px-1.5 py-0.5 text-[10px] bg-[#263238] text-[#90caf9] border border-[#37474f]">
                        MODPACK
                      </span>
                    </div>
                    <div class="text-[11px] text-[#888]">{mod.size_kb} KB</div>
                  </div>

                  <div class="flex items-center gap-2 shrink-0">
                    <button
                      type="button"
                      onclick={() => handleManageInstalledMod(mod.filename, false)}
                      class="mc-btn px-2.5 py-1 text-xs"
                    >
                      {mod.enabled ? "DISABLE" : "ENABLE"}
                    </button>
                    <button
                      type="button"
                      onclick={() => handleManageInstalledMod(mod.filename, true)}
                      class="mc-btn mc-btn-red px-2.5 py-1 text-xs"
                    >
                      DELETE
                    </button>
                  </div>
                </div>
              {/each}
            {/if}
          </div>
        {/if}
      </div>
    </div>
  {/if}

  <div class="mt-4 pt-3 border-t-2 border-[#181818] flex justify-end">
    <form method="dialog">
      <!-- svelte-ignore a11y_autofocus -->
      <button type="submit" class="mc-btn px-4 py-1.5 text-xs" autofocus>
        CLOSE
      </button>
    </form>
  </div>
</dialog>

<!-- ========================================================================= -->
<!-- NATIVE MODAL 3: MICROSOFT DEVICE CODE OAUTH DIALOG                        -->
<!-- ========================================================================= -->
<dialog
  bind:this={authDialog}
  {...({ closedby: "any" } as any)}
  aria-labelledby="auth-dialog-title"
  class="mc-panel w-[500px] p-5 text-white"
>
  <div class="flex items-center justify-between border-b-2 border-[#181818] pb-3 mb-4">
    <h2 id="auth-dialog-title" class="font-mc-title text-base text-[#ffd54f]">
      MICROSOFT ACCOUNT SIGN IN
    </h2>
    <form method="dialog">
      <button type="submit" class="mc-btn px-2.5 py-1 text-xs">X</button>
    </form>
  </div>

  <div class="space-y-4 text-sm">
    <p class="text-[#d0d0d0] leading-relaxed">
      LoxyLauncher uses official Microsoft Device Code authentication. Enter the
      code below at <strong class="text-[#4fc3f7]">microsoft.com/link</strong> to
      sign in safely in your default browser:
    </p>

    {#if deviceCodeInfo}
      <div class="mc-inset p-4 text-center space-y-2">
        <div class="text-xs text-[#aaa]">YOUR VERIFICATION CODE</div>
        <div class="font-mc-title text-2xl tracking-widest text-[#66bb6a] select-all py-1">
          {deviceCodeInfo.user_code}
        </div>
        <button
          type="button"
          onclick={copyCodeAndOpenLink}
          class="mc-btn mc-btn-gold w-full py-2.5 text-xs font-bold"
        >
          {codeCopied
            ? "CODE COPIED! OPENING BROWSER..."
            : "COPY CODE & OPEN MICROSOFT.COM/LINK"}
        </button>
      </div>

      {#if isAuthPolling}
        <div class="flex items-center justify-center gap-2 text-[#81c784] py-2 text-xs">
          <span class="animate-pulse">●</span>
          <span>Waiting for you to complete login in browser...</span>
        </div>
      {/if}
    {:else if !authError}
      <div class="mc-inset p-5 text-center text-[#aaa]">
        Requesting verification code from Microsoft Identity Platform...
      </div>
    {/if}

    {#if authError}
      <div class="mc-inset p-3.5 border border-[#ef5350] text-[#ffcdd2] space-y-2 text-xs">
        <div class="font-bold text-[#ef5350]">AUTHENTICATION NOTICE</div>
        <p class="leading-relaxed select-text">{authError}</p>
        <div class="pt-1 flex gap-2">
          <button
            type="button"
            onclick={() => {
              authDialog?.close();
              settingsDialog?.showModal();
            }}
            class="mc-btn px-3 py-1.5 text-xs"
          >
            OPEN SETTINGS
          </button>
        </div>
      </div>
    {/if}
  </div>

  <div class="mt-5 pt-3 border-t-2 border-[#181818] flex justify-end">
    <form method="dialog">
      <!-- svelte-ignore a11y_autofocus -->
      <button type="submit" class="mc-btn px-4 py-1.5 text-xs" autofocus>
        CLOSE
      </button>
    </form>
  </div>
</dialog>

<!-- ========================================================================= -->
<!-- NATIVE MODAL 4: LAUNCHER SETTINGS DIALOG                                  -->
<!-- ========================================================================= -->
<dialog
  bind:this={settingsDialog}
  {...({ closedby: "any" } as any)}
  aria-labelledby="settings-dialog-title"
  class="mc-panel w-[700px] p-5 text-white"
>
  <div class="flex items-center justify-between border-b-2 border-[#181818] pb-3 mb-4">
    <h2 id="settings-dialog-title" class="font-mc-title text-base text-[#66bb6a]">
      LAUNCHER & MODPACK SETTINGS
    </h2>
    <form method="dialog">
      <button type="submit" class="mc-btn px-2.5 py-1 text-xs">X</button>
    </form>
  </div>

  <div class="space-y-4 overflow-y-auto max-h-[68vh] pr-1 text-sm">
    <!-- MODPACK SOURCE (MODRINTH / .MRPACK / MANIFEST) & SERVER IP -->
    <div class="mc-inset p-3.5 space-y-3">
      <div>
        <label for="manifest-url-input" class="font-bold text-[#66bb6a] block mb-1 text-xs">
          MODPACK SOURCE (MODRINTH SLUG, MODRINTH URL, .MRPACK URL, OR MANIFEST JSON)
        </label>
        <input
          id="manifest-url-input"
          type="text"
          placeholder="e.g. fabulously-optimized or https://modrinth.com/modpack/your-pack"
          bind:value={config.manifest_url}
          class="w-full bg-[#1b1b1b] border border-[#444] px-2.5 py-1.5 text-white font-mc-mono text-sm"
        />
        <div class="text-xs text-[#9e9e9e] mt-1.5 space-y-0.5">
          <div>• <strong>Modrinth Pack:</strong> Enter slug (<code class="text-[#81c784]">your-pack-slug</code> or <code class="text-[#81c784]">your-pack-slug@1.0.0</code>) or full <code class="text-[#81c784]">https://modrinth.com/modpack/...</code> URL</div>
          <div>• <strong>Direct .mrpack:</strong> Paste any <code class="text-[#4fc3f7]">https://.../*.mrpack</code> link (Modrinth CDN or GitHub Release)</div>
          <div>• <strong>Custom Manifest:</strong> Paste a <code class="text-[#ffd54f]">https://.../launcher-manifest.json</code> URL</div>
        </div>
      </div>

      <div class="grid grid-cols-3 gap-3 pt-1">
        <div class="col-span-2">
          <label for="server-host-input" class="font-bold text-[#4fc3f7] block mb-1 text-xs">
            MINECRAFT SERVER HOST / IP
          </label>
          <input
            id="server-host-input"
            type="text"
            placeholder="play.example.com"
            bind:value={config.server_host}
            class="w-full bg-[#1b1b1b] border border-[#444] px-2.5 py-1.5 text-white font-mc-mono text-sm"
          />
        </div>
        <div>
          <label for="server-port-input" class="font-bold text-[#4fc3f7] block mb-1 text-xs">
            SERVER PORT
          </label>
          <input
            id="server-port-input"
            type="number"
            bind:value={config.server_port}
            class="w-full bg-[#1b1b1b] border border-[#444] px-2.5 py-1.5 text-white font-mc-mono text-sm"
          />
        </div>
      </div>

      <label class="flex items-center gap-2 pt-1 cursor-pointer text-xs">
        <input type="checkbox" bind:checked={config.keep_user_mods} />
        <span class="font-bold text-[#a5d6a7]">
          Allow user-installed mods (do not delete custom mods in mods/ folder on sync)
        </span>
      </label>
    </div>

    <!-- RAM ALLOCATION -->
    <div class="mc-inset p-3.5 space-y-2">
      <div class="flex justify-between items-center">
        <label for="max-ram-slider" class="font-bold text-[#a5d6a7] text-xs">
          MAXIMUM MEMORY (RAM) ALLOCATION
        </label>
        <span class="text-sm font-bold text-white">
          {config.max_ram_mb} MB ({(config.max_ram_mb / 1024).toFixed(1)} GB)
        </span>
      </div>
      <!-- svelte-ignore a11y_autofocus -->
      <input
        id="max-ram-slider"
        type="range"
        min="2048"
        max={systemMemory.total_ram_mb}
        step="512"
        bind:value={config.max_ram_mb}
        class="mc-slider w-full"
        autofocus
      />
      <div class="flex justify-between text-xs text-[#888]">
        <span>Min: 2048 MB</span>
        <span>Recommended: {systemMemory.recommended_max_mb} MB</span>
        <span>System Total: {systemMemory.total_ram_mb} MB</span>
      </div>
    </div>

    <!-- JAVA RUNTIME & DETECTED SYSTEM JDKS -->
    <div class="mc-inset p-3.5 space-y-3">
      <div>
        <label for="java-path-input" class="font-bold text-[#ffd54f] block mb-1 text-xs">
          JAVA RUNTIME (AUTO-USES INSTALLED SYSTEM JDK 8 / 17 / 21+)
        </label>
        <div class="flex gap-2">
          <input
            id="java-path-input"
            type="text"
            placeholder="Auto-select matching system JDK (/usr/lib/jvm, etc.)"
            value={config.java_path ?? ""}
            oninput={(e) => {
              const val = (e.currentTarget as HTMLInputElement).value.trim();
              config.java_path = val.length > 0 ? val : null;
            }}
            class="flex-1 bg-[#1b1b1b] border border-[#444] px-2.5 py-1.5 text-white font-mc-mono text-sm"
          />
          <button
            type="button"
            onclick={browseJavaPath}
            class="mc-btn px-3 py-1.5 text-xs"
          >
            BROWSE
          </button>
          <button
            type="button"
            onclick={() => (config.java_path = null)}
            class="mc-btn px-2.5 py-1.5 text-xs {!config.java_path ? 'mc-btn-green' : ''}"
          >
            AUTO SYSTEM JDK
          </button>
        </div>

        {#if detectedJavas.length > 0}
          <div class="mt-2">
            <div class="text-xs text-[#aaa] mb-1">
              Detected System JDKs (click to pin, or leave on AUTO to match MC version):
            </div>
            <div class="flex flex-wrap gap-1.5">
              {#each detectedJavas as dj}
                <button
                  type="button"
                  onclick={() => (config.java_path = dj.path)}
                  class="mc-btn px-2.5 py-1 text-xs {config.java_path === dj.path
                    ? 'mc-btn-green'
                    : ''}"
                  title={dj.path}
                >
                  {dj.label}
                </button>
              {/each}
            </div>
          </div>
        {/if}
      </div>

      <div>
        <label for="jvm-args-input" class="font-bold text-[#b0bec5] block mb-1 text-xs">
          CUSTOM JVM ARGUMENTS
        </label>
        <input
          id="jvm-args-input"
          type="text"
          bind:value={config.jvm_args}
          class="w-full bg-[#1b1b1b] border border-[#444] px-2.5 py-1.5 text-white font-mc-mono text-sm"
        />
      </div>
    </div>

    <!-- INSTANCE DIRECTORY -->
    <div class="mc-inset p-3.5 space-y-2">
      <label for="instance-dir-input" class="font-bold text-[#4fc3f7] block text-xs">
        MODPACK INSTANCE DIRECTORY
      </label>
      <div class="flex gap-2">
        <input
          id="instance-dir-input"
          type="text"
          bind:value={config.instance_dir}
          class="flex-1 bg-[#1b1b1b] border border-[#444] px-2.5 py-1.5 text-white font-mc-mono text-sm"
        />
        <button
          type="button"
          onclick={browseInstanceFolder}
          class="mc-btn px-3 py-1.5 text-xs"
        >
          BROWSE
        </button>
      </div>
    </div>

    <!-- DISPLAY & LIFECYCLE -->
    <div class="mc-inset p-3.5 grid grid-cols-2 gap-4 text-xs">
      <div class="space-y-2">
        <div class="font-bold text-[#ce93d8]">GAME RESOLUTION</div>
        <div class="flex items-center gap-2">
          <input
            type="number"
            bind:value={config.resolution_width}
            aria-label="Resolution Width"
            class="w-24 bg-[#1b1b1b] border border-[#444] px-2 py-1 text-white"
          />
          <span>x</span>
          <input
            type="number"
            bind:value={config.resolution_height}
            aria-label="Resolution Height"
            class="w-24 bg-[#1b1b1b] border border-[#444] px-2 py-1 text-white"
          />
        </div>
        <label class="flex items-center gap-2 pt-1 cursor-pointer">
          <input type="checkbox" bind:checked={config.fullscreen} />
          <span>Launch in Fullscreen mode (syncs options.txt)</span>
        </label>
      </div>

      <div class="space-y-2">
        <label for="close-behavior-select" class="font-bold text-[#ce93d8] block">
          ON GAME LAUNCH BEHAVIOR
        </label>
        <select
          id="close-behavior-select"
          bind:value={config.close_behavior}
          class="w-full bg-[#1b1b1b] border border-[#444] px-2 py-1.5 text-white text-xs"
        >
          <option value="hide_and_reopen">Hide launcher & reopen on exit</option>
          <option value="keep_open">Keep launcher open</option>
          <option value="close_launcher">Close launcher completely</option>
        </select>

        <label class="flex items-center gap-2 pt-1 cursor-pointer">
          <input type="checkbox" bind:checked={config.discord_rpc_enabled} />
          <span>Enable Discord Rich Presence (RPC)</span>
        </label>
      </div>
    </div>

    <!-- ADVANCED: AZURE CLIENT ID -->
    <div class="mc-inset p-3.5 space-y-3">
      <div>
        <label for="azure-client-input" class="font-bold text-[#80cbc4] block mb-1 text-xs">
          MICROSOFT OAUTH CLIENT ID (DEFAULT: 00000000441cc96b PRE-APPROVED)
        </label>
        <div class="flex gap-2">
          <input
            id="azure-client-input"
            type="text"
            bind:value={config.azure_client_id}
            class="flex-1 bg-[#1b1b1b] border border-[#444] px-2.5 py-1.5 text-white font-mc-mono text-sm"
          />
          <button
            type="button"
            onclick={() => (config.azure_client_id = "00000000441cc96b")}
            class="mc-btn px-3 py-1.5 text-xs"
            title="Reset to the official pre-whitelisted Minecraft Client ID"
          >
            DEFAULT
          </button>
        </div>
      </div>
    </div>
  </div>

  <div class="mt-4 pt-3 border-t-2 border-[#181818] flex justify-end gap-3">
    <form method="dialog">
      <button type="submit" class="mc-btn px-4 py-2 text-xs">CANCEL</button>
    </form>
    <button
      type="button"
      onclick={handleSaveSettings}
      class="mc-btn mc-btn-green px-5 py-2 text-xs font-bold"
    >
      SAVE SETTINGS
    </button>
  </div>
</dialog>

<!-- ========================================================================= -->
<!-- NATIVE MODAL 5: REAL-TIME GAME CONSOLE / CRASH LOG VIEWER                 -->
<!-- ========================================================================= -->
<dialog
  bind:this={consoleDialog}
  {...({ closedby: "any" } as any)}
  aria-labelledby="console-dialog-title"
  class="mc-panel w-[760px] p-5 text-white"
>
  <div class="flex items-center justify-between border-b-2 border-[#181818] pb-3 mb-3">
    <h2 id="console-dialog-title" class="font-mc-title text-base text-[#4fc3f7]">
      REAL-TIME GAME LOG CONSOLE
    </h2>
    <div class="flex items-center gap-2">
      {#each ["ALL", "INFO", "WARN", "ERROR"] as lvl}
        <button
          type="button"
          onclick={() => (logFilter = lvl as any)}
          class="mc-btn px-2.5 py-1 text-xs {logFilter === lvl
            ? 'mc-btn-green'
            : ''}"
        >
          {lvl}
        </button>
      {/each}
      <form method="dialog">
        <button type="submit" class="mc-btn px-2.5 py-1 text-xs ml-2">X</button>
      </form>
    </div>
  </div>

  <div
    class="mc-inset p-3 h-[360px] overflow-y-auto font-mc-mono text-xs space-y-1 select-text"
  >
    {#if filteredLogs.length === 0}
      <div class="text-[#777] text-center py-12 text-sm">
        No game logs recorded yet. Launch the modpack to stream live JVM stdout/stderr.
      </div>
    {:else}
      {#each filteredLogs as entry}
        <div
          class="leading-relaxed break-all {entry.level === 'ERROR'
            ? 'text-[#ff8a80]'
            : entry.level === 'WARN'
              ? 'text-[#ffd54f]'
              : 'text-[#d0d0d0]'}"
        >
          <span class="text-[#777]">[{entry.timestamp}]</span>
          <span class="font-bold">[{entry.level}]</span>
          <span>{entry.line}</span>
        </div>
      {/each}
    {/if}
  </div>

  <div class="mt-4 pt-3 border-t-2 border-[#181818] flex justify-between items-center">
    <div class="flex gap-2">
      <button
        type="button"
        onclick={copyAllLogs}
        class="mc-btn px-3.5 py-1.5 text-xs"
      >
        {logsCopied ? "COPIED TO CLIPBOARD!" : "COPY ALL LOGS"}
      </button>
      <button
        type="button"
        onclick={clearLogs}
        class="mc-btn mc-btn-red px-3.5 py-1.5 text-xs"
      >
        CLEAR
      </button>
    </div>
    <form method="dialog">
      <!-- svelte-ignore a11y_autofocus -->
      <button type="submit" class="mc-btn px-4 py-1.5 text-xs" autofocus>
        CLOSE
      </button>
    </form>
  </div>
</dialog>
