use std::{net::SocketAddr, sync::Arc};

#[cfg(windows)]
use std::io;

use hickory_resolver::{
    config::{LookupIpStrategy, ResolverConfig, ResolverOpts},
    system_conf, TokioResolver as TokioAsyncResolver,
};
use once_cell::sync::OnceCell;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use tracing::{debug, instrument, warn};

#[cfg(windows)]
use hickory_resolver::{config::NameServerConfig, proto::xfer::Protocol};
#[cfg(windows)]
use netdev::Interface;

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

    let interface = get_default_interface()?;

    if interface.dns_servers.is_empty() {
        warn!("No DNS servers found on default interface; falling back to system DNS config");

        return system_conf::read_system_conf().map_err(|err| {
            io::Error::other(format!("Failed to read system DNS config: {err}")).into()
        });
    }

    interface.dns_servers.iter().for_each(|addr| {
        tracing::trace!("Adding DNS server: {}", addr);
        let socket_addr = SocketAddr::new(*addr, 53);
        for protocol in [Protocol::Udp, Protocol::Tcp] {
            config.add_name_server(NameServerConfig {
                socket_addr,
                protocol,
                tls_dns_name: None,
                trust_negative_responses: false,
                bind_addr: None,
                http_endpoint: None,
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
    let mut builder = TokioAsyncResolver::builder_with_config(config, Default::default());
    *builder.options_mut() = opts;
    Ok(builder.build())
}

#[cfg(windows)]
#[instrument]
fn get_default_interface() -> Result<Interface, BoxError> {
    debug!("Retrieving default network interface");
    let interface = netdev::get_default_interface().map_err(|err| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("Failed to get default interface: {err}"),
        )
    })?;

    let name = interface
        .friendly_name
        .as_deref()
        .unwrap_or(interface.name.as_str());

    debug!(
        "Using interface {} (index {}) with {} DNS servers",
        name,
        interface.index,
        interface.dns_servers.len()
    );

    Ok(interface)
}
