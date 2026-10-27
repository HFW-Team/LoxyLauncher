# LoxyLauncher

Lightweight, dedicated Minecraft launcher for private modpack launching built with **Rust + Tauri v2** and **Svelte 5 + Tailwind CSS**, featuring an authentic retro Minecraft pixel-art UI.

## Key Features

- **Microsoft OAuth Device Code Authentication**: Official Microsoft -> Xbox Live -> XSTS -> Minecraft Services token chain via firewall-friendly Device Code Flow (`microsoft.com/link`).
- **Differential `.mrpack` & Remote JSON Hash Sync**: Fetches remote JSON manifests and Modrinth `.mrpack` archives from GitHub Releases or Modrinth CDN, verifying every file with `SHA-1` / `SHA-256` / `SHA-512` hashes and pruning obsolete mods.
- **Dynamic Mod Loader Support**: Supports **Fabric**, **NeoForge**, **Forge**, and **Vanilla** dynamically parsed from the remote manifest or `modrinth.index.json`.
- **Automatic Java Runtime (JRE) Resolution**: Automatically detects or downloads the matching Eclipse Temurin (Adoptium) Java 8, 17, or 21 runtime for the target Minecraft version, with custom Java path override support.
- **Live Server Status Ping**: Native TCP Minecraft Server List Ping (`0x00`) displaying online status, player count, latency (`ms`), and server MOTD.
- **Real-Time Game Log Console**: Streams JVM `stdout`/`stderr` live with `INFO`/`WARN`/`ERROR` filtering and one-click crash log copying.
- **Discord Rich Presence (RPC)**: Displays current modpack, server status, and active player session on Discord.

## Setting Up Microsoft OAuth (Azure App Client ID)

1. Go to the [Microsoft Entra (Azure) Portal — App Registrations](https://portal.azure.com/#view/Microsoft_AAD_RegisteredApps/ApplicationsListBlade).
2. Click **New registration**:
   - **Name**: `LoxyLauncher`
   - **Supported account types**: *Personal Microsoft accounts only* (`consumers`).
   - **Redirect URI**: Select *Public client/native (mobile & desktop)* and enter `https://login.microsoftonline.com/common/oauth2/nativeclient`.
3. Under **Authentication** -> **Advanced settings**, set **Allow public client flows** to **Yes** (enables Device Code Flow) and click **Save**.
4. Copy the **Application (client) ID** and either:
   - Set it at compile time via environment variable: `LOXY_AZURE_CLIENT_ID="your-client-id" pnpm tauri build`
   - Or paste it directly inside LoxyLauncher under **Settings -> Microsoft Azure App Client ID**.

## Development & Packaging

```bash
# Install frontend dependencies
pnpm install

# Run in development mode
pnpm tauri dev

# Build production installers (.AppImage / .deb / .msi / .dmg)
pnpm tauri build
```
