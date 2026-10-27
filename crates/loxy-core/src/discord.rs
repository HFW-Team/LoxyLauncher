use discord_rich_presence::{activity, DiscordIpc, DiscordIpcClient};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// Default Discord Application ID for LoxyLauncher Rich Presence.
/// Can be overridden via `LOXY_DISCORD_CLIENT_ID` at compile time.
pub const DEFAULT_DISCORD_APP_ID: &str = match option_env!("LOXY_DISCORD_CLIENT_ID") {
    Some(id) => id,
    None => "1200000000000000000",
};

pub struct DiscordRpcManager {
    client: Mutex<Option<DiscordIpcClient>>,
    start_timestamp: i64,
}

impl DiscordRpcManager {
    pub fn new() -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        Self {
            client: Mutex::new(None),
            start_timestamp: now,
        }
    }

    pub fn update_status(&self, enabled: bool, details: &str, state_text: &str) {
        let mut guard = match self.client.lock() {
            Ok(g) => g,
            Err(_) => return,
        };

        if !enabled {
            if let Some(mut c) = guard.take() {
                let _ = c.clear_activity();
                let _ = c.close();
            }
            return;
        }

        if guard.is_none() {
            if let Ok(mut new_client) = DiscordIpcClient::new(DEFAULT_DISCORD_APP_ID) {
                if new_client.connect().is_ok() {
                    *guard = Some(new_client);
                }
            }
        }

        if let Some(client) = guard.as_mut() {
            let act = activity::Activity::new()
                .details(details)
                .state(state_text)
                .timestamps(activity::Timestamps::new().start(self.start_timestamp));
            if client.set_activity(act).is_err() {
                let _ = client.close();
                *guard = None;
            }
        }
    }
}
