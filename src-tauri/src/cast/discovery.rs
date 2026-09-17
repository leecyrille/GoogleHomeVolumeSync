//! Continuous mDNS discovery of Google Cast devices (`_googlecast._tcp`).
//! Also browses Yamaha MusicCast (`_musiccast._tcp` isn't standard; YXC devices
//! advertise `_http._tcp` — so Yamaha is found via its own probe or manual add)
//! and LG webOS (`_airplay` etc. unreliable — LG uses SSDP; manual add supported).

use mdns_sd::{ServiceDaemon, ServiceEvent};
use tracing::{debug, info, warn};

#[derive(Clone, Debug)]
pub struct DiscoveredCast {
    pub uuid: String,
    pub friendly_name: String,
    pub model: String,
    pub ip: String,
    pub port: u16,
    pub is_group: bool,
}

pub fn start_cast_browse(tx: tokio::sync::mpsc::Sender<DiscoveredCast>) {
    std::thread::spawn(move || {
        let daemon = match ServiceDaemon::new() {
            Ok(d) => d,
            Err(e) => {
                warn!(error=%e, "mdns: failed to start daemon");
                return;
            }
        };
        let receiver = match daemon.browse("_googlecast._tcp.local.") {
            Ok(r) => r,
            Err(e) => {
                warn!(error=%e, "mdns: failed to browse");
                return;
            }
        };
        info!("mdns: browsing _googlecast._tcp.local.");
        while let Ok(event) = receiver.recv() {
            if let ServiceEvent::ServiceResolved(info) = event {
                let get = |k: &str| info.get_property_val_str(k).unwrap_or("").to_string();
                let uuid = get("id");
                if uuid.is_empty() {
                    continue;
                }
                let model = get("md");
                let friendly = get("fn");
                // Cast groups run on the group leader with model "Google Cast Group".
                let is_group = model.eq_ignore_ascii_case("Google Cast Group");
                // IPv4 only: cast devices always advertise v4, and v6 link-local
                // addresses aren't reliably routable from here.
                let Some(ip) = info
                    .get_addresses()
                    .iter()
                    .find(|a| a.is_ipv4())
                    .map(|a| a.to_string())
                else {
                    continue;
                };
                let d = DiscoveredCast {
                    uuid: uuid.clone(),
                    friendly_name: friendly.clone(),
                    model: model.clone(),
                    ip: ip.clone(),
                    port: info.get_port(),
                    is_group,
                };
                debug!(uuid=%uuid, name=%friendly, model=%model, ip=%ip, port=info.get_port(), is_group, "mdns: resolved cast device");
                if tx.blocking_send(d).is_err() {
                    return;
                }
            }
        }
    });
}
