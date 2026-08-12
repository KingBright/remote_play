use remote_core::net::DEFAULT_CONTROL_PORT;
use std::net::SocketAddr;

pub fn configured_hosts() -> Vec<SocketAddr> {
    let mut hosts = std::env::var("REMOTE_PLAY_HOSTS")
        .ok()
        .map(|value| parse_configured_hosts(&value))
        .unwrap_or_default();

    if hosts.is_empty() {
        hosts.push(SocketAddr::from(([127, 0, 0, 1], DEFAULT_CONTROL_PORT)));
    }

    hosts
}

fn parse_configured_hosts(value: &str) -> Vec<SocketAddr> {
    let mut hosts = Vec::new();
    for addr in value
        .split(',')
        .filter_map(|part| part.trim().parse::<SocketAddr>().ok())
    {
        if !hosts.contains(&addr) {
            hosts.push(addr);
        }
    }
    hosts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_configured_hosts_without_invalid_or_duplicate_entries() {
        let hosts =
            parse_configured_hosts("127.0.0.1:39271, invalid, 10.0.0.2:9000, 127.0.0.1:39271");

        assert_eq!(hosts.len(), 2);
        assert_eq!(
            hosts[0],
            SocketAddr::from(([127, 0, 0, 1], DEFAULT_CONTROL_PORT))
        );
        assert_eq!(hosts[1], "10.0.0.2:9000".parse().unwrap());
    }
}
