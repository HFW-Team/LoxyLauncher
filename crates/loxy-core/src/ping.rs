use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerStatus {
    pub online: bool,
    pub host: String,
    pub port: u16,
    pub players_online: u32,
    pub players_max: u32,
    pub motd: String,
    pub version: Option<String>,
    pub latency_ms: Option<u64>,
}

fn write_varint(mut value: i32, buf: &mut Vec<u8>) {
    loop {
        if (value & !0x7F) == 0 {
            buf.push(value as u8);
            return;
        }
        buf.push(((value & 0x7F) | 0x80) as u8);
        value = ((value as u32) >> 7) as i32;
    }
}

async fn read_varint(stream: &mut TcpStream) -> std::io::Result<i32> {
    let mut num_read = 0;
    let mut result = 0i32;
    loop {
        let read = stream.read_u8().await?;
        let value = (read & 0x7F) as i32;
        result |= value << (7 * num_read);
        num_read += 1;
        if num_read > 5 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "VarInt is too big",
            ));
        }
        if (read & 0x80) == 0 {
            break;
        }
    }
    Ok(result)
}

pub fn strip_minecraft_formatting(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '§' {
            let _ = chars.next();
        } else {
            out.push(c);
        }
    }
    out.trim().to_string()
}

fn extract_motd(desc: &Value) -> String {
    if let Some(s) = desc.as_str() {
        return strip_minecraft_formatting(s);
    }
    if let Some(obj) = desc.as_object() {
        let mut combined = String::new();
        if let Some(text) = obj.get("text").and_then(|v| v.as_str()) {
            combined.push_str(text);
        }
        if let Some(extra) = obj.get("extra").and_then(|v| v.as_array()) {
            for item in extra {
                combined.push_str(&extract_motd(item));
            }
        }
        return strip_minecraft_formatting(&combined);
    }
    String::from("A Minecraft Server")
}

pub async fn ping_minecraft_server(host: &str, port: u16) -> ServerStatus {
    let addr = format!("{}:{}", host, port);
    let start = Instant::now();

    let result = timeout(Duration::from_secs(4), async {
        let mut stream = TcpStream::connect(&addr).await?;
        let tcp_latency = start.elapsed().as_millis() as u64;

        // Build Handshake packet (Packet ID 0x00)
        let mut handshake_data = Vec::new();
        write_varint(0x00, &mut handshake_data); // Packet ID
        write_varint(767, &mut handshake_data); // Protocol version (1.21+)
        let host_bytes = host.as_bytes();
        write_varint(host_bytes.len() as i32, &mut handshake_data);
        handshake_data.extend_from_slice(host_bytes);
        handshake_data.extend_from_slice(&port.to_be_bytes());
        write_varint(1, &mut handshake_data); // Next state = 1 (Status)

        let mut packet = Vec::new();
        write_varint(handshake_data.len() as i32, &mut packet);
        packet.extend_from_slice(&handshake_data);

        // Status Request packet (length 1, packet ID 0x00)
        packet.push(0x01);
        packet.push(0x00);

        stream.write_all(&packet).await?;
        stream.flush().await?;

        // Read response packet length & packet ID
        let _packet_len = read_varint(&mut stream).await?;
        let packet_id = read_varint(&mut stream).await?;
        if packet_id != 0x00 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Unexpected packet ID in status response",
            ));
        }

        let json_len = read_varint(&mut stream).await?;
        if !(0..=65536).contains(&json_len) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Invalid JSON length in status response",
            ));
        }

        let mut json_buf = vec![0u8; json_len as usize];
        stream.read_exact(&mut json_buf).await?;

        let parsed: Value = serde_json::from_slice(&json_buf).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, format!("JSON error: {}", e))
        })?;

        let players_online = parsed
            .get("players")
            .and_then(|p| p.get("online"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;

        let players_max = parsed
            .get("players")
            .and_then(|p| p.get("max"))
            .and_then(|v| v.as_u64())
            .unwrap_or(20) as u32;

        let version = parsed
            .get("version")
            .and_then(|v| v.get("name"))
            .and_then(|n| n.as_str())
            .map(strip_minecraft_formatting);

        let motd = parsed
            .get("description")
            .map(extract_motd)
            .unwrap_or_else(|| "Loxy Private Modpack Server".to_string());

        Ok::<ServerStatus, std::io::Error>(ServerStatus {
            online: true,
            host: host.to_string(),
            port,
            players_online,
            players_max,
            motd,
            version,
            latency_ms: Some(tcp_latency),
        })
    })
    .await;

    match result {
        Ok(Ok(status)) => status,
        _ => ServerStatus {
            online: false,
            host: host.to_string(),
            port,
            players_online: 0,
            players_max: 0,
            motd: "Server is currently offline or unreachable".to_string(),
            version: None,
            latency_ms: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_minecraft_formatting() {
        assert_eq!(
            strip_minecraft_formatting("§aLoxy §lServer §7- §bModpack v1.0"),
            "Loxy Server - Modpack v1.0"
        );
    }

    #[test]
    fn test_varint_encoding() {
        let mut buf = Vec::new();
        write_varint(300, &mut buf);
        assert_eq!(buf, vec![0xAC, 0x02]);
    }
}
