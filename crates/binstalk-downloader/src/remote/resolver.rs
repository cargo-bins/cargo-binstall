use std::{net::SocketAddr, sync::Arc};

use hyper::client::connect::dns::Name;
use once_cell::sync::OnceCell;
use reqwest::dns::{Addrs, Resolve};
use tracing::{debug, info, instrument, trace, warn};
#[cfg(windows)]
use trust_dns_resolver::config::{NameServerConfig, Protocol, ResolverConfig, ResolverOpts};
use trust_dns_resolver::TokioAsyncResolver;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

#[derive(Debug, Default, Clone)]
pub struct TrustDnsResolver(Arc<OnceCell<TokioAsyncResolver>>);

impl Resolve for TrustDnsResolver {
    fn resolve(&self, name: Name) -> reqwest::dns::Resolving {
        let resolver = self.clone();
        Box::pin(async move {
            let resolver = resolver.0.get_or_try_init(new_resolver)?;

            let lookup = resolver.lookup_ip(name.as_str()).await?;
            let addrs: Addrs = Box::new(lookup.into_iter().map(|ip| SocketAddr::new(ip, 0)));
            Ok(addrs)
        })
    }
}

#[instrument(level = "trace")]
fn new_resolver() -> Result<TokioAsyncResolver, BoxError> {
    #[cfg(unix)]
    {
        info!("Using system DNS resolver configuration");
        Ok(TokioAsyncResolver::tokio_from_system_conf()?)
    }
    #[cfg(windows)]
    {
        info!("Using custom DNS resolver configuration");
        let mut config = ResolverConfig::new();
        let opts = ResolverOpts::default();

        get_dns_servers()?.for_each(|addr| {
            trace!("Adding DNS server: {}", addr);
            let socket_addr = SocketAddr::new(addr, 53);
            for protocol in [Protocol::Udp, Protocol::Tcp] {
                config.add_name_server(NameServerConfig {
                    socket_addr,
                    protocol,
                    tls_dns_name: None,
                    trust_negative_responses: false,
                    #[cfg(feature = "rustls")]
                    tls_config: None,
                    bind_addr: None,
                })
            }
        });

        debug!("Resolver configuration complete");
        Ok(TokioAsyncResolver::tokio(config, opts))
    }
}

#[cfg(windows)]
#[instrument(level = "trace")]
fn get_dns_servers() -> Result<impl Iterator<Item = std::net::IpAddr>, BoxError> {
    debug!("Retrieving local IP address");
    let local_ip = match default_net::interface::get_local_ipaddr() {
        Some(ip) => ip,
        None => return Err("Local IP address not found".into()),
    };
    debug!("Local IP address: {local_ip}");
    debug!("Retrieving network adapters");
    let adapters = ipconfig::get_adapters()?;
    debug!("Found {} network adapters", adapters.len());
    debug!("Searching for adapter with IP address {local_ip}");
    let adapter = find_adapter(local_ip, adapters)?;
    debug!(
        "Using adapter {} with {} DNS servers",
        adapter.friendly_name(),
        adapter.dns_servers().len()
    );
    Ok(adapter.dns_servers().to_vec().into_iter())
}

#[cfg(windows)]
fn find_adapter(
    local_ip: std::net::IpAddr,
    adapters: Vec<ipconfig::Adapter>,
) -> Result<ipconfig::Adapter, BoxError> {
    let mut found: Option<ipconfig::Adapter> = None;
    for adapter in adapters {
        if adapter.ip_addresses().contains(&local_ip) {
            debug!("Found adapter candidate: {}", adapter.friendly_name());
            if found.is_none() {
                found = Some(adapter);
            }
        }
    }
    found.ok_or_else(|| "Adapter not found".into())
}
