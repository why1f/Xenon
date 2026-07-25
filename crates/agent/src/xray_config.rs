use crate::config::XrayConfig;
use anyhow::Context;
use serde_json::json;
use url::Url;

pub fn build_bootstrap(config: &XrayConfig) -> anyhow::Result<Vec<u8>> {
    let api_url = Url::parse(&config.api_endpoint).context("parse Xray API endpoint")?;
    let api_port = api_url
        .port_or_known_default()
        .context("Xray API endpoint must include a known port")?;
    let mut stream_settings = json!({
        "network": config.transport,
        "security": config.security,
    });
    match config.security.as_str() {
        "none" => {}
        "tls" => {
            let certificate_file = config
                .tls_certificate_path
                .as_deref()
                .context("TLS certificate path is required")?;
            let key_file = config
                .tls_key_path
                .as_deref()
                .context("TLS key path is required")?;
            stream_settings["tlsSettings"] = json!({
                "serverName": config.server_name,
                "certificates": [{
                    "certificateFile": certificate_file,
                    "keyFile": key_file,
                }],
            });
        }
        "reality" => {
            let private_key = config
                .reality_private_key
                .as_deref()
                .context("Reality private key is required")?;
            let server_name = config
                .server_name
                .as_deref()
                .context("Reality server_name is required")?;
            if config.reality_short_ids.is_empty() {
                anyhow::bail!("Reality short_ids must not be empty");
            }
            stream_settings["realitySettings"] = json!({
                "show": false,
                "dest": config.reality_dest,
                "xver": 0,
                "serverNames": [server_name],
                "privateKey": private_key,
                "shortIds": config.reality_short_ids,
                "fingerprint": config.reality_fingerprint,
            });
        }
        value => anyhow::bail!("unsupported Xray security: {value}"),
    }

    let value = json!({
        "log": { "loglevel": "warning" },
        "api": {
            "tag": "api",
            "services": ["HandlerService", "StatsService"],
        },
        "stats": {},
        "policy": {
            "levels": {
                "0": {
                    "statsUserUplink": true,
                    "statsUserDownlink": true,
                }
            },
            "system": {
                "statsInboundUplink": true,
                "statsInboundDownlink": true,
                "statsOutboundUplink": true,
                "statsOutboundDownlink": true,
            }
        },
        "inbounds": [
            {
                "tag": config.inbound_tag,
                "listen": config.listen_address,
                "port": config.listen_port,
                "protocol": config.protocol,
                "settings": {
                    "clients": [],
                    "decryption": "none",
                },
                "streamSettings": stream_settings,
            },
            {
                "tag": "api",
                "listen": "127.0.0.1",
                "port": api_port,
                "protocol": "dokodemo-door",
                "settings": { "address": "127.0.0.1" },
            }
        ],
        "outbounds": [
            { "protocol": "freedom", "tag": "direct" }
        ],
        "routing": {
            "domainStrategy": "AsIs",
            "rules": [
                { "type": "field", "inboundTag": ["api"], "outboundTag": "api" }
            ]
        }
    });
    Ok(serde_json::to_vec(&value)?)
}

#[cfg(test)]
mod tests {
    use super::build_bootstrap;
    use crate::config::XrayConfig;
    use serde_json::Value;

    #[test]
    fn builds_minimal_stats_enabled_vless_config() {
        let config = XrayConfig::default();
        let value: Value =
            serde_json::from_slice(&build_bootstrap(&config).expect("config")).expect("json");
        assert_eq!(value["inbounds"][0]["protocol"], "vless");
        assert_eq!(value["inbounds"][1]["port"], 10085);
        assert_eq!(value["policy"]["levels"]["0"]["statsUserUplink"], true);
    }

    #[test]
    fn builds_reality_settings_without_exposing_private_key_to_clients() {
        let config = XrayConfig {
            security: "reality".into(),
            server_name: Some("www.example.com".into()),
            reality_private_key: Some("private-key".into()),
            reality_short_ids: vec!["abcd".into()],
            ..XrayConfig::default()
        };
        let value: Value =
            serde_json::from_slice(&build_bootstrap(&config).expect("config")).expect("json");
        assert_eq!(
            value["inbounds"][0]["streamSettings"]["realitySettings"]["privateKey"],
            "private-key"
        );
        assert!(value["inbounds"][0]["streamSettings"]["realitySettings"]
            .get("publicKey")
            .is_none());
    }
}
