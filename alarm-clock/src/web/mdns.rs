//! mDNS discovery for the embedded web config server (slice 8).
//!
//! Advertises `alarm.local` as a `_https._tcp` service on the tokio worker so
//! phones on the same LAN can discover the Pi without knowing its IP address.

use mdns_sd::{ServiceDaemon, ServiceInfo};

/// Start advertising `alarm.local` over mDNS.
///
/// Returns the mDNS daemon handle so the caller can keep it alive for the
/// lifetime of the application. Dropping the daemon stops the advertisement.
pub fn advertise_alarm_local(port: u16) -> Result<ServiceDaemon, String> {
    let mdns = ServiceDaemon::new().map_err(|e| format!("failed to create mDNS daemon: {e}"))?;

    let service_type = "_https._tcp.local.";
    let instance_name = "alarm";
    let host_name = "alarm.local.";

    let service = ServiceInfo::new(
        service_type,
        instance_name,
        host_name,
        // Advertise on all interfaces.
        (),
        port,
        // No TXT properties needed for v1.
        Vec::<mdns_sd::TxtProperty>::new(),
    )
    .map_err(|e| format!("failed to build mDNS service info: {e}"))?;

    mdns.register(service)
        .map_err(|e| format!("failed to register mDNS service: {e}"))?;

    Ok(mdns)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mdns_advertisement_starts() {
        let daemon = advertise_alarm_local(8443);
        assert!(daemon.is_ok(), "mDNS advertisement should start");
        // The daemon handle is dropped here, unregistering the service.
    }
}
