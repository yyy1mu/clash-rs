use crate::{
    config::{LocalConfig, Server},
    controller,
    core::crypto,
    oidc, DOMAIN,
};
use anyhow::Result;
use std::io::{self, Write};
pub fn fetch_config() -> Result<LocalConfig> {
    let agent = controller::http_agent();

    let (kp_token, username) = oidc::run(&agent)?;

    let device_id = {
        use rand::RngCore;
        let mut b = [0u8; 8];
        rand::thread_rng().fill_bytes(&mut b);
        crypto::hex(&b)
    };
    if crate::core::util::debug_enabled() {
        eprintln!("  device_id={device_id}");
    }

    let dev_body = serde_json::json!({
        "domain": DOMAIN, "type": "android", "oem_name": "panabit",
        "device_id": device_id, "userName": username,
        "serverlist_version": "0", "ipfilter_version": "0", "branding_version": "0",
    });

    eprint!("  Registering device... ");
    io::stdout().flush().ok();
    let (st, _) = controller::post(&agent, "/m/auth", &dev_body, &kp_token)?;
    if st != 200 {
        anyhow::bail!("controller request failed HTTP {st}");
    }
    eprintln!("OK");

    let mut kp_body = dev_body.clone();
    kp_body["type"] = serde_json::Value::String("keepalive".into());
    let (st, _) = controller::post(&agent, "/m/keepalive", &kp_body, &kp_token)?;
    if st != 200 {
        anyhow::bail!("keepalive failed HTTP {st}");
    }

    eprint!("  Fetching server config... ");
    io::stdout().flush().ok();
    let (st, resp) = controller::post(&agent, "/m/config", &dev_body, &kp_token)?;
    if st != 200 {
        anyhow::bail!("controller request failed HTTP {st}");
    }
    eprintln!("OK");

    let list = resp["serverlist"]["serverlist"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("controller response has no server list"))?;
    let servers = list
        .iter()
        .map(|s| {
            let port = u16::try_from(s["serverPort"].as_u64().unwrap_or(6001))
                .map_err(|_| anyhow::anyhow!("invalid iwan server port"))?;
            let server = Server {
                name: string_field(s, "name"),
                host: string_field(s, "serverName"),
                port,
                username: string_field(s, "userName"),
                password: string_field(s, "passWord"),
            };
            anyhow::ensure!(
                !server.host.is_empty()
                    && !server.username.is_empty()
                    && !server.password.is_empty()
                    && port != 0,
                "controller returned incomplete iwan line"
            );
            Ok(server)
        })
        .collect::<Result<Vec<_>>>()?;
    anyhow::ensure!(!servers.is_empty(), "controller returned no iwan lines");
    Ok(LocalConfig {
        domain: DOMAIN.to_string(),
        servers,
    })
}

fn string_field(value: &serde_json::Value, key: &str) -> String {
    value[key].as_str().unwrap_or_default().to_string()
}

pub fn print_servers(servers: &[Server]) {
    for (idx, s) in servers.iter().enumerate() {
        println!("{:>2}. {:30} {}:{}", idx + 1, s.name, s.host, s.port);
    }
}
