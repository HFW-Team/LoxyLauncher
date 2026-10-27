use crate::config::{MinecraftAccount, DEFAULT_AZURE_CLIENT_ID};
use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceCodeResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
    #[serde(default)]
    pub message: String,
}

#[derive(Debug, Deserialize)]
struct MsTokenSuccess {
    access_token: String,
    refresh_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MsTokenError {
    error: String,
    error_description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct XblResponse {
    #[serde(rename = "Token")]
    token: String,
    #[serde(rename = "DisplayClaims")]
    display_claims: XblDisplayClaims,
}

#[derive(Debug, Deserialize)]
struct XblDisplayClaims {
    xui: Vec<XblXui>,
}

#[derive(Debug, Deserialize)]
struct XblXui {
    uhs: String,
}

#[derive(Debug, Deserialize)]
struct McLoginResponse {
    access_token: String,
    expires_in: i64,
}

#[derive(Debug, Deserialize)]
struct McProfileResponse {
    id: String,
    name: String,
    skins: Option<Vec<McSkin>>,
}

#[derive(Debug, Deserialize)]
struct McSkin {
    url: String,
    state: Option<String>,
}

/// Returns true if the client_id uses Microsoft Live's legacy MBI_SSL flow
/// (e.g., official whitelisted Minecraft client IDs like `00000000441cc96b` or `00000000402b5328`).
fn uses_live_mbi_ssl_flow(client_id: &str) -> bool {
    !client_id.contains('-')
}

fn effective_client_id(client_id: &str) -> &str {
    let trimmed = client_id.trim();
    if trimmed.is_empty() || trimmed == "00000000-0000-0000-0000-000000000000" {
        DEFAULT_AZURE_CLIENT_ID
    } else {
        trimmed
    }
}

pub async fn request_device_code(client: &Client, client_id: &str) -> Result<DeviceCodeResponse> {
    let cid = effective_client_id(client_id);

    let resp = if uses_live_mbi_ssl_flow(cid) {
        let params = [
            ("client_id", cid),
            ("scope", "service::user.auth.xboxlive.com::MBI_SSL"),
            ("response_type", "device_code"),
        ];
        client
            .post("https://login.live.com/oauth20_connect.srf")
            .form(&params)
            .send()
            .await
            .context("Failed to contact login.live.com devicecode endpoint")?
    } else {
        let params = [
            ("client_id", cid),
            ("scope", "XboxLive.signin offline_access"),
        ];
        client
            .post("https://login.microsoftonline.com/consumers/oauth2/v2.0/devicecode")
            .form(&params)
            .send()
            .await
            .context("Failed to contact Microsoft OAuth devicecode endpoint")?
    };

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        bail!("Microsoft Device Code request failed ({}): {}", status, body);
    }

    let mut data: DeviceCodeResponse = resp
        .json()
        .await
        .context("Invalid JSON from Microsoft devicecode endpoint")?;

    if data.message.is_empty() {
        data.message = format!(
            "Go to {} and enter code {}",
            data.verification_uri, data.user_code
        );
    }

    Ok(data)
}

pub async fn poll_for_microsoft_token(
    client: &Client,
    client_id: &str,
    device_code: &str,
    interval_secs: u64,
    expires_in_secs: u64,
) -> Result<MinecraftAccount> {
    let cid = effective_client_id(client_id);
    let is_live_flow = uses_live_mbi_ssl_flow(cid);
    let mut poll_interval = interval_secs.max(3);
    let deadline = Utc::now().timestamp() + expires_in_secs as i64;

    loop {
        if Utc::now().timestamp() >= deadline {
            bail!("Device code expired before sign-in was completed. Please try again.");
        }

        tokio::time::sleep(Duration::from_secs(poll_interval)).await;

        let params = [
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("client_id", cid),
            ("device_code", device_code),
        ];

        let token_url = if is_live_flow {
            format!("https://login.live.com/oauth20_token.srf?client_id={}", cid)
        } else {
            "https://login.microsoftonline.com/consumers/oauth2/v2.0/token".to_string()
        };

        let resp = client
            .post(&token_url)
            .form(&params)
            .send()
            .await
            .context("Failed to poll Microsoft token endpoint")?;

        if resp.status().is_success() {
            let token_data: MsTokenSuccess = resp
                .json()
                .await
                .context("Failed to parse Microsoft token response")?;
            let refresh = token_data.refresh_token.unwrap_or_default();
            return exchange_ms_token_for_minecraft(
                client,
                &token_data.access_token,
                &refresh,
                is_live_flow,
            )
            .await;
        } else {
            let body = resp.text().await.unwrap_or_default();
            if let Ok(err) = serde_json::from_str::<MsTokenError>(&body) {
                match err.error.as_str() {
                    "authorization_pending" => continue,
                    "slow_down" => {
                        poll_interval += 5;
                        continue;
                    }
                    "authorization_declined" => {
                        bail!("Sign-in request was declined by the user.");
                    }
                    "expired_token" => {
                        bail!("The verification code has expired. Please start sign-in again.");
                    }
                    other => {
                        let desc = err.error_description.unwrap_or_else(|| other.to_string());
                        bail!("Microsoft authentication error: {}", desc);
                    }
                }
            } else {
                bail!("Unexpected Microsoft token error: {}", body);
            }
        }
    }
}

pub async fn refresh_minecraft_session(
    client: &Client,
    client_id: &str,
    refresh_token: &str,
) -> Result<MinecraftAccount> {
    let cid = effective_client_id(client_id);
    let is_live_flow = uses_live_mbi_ssl_flow(cid);

    let (token_url, scope) = if is_live_flow {
        (
            "https://login.live.com/oauth20_token.srf",
            "service::user.auth.xboxlive.com::MBI_SSL",
        )
    } else {
        (
            "https://login.microsoftonline.com/consumers/oauth2/v2.0/token",
            "XboxLive.signin offline_access",
        )
    };

    let params = [
        ("client_id", cid),
        ("refresh_token", refresh_token),
        ("grant_type", "refresh_token"),
        ("scope", scope),
    ];

    let resp = client
        .post(token_url)
        .form(&params)
        .send()
        .await
        .context("Failed to refresh Microsoft OAuth token")?;

    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        bail!("Failed to refresh Microsoft session: {}", body);
    }

    let token_data: MsTokenSuccess = resp.json().await?;
    let new_refresh = token_data
        .refresh_token
        .unwrap_or_else(|| refresh_token.to_string());

    exchange_ms_token_for_minecraft(client, &token_data.access_token, &new_refresh, is_live_flow)
        .await
}

async fn exchange_ms_token_for_minecraft(
    client: &Client,
    ms_access_token: &str,
    ms_refresh_token: &str,
    is_live_flow: bool,
) -> Result<MinecraftAccount> {
    // When using the official Live MBI_SSL flow (00000000441cc96b), RpsTicket is the raw token.
    // When using a custom Azure Entra app, RpsTicket requires the `d=` prefix.
    let rps_ticket = if is_live_flow {
        ms_access_token.to_string()
    } else {
        format!("d={}", ms_access_token)
    };

    // 1. Authenticate with Xbox Live
    let xbl_payload = json!({
        "Properties": {
            "AuthMethod": "RPS",
            "SiteName": "user.auth.xboxlive.com",
            "RpsTicket": rps_ticket
        },
        "RelyingParty": "http://auth.xboxlive.com",
        "TokenType": "JWT"
    });

    let xbl_resp = client
        .post("https://user.auth.xboxlive.com/user/authenticate")
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .header("x-xbl-contract-version", "1")
        .json(&xbl_payload)
        .send()
        .await
        .context("Failed to authenticate with Xbox Live")?;

    if !xbl_resp.status().is_success() {
        let body = xbl_resp.text().await.unwrap_or_default();
        bail!("Xbox Live authentication failed: {}", body);
    }

    let xbl_data: XblResponse = xbl_resp
        .json()
        .await
        .context("Failed to parse Xbox Live token response")?;
    let uhs = xbl_data
        .display_claims
        .xui
        .first()
        .map(|x| x.uhs.clone())
        .ok_or_else(|| anyhow!("Missing user hash (uhs) in Xbox Live response"))?;

    // 2. Authorize with XSTS for Minecraft Services
    let xsts_payload = json!({
        "Properties": {
            "SandboxId": "RETAIL",
            "UserTokens": [xbl_data.token]
        },
        "RelyingParty": "rp://api.minecraftservices.com/",
        "TokenType": "JWT"
    });

    let xsts_resp = client
        .post("https://xsts.auth.xboxlive.com/xsts/authorize")
        .header("Accept", "application/json")
        .json(&xsts_payload)
        .send()
        .await
        .context("Failed to authorize with XSTS")?;

    if !xsts_resp.status().is_success() {
        let body = xsts_resp.text().await.unwrap_or_default();
        if body.contains("2148916233") {
            bail!("This Microsoft account does not have an Xbox Live profile. Please sign in to xbox.com once to create one.");
        } else if body.contains("2148916238") {
            bail!("This account belongs to a minor and must be added to a Microsoft Family group with multiplayer permissions enabled.");
        }
        bail!("XSTS authorization failed: {}", body);
    }

    let xsts_data: XblResponse = xsts_resp
        .json()
        .await
        .context("Failed to parse XSTS response")?;

    // 3. Authenticate with Minecraft Services
    let mc_payload = json!({
        "identityToken": format!("XBL3.0 x={};{}", uhs, xsts_data.token)
    });

    let mc_resp = client
        .post("https://api.minecraftservices.com/authentication/login_with_xbox")
        .header("Accept", "application/json")
        .json(&mc_payload)
        .send()
        .await
        .context("Failed to login to Minecraft Services")?;

    if !mc_resp.status().is_success() {
        let body = mc_resp.text().await.unwrap_or_default();
        if body.contains("Invalid app registration") {
            bail!(
                "Minecraft Services rejected your custom Azure Client ID because Microsoft requires manual partner approval (aka.ms/AppRegInfo). In Settings, click 'DEFAULT' next to Client ID to use the pre-approved Minecraft Client ID (00000000441cc96b)."
            );
        }
        bail!("Minecraft Services login failed: {}", body);
    }

    let mc_login: McLoginResponse = mc_resp
        .json()
        .await
        .context("Failed to parse Minecraft Services login response")?;

    // 4. Fetch Minecraft Profile (verifies game ownership and gets UUID + username + skin)
    let profile_resp = client
        .get("https://api.minecraftservices.com/minecraft/profile")
        .bearer_auth(&mc_login.access_token)
        .send()
        .await
        .context("Failed to query Minecraft profile")?;

    if profile_resp.status().as_u16() == 404 {
        bail!("This Microsoft account does not own Minecraft: Java Edition.");
    }
    if !profile_resp.status().is_success() {
        let body = profile_resp.text().await.unwrap_or_default();
        bail!("Failed to fetch Minecraft profile: {}", body);
    }

    let profile: McProfileResponse = profile_resp
        .json()
        .await
        .context("Failed to parse Minecraft profile JSON")?;

    let skin_url = profile.skins.and_then(|skins| {
        skins
            .into_iter()
            .find(|s| s.state.as_deref() == Some("ACTIVE"))
            .map(|s| s.url)
    });

    let expires_at = Utc::now().timestamp() + mc_login.expires_in - 60;

    Ok(MinecraftAccount {
        uuid: profile.id,
        username: profile.name,
        access_token: mc_login.access_token,
        refresh_token: ms_refresh_token.to_string(),
        expires_at,
        skin_url,
    })
}
