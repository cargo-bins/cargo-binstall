use std::{net::SocketAddr, sync::Arc};

use hickory_resolver::{
    config::{LookupIpStrategy, ResolverConfig, ResolverOpts},
    system_conf, TokioAsyncResolver,
};
use once_cell::sync::OnceCell;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use tracing::{debug, instrument, warn};

#[cfg(windows)]
use hickory_resolver::config::{NameServerConfig, Protocol, ResolverConfig, ResolverOpts};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

#[derive(Debug, Default, Clone)]
pub struct TrustDnsResolver(Arc<OnceCell<TokioAsyncResolver>>);

impl Resolve for TrustDnsResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let resolver = self.clone();
        Box::pin(async move {
            let resolver = resolver.0.get_or_try_init(new_resolver)?;

            let lookup = resolver.lookup_ip(name.as_str()).await?;
            let addrs: Addrs = Box::new(lookup.into_iter().map(|ip| SocketAddr::new(ip, 0)));
            Ok(addrs)
        })
    }
}

#[cfg(unix)]
fn get_configs() -> Result<(ResolverConfig, ResolverOpts), BoxError> {
    debug!("Using system DNS resolver configuration");
    system_conf::read_system_conf().map_err(Into::into)
}

#[cfg(windows)]
fn get_configs() -> Result<(ResolverConfig, ResolverOpts), BoxError> {
    debug!("Using custom DNS resolver configuration");
    let mut config = ResolverConfig::new();
    let opts = ResolverOpts::default();

    get_adapter()?.dns_servers().iter().for_each(|addr| {
        tracing::trace!("Adding DNS server: {}", addr);
        let socket_addr = SocketAddr::new(*addr, 53);
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

    Ok((config, opts))
}

#[instrument]
fn new_resolver() -> Result<TokioAsyncResolver, BoxError> {
    let (config, mut opts) = get_configs()?;

    debug!("Resolver configuration complete");
    opts.ip_strategy = LookupIpStrategy::Ipv4AndIpv6;
    Ok(TokioAsyncResolver::tokio(config, opts))
}

#[cfg(windows)]
#[instrument]
fn get_adapter() -> Result<ipconfig::Adapter, BoxError> {
    debug!("Retrieving local IP address");
    let local_ip =
        default_net::interface::get_local_ipaddr().ok_or("Local IP address not found")?;
    debug!("Local IP address: {local_ip}");
    debug!("Retrieving network adapters");
    let adapters = ipconfig::get_adapters()?;
    debug!("Found {} network adapters", adapters.len());
    debug!("Searching for adapter with IP address {local_ip}");
    let adapter = adapters
        .into_iter()
        .find(|adapter| adapter.ip_addresses().contains(&local_ip))
        .ok_or("Adapter not found")?;
    debug!(
        "Using adapter {} with {} DNS servers",
        adapter.friendly_name(),
        adapter.dns_servers().len()
    );
    Ok(adapter)
}
