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
    InitialStateResponse,
    DashboardStatus,
    ManifestCheckResult,
    ServerStatus,
    DeviceCodeResponse,
    SyncProgressEvent,
    GameLogEntry,
    GameExitEvent,
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
    manifest_url:
      "https://raw.githubusercontent.com/loxy-community/modpack-manifest/main/launcher-manifest.json",
    azure_client_id: "00000000441cc96b",
    account: null,
  });

  let systemMemory = $state<SystemMemoryInfo>({
    total_ram_mb: 16384,
    recommended_max_mb: 6144,
  });

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

  // Updater State
  let updaterState = $state<string>("v0.1.0");

  // Native <dialog> Element Refs
  let authDialog: HTMLDialogElement | undefined = $state();
  let settingsDialog: HTMLDialogElement | undefined = $state();
  let consoleDialog: HTMLDialogElement | undefined = $state();

  let filteredLogs = $derived(
    logFilter === "ALL" ? logs : logs.filter((l) => l.level === logFilter)
  );

  let errorCount = $derived(logs.filter((l) => l.level === "ERROR").length);

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

    (async () => {
      try {
        const init = await invoke<InitialStateResponse>("get_initial_state");
        config = init.config;
        systemMemory = init.system_memory;

        const existingLogs = await invoke<GameLogEntry[]>("get_game_logs");
        logs = existingLogs;

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

    const unlistenLog = listen<GameLogEntry>("game-log", (ev) => {
      logs = [...logs.slice(-2499), ev.payload];
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
      statusBanner = {
        type: "info",
        text: "Settings saved. Refreshing modpack manifest...",
      };
      await refreshManifestAndServer();
    } catch (err) {
      statusBanner = {
        type: "error",
        text: `Failed to save settings: ${String(err)}`,
      };
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
          <h1 class="font-mc-title text-base tracking-wider text-[#66bb6a]">
            LOXYLAUNCHER
          </h1>
          <button
            type="button"
            onclick={checkForLauncherUpdate}
            class="px-2 py-0.5 text-[11px] bg-[#1d1d1d] border border-[#444] text-[#aaa] hover:text-white cursor-pointer"
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
    <div class="flex items-center gap-3">
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
            <div class="text-[10px] text-[#888]">Microsoft Account</div>
          </div>
          <button
            type="button"
            onclick={handleLogout}
            class="mc-btn mc-btn-red px-2 py-1 text-[10px] ml-1"
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
        onclick={() => consoleDialog?.showModal()}
        class="mc-btn px-3 py-2 text-xs"
      >
        <span>CONSOLE</span>
        {#if errorCount > 0}
          <span class="px-1.5 py-0.2 bg-[#d32f2f] text-white text-[10px]">
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
            <span class="text-[11px] uppercase tracking-widest text-[#81c784]">
              Featured Private Pack
            </span>
            <h2 class="font-mc-title text-xl text-white mt-1">
              {manifestCheck.manifest.name}
            </h2>
          </div>

          <button
            type="button"
            onclick={refreshManifestAndServer}
            disabled={isRefreshing || isLaunching}
            class="mc-btn px-2.5 py-1 text-xs"
          >
            {isRefreshing ? "SYNCING..." : "CHECK UPDATES"}
          </button>
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
          <span class="text-[11px] text-[#777]">
            SHA-1 / SHA-256 + .mrpack Verified
          </span>
        </div>

        <div class="mc-inset p-3.5 flex-1 overflow-y-auto text-xs leading-relaxed text-[#d0d0d0] space-y-2">
          <p>
            {manifestCheck.manifest.news ??
              "No announcement posted in the remote manifest."}
          </p>
          <div class="pt-3 border-t border-[#262626] text-[11px] text-[#888] space-y-1">
            <div>
              <strong class="text-[#aaa]">Remote Source:</strong>
              {manifestCheck.manifest.mrpack_url
                ? "Modrinth .mrpack + Remote JSON Manifest"
                : "Remote JSON Hash Manifest"}
            </div>
            <div>
              <strong class="text-[#aaa]">Tracked Custom Overrides:</strong>
              {manifestCheck.manifest.files.length} file(s)
            </div>
            <div class="truncate" title={config.instance_dir}>
              <strong class="text-[#aaa]">Instance Path:</strong>
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
            class="mc-btn px-2 py-0.5 text-[11px]"
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
            <span class="text-[#81c784] text-[11px]">
              {serverStatus.latency_ms != null ? `${serverStatus.latency_ms} ms` : "---"}
            </span>
          </div>

          <p class="font-mc-mono text-base text-[#bdbdbd] leading-snug">
            {serverStatus.motd}
          </p>

          <!-- Player Count Bar -->
          <div class="pt-1">
            <div class="flex justify-between text-[11px] text-[#aaa] mb-1">
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

        <div class="grid grid-cols-2 gap-2 text-xs my-auto">
          <div class="mc-inset p-2.5">
            <div class="text-[10px] text-[#888]">ALLOCATED RAM</div>
            <div class="text-sm text-[#66bb6a] font-bold mt-0.5">
              {config.max_ram_mb} MB
            </div>
          </div>

          <div class="mc-inset p-2.5">
            <div class="text-[10px] text-[#888]">JAVA RUNTIME</div>
            <div class="text-sm text-[#4fc3f7] font-bold mt-0.5">
              {config.java_path ? "Custom JRE" : "Auto Adoptium"}
            </div>
          </div>

          <div class="mc-inset p-2.5">
            <div class="text-[10px] text-[#888]">DISPLAY</div>
            <div class="text-sm text-white font-bold mt-0.5">
              {config.fullscreen
                ? "Fullscreen"
                : `${config.resolution_width}x${config.resolution_height}`}
            </div>
          </div>

          <div class="mc-inset p-2.5">
            <div class="text-[10px] text-[#888]">DISCORD RPC</div>
            <div class="text-sm text-[#ce93d8] font-bold mt-0.5">
              {config.discord_rpc_enabled ? "ACTIVE" : "DISABLED"}
            </div>
          </div>
        </div>
      </section>
    </div>
  </main>

  <!-- BOTTOM DIRT LAUNCH DECK -->
  <footer class="mc-dirt-bar px-6 py-4 flex items-center justify-between gap-6">
    <!-- Sync / Hash Progress Status -->
    <div class="flex-1">
      {#if syncProgress}
        <div class="flex justify-between text-xs mb-1.5">
          <span class="text-[#a5d6a7] font-bold">{syncProgress.stage}</span>
          <span class="text-white">{syncProgress.percent}%</span>
        </div>
        <div class="w-full h-5 mc-inset p-0.5">
          <div
            class="h-full bg-[#43a047] transition-all duration-150"
            style={`width: ${syncProgress.percent}%`}
          ></div>
        </div>
        <div class="text-[11px] text-[#bdbdbd] mt-1 truncate">
          {syncProgress.current_file} ({syncProgress.completed}/{syncProgress.total})
        </div>
      {:else}
        <div class="flex items-center gap-3">
          <div class="w-3 h-3 bg-[#66bb6a] border border-black"></div>
          <div>
            <div class="text-xs font-bold text-white">
              {manifestCheck.update_available
                ? `Update v${manifestCheck.manifest.version} Ready to Sync`
                : `${manifestCheck.manifest.name} Ready to Launch`}
            </div>
            <div class="text-[11px] text-[#aaa]">
              {config.account
                ? `Authenticated as ${config.account.username} • Auto-sync enabled`
                : "Sign in with Microsoft OAuth to launch"}
            </div>
          </div>
        </div>
      {/if}
    </div>

    <!-- Giant Minecraft Play Button -->
    <button
      type="button"
      onclick={handlePlay}
      disabled={isLaunching}
      class="mc-btn mc-btn-green px-10 py-4 text-sm font-mc-title tracking-wider shrink-0"
    >
      {#if isLaunching}
        <span>LAUNCHING...</span>
      {:else if !config.account}
        <span>SIGN IN & PLAY</span>
      {:else if manifestCheck.update_available}
        <span>UPDATE & PLAY</span>
      {:else}
        <span>PLAY MODPACK</span>
      {/if}
    </button>
  </footer>
</div>

<!-- ========================================================================= -->
<!-- NATIVE MODAL 1: MICROSOFT DEVICE CODE OAUTH DIALOG                        -->
<!-- ========================================================================= -->
<dialog
  bind:this={authDialog}
  {...({ closedby: "any" } as any)}
  aria-labelledby="auth-dialog-title"
  class="mc-panel w-[500px] p-5 text-white"
>
  <div class="flex items-center justify-between border-b-2 border-[#181818] pb-3 mb-4">
    <h2 id="auth-dialog-title" class="font-mc-title text-sm text-[#ffd54f]">
      MICROSOFT ACCOUNT SIGN IN
    </h2>
    <form method="dialog">
      <button type="submit" class="mc-btn px-2.5 py-1 text-xs">X</button>
    </form>
  </div>

  <div class="space-y-4 text-xs">
    <p class="text-[#d0d0d0] leading-relaxed">
      LoxyLauncher uses official Microsoft Device Code authentication. Enter the
      code below at <strong class="text-[#4fc3f7]">microsoft.com/link</strong> to
      sign in safely in your default browser:
    </p>

    {#if deviceCodeInfo}
      <div class="mc-inset p-4 text-center space-y-2">
        <div class="text-[11px] text-[#aaa]">YOUR VERIFICATION CODE</div>
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
        <div class="flex items-center justify-center gap-2 text-[#81c784] py-2">
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
      <div class="mc-inset p-3.5 border border-[#ef5350] text-[#ffcdd2] space-y-2">
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
            CONFIGURE CLIENT ID IN SETTINGS
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
<!-- NATIVE MODAL 2: LAUNCHER SETTINGS DIALOG                                  -->
<!-- ========================================================================= -->
<dialog
  bind:this={settingsDialog}
  {...({ closedby: "any" } as any)}
  aria-labelledby="settings-dialog-title"
  class="mc-panel w-[680px] p-5 text-white"
>
  <div class="flex items-center justify-between border-b-2 border-[#181818] pb-3 mb-4">
    <h2 id="settings-dialog-title" class="font-mc-title text-sm text-[#66bb6a]">
      LAUNCHER & MODPACK SETTINGS
    </h2>
    <form method="dialog">
      <button type="submit" class="mc-btn px-2.5 py-1 text-xs">X</button>
    </form>
  </div>

  <div class="space-y-4 overflow-y-auto max-h-[68vh] pr-1 text-xs">
    <!-- RAM ALLOCATION -->
    <div class="mc-inset p-3.5 space-y-2">
      <div class="flex justify-between items-center">
        <label for="max-ram-slider" class="font-bold text-[#a5d6a7]">
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
      <div class="flex justify-between text-[10px] text-[#888]">
        <span>Min: 2048 MB</span>
        <span>Recommended: {systemMemory.recommended_max_mb} MB</span>
        <span>System Total: {systemMemory.total_ram_mb} MB</span>
      </div>
    </div>

    <!-- INSTANCE DIRECTORY -->
    <div class="mc-inset p-3.5 space-y-2">
      <label for="instance-dir-input" class="font-bold text-[#4fc3f7] block">
        MODPACK INSTANCE DIRECTORY
      </label>
      <div class="flex gap-2">
        <input
          id="instance-dir-input"
          type="text"
          bind:value={config.instance_dir}
          class="flex-1 bg-[#1b1b1b] border border-[#444] px-2.5 py-1.5 text-xs text-white font-mc-mono text-base"
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

    <!-- JAVA RUNTIME & JVM FLAGS -->
    <div class="mc-inset p-3.5 space-y-3">
      <div>
        <label for="java-path-input" class="font-bold text-[#ffd54f] block mb-1">
          JAVA RUNTIME EXECUTABLE (LEAVE EMPTY FOR AUTO ADOPTIUM JRE)
        </label>
        <div class="flex gap-2">
          <input
            id="java-path-input"
            type="text"
            placeholder="Auto-download matching Java 8 / 17 / 21 via Adoptium API"
            value={config.java_path ?? ""}
            oninput={(e) => {
              const val = (e.currentTarget as HTMLInputElement).value.trim();
              config.java_path = val.length > 0 ? val : null;
            }}
            class="flex-1 bg-[#1b1b1b] border border-[#444] px-2.5 py-1.5 text-white font-mc-mono text-base"
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
            class="mc-btn px-2.5 py-1.5 text-xs"
          >
            AUTO
          </button>
        </div>
      </div>

      <div>
        <label for="jvm-args-input" class="font-bold text-[#b0bec5] block mb-1">
          CUSTOM JVM ARGUMENTS
        </label>
        <input
          id="jvm-args-input"
          type="text"
          bind:value={config.jvm_args}
          class="w-full bg-[#1b1b1b] border border-[#444] px-2.5 py-1.5 text-white font-mc-mono text-base"
        />
      </div>
    </div>

    <!-- DISPLAY & LIFECYCLE -->
    <div class="mc-inset p-3.5 grid grid-cols-2 gap-4">
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
          <span>Launch in Fullscreen mode</span>
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

    <!-- ADVANCED: MANIFEST URL & AZURE CLIENT ID -->
    <div class="mc-inset p-3.5 space-y-3">
      <div>
        <label for="manifest-url-input" class="font-bold text-[#80cbc4] block mb-1">
          REMOTE MANIFEST JSON URL (GITHUB / CDN)
        </label>
        <input
          id="manifest-url-input"
          type="url"
          bind:value={config.manifest_url}
          class="w-full bg-[#1b1b1b] border border-[#444] px-2.5 py-1.5 text-white font-mc-mono text-base"
        />
      </div>

      <div>
        <label for="azure-client-input" class="font-bold text-[#80cbc4] block mb-1">
          MICROSOFT OAUTH CLIENT ID (DEFAULT: 00000000441cc96b PRE-APPROVED)
        </label>
        <div class="flex gap-2">
          <input
            id="azure-client-input"
            type="text"
            bind:value={config.azure_client_id}
            class="flex-1 bg-[#1b1b1b] border border-[#444] px-2.5 py-1.5 text-white font-mc-mono text-base"
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
<!-- NATIVE MODAL 3: REAL-TIME GAME CONSOLE / CRASH LOG VIEWER                 -->
<!-- ========================================================================= -->
<dialog
  bind:this={consoleDialog}
  {...({ closedby: "any" } as any)}
  aria-labelledby="console-dialog-title"
  class="mc-panel w-[760px] p-5 text-white"
>
  <div class="flex items-center justify-between border-b-2 border-[#181818] pb-3 mb-3">
    <h2 id="console-dialog-title" class="font-mc-title text-sm text-[#4fc3f7]">
      REAL-TIME GAME LOG CONSOLE
    </h2>
    <div class="flex items-center gap-2">
      {#each ["ALL", "INFO", "WARN", "ERROR"] as lvl}
        <button
          type="button"
          onclick={() => (logFilter = lvl as any)}
          class="mc-btn px-2.5 py-1 text-[10px] {logFilter === lvl
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
    class="mc-inset p-3 h-[360px] overflow-y-auto font-mc-mono text-base space-y-1 select-text"
  >
    {#if filteredLogs.length === 0}
      <div class="text-[#777] text-center py-12">
        No game logs recorded yet. Launch the modpack to stream live JVM stdout/stderr.
      </div>
    {:else}
      {#each filteredLogs as entry}
        <div
          class="leading-tight break-all {entry.level === 'ERROR'
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
