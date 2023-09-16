use std::{net::SocketAddr, sync::Arc};

use hyper::client::connect::dns::Name;
use once_cell::sync::OnceCell;
use reqwest::dns::{Addrs, Resolve};
use trust_dns_resolver::config::{NameServerConfig, Protocol, ResolverConfig, ResolverOpts};
use trust_dns_resolver::TokioAsyncResolver;

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

fn new_resolver() -> Result<TokioAsyncResolver, Box<dyn std::error::Error + Send + Sync>> {
    #[cfg(unix)]
    {
        let (config, opts) = trust_dns_resolver::system_conf::read_system_conf()?;
        Ok(TokioAsyncResolver::tokio(config, opts))
    }
    #[cfg(windows)]
    {
        let mut config = ResolverConfig::new();
        let opts = ResolverOpts::default();

        let current_interface = default_net::get_default_interface()?;
        ipconfig::get_adapters()?
            .iter()
            .filter_map(|adapter| {
                if adapter.adapter_name() == current_interface.name {
                    Some(adapter.dns_servers())
                } else {
                    None
                }
            })
            .flatten()
            .for_each(|addr| {
                const DNS_PORT: u16 = 53;
                config.add_name_server(NameServerConfig {
                    socket_addr: SocketAddr::new(*addr, DNS_PORT),
                    protocol: Protocol::Tcp,
                    tls_dns_name: None,
                    trust_negative_responses: false,
                    #[cfg(feature = "rustls")]
                    tls_config: None,
                    bind_addr: None,
                });
                config.add_name_server(NameServerConfig {
                    socket_addr: SocketAddr::new(*addr, DNS_PORT),
                    protocol: Protocol::Udp,
                    tls_dns_name: None,
                    trust_negative_responses: false,
                    #[cfg(feature = "rustls")]
                    tls_config: None,
                    bind_addr: None,
                })
            });

        Ok(TokioAsyncResolver::tokio(config, opts))
    }
}
