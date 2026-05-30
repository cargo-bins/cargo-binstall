use std::{net::SocketAddr, sync::Arc, time::Duration};

#[cfg(windows)]
use std::io;

#[cfg(windows)]
use hickory_resolver::config::ConnectionConfig;
use hickory_resolver::{
    config::{
        LookupIpStrategy, NameServerConfig, ResolverConfig, ResolverOpts, ServerGroup, CLOUDFLARE,
        GOOGLE,
    },
    system_conf, TokioResolver as TokioAsyncResolver,
};
use once_cell::sync::OnceCell;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use tracing::{debug, instrument, warn};

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
            let addrs: Addrs = Box::new(
                lookup
                    .iter()
                    .map(|ip| SocketAddr::new(ip, 0))
                    .collect::<Vec<_>>()
                    .into_iter(),
            );
            Ok(addrs)
        })
    }
}

/// Encrypted public DNS (Cloudflare then Google), used as a last resort when no usable
/// system nameserver can be obtained.
fn public_dns_configs() -> (ResolverConfig, ResolverOpts) {
    let mut config = ResolverConfig::default();
    let mut opts = ResolverOpts::default();

    let dns_providers = [CLOUDFLARE, GOOGLE];
    // quic first as it is secure while being the fastes
    dns_providers
        .iter()
        .flat_map(ServerGroup::quic)
        // h3 is secure but slower than quic
        .chain(dns_providers.iter().flat_map(ServerGroup::h3))
        // likewise tls is faster tha https
        .chain(dns_providers.iter().flat_map(ServerGroup::tls))
        .chain(dns_providers.iter().flat_map(ServerGroup::https))
        // fallback to udp and tcp
        .chain(dns_providers.iter().flat_map(ServerGroup::udp_and_tcp))
        .for_each(|name_server| config.add_name_server(name_server));

    opts.timeout = Duration::from_millis(750);

    (config, opts)
}

#[cfg(windows)]
fn get_system_configs() -> (ResolverConfig, ResolverOpts) {
    system_conf::read_system_conf().unwrap_or_else(|err| {
        debug!(
            "hickory-dns: failed to load system DNS configuration; \
            falling back to cloudflare and then google: {:?}",
            err
        );
        public_dns_configs()
    })
}

/// Extract the usable nameserver IPs from `/etc/resolv.conf`-style contents.
///
/// `hickory_resolver::system_conf::read_system_conf` is all-or-nothing: a single
/// unparseable `nameserver` entry fails the whole load. macOS routinely lists a
/// router-advertised link-local IPv6 server with a zone id (e.g.
/// `fe80::1%en0`) that it cannot parse, which throws away the valid
/// IPv4 nameserver alongside it. We salvage the entries we can actually use: scoped
/// (zone-suffixed) addresses are skipped because a link-local server is unusable without
/// its scope id, and anything that is not a bare `IpAddr` is ignored.
#[cfg(unix)]
fn parse_nameservers(contents: &str) -> Vec<std::net::IpAddr> {
    contents
        .lines()
        .filter_map(|line| {
            let mut tokens = line.split_whitespace();
            (tokens.next() == Some("nameserver"))
                .then(|| tokens.next())
                .flatten()
        })
        // Skip scoped/link-local entries (`addr%zone`); unusable without scope handling.
        .filter(|tok| !tok.contains('%'))
        .filter_map(|tok| tok.parse().ok())
        .collect()
}

/// Build a resolver config from the parseable system nameservers, or `None` if none are
/// usable.
#[cfg(unix)]
fn salvage_system_configs() -> Option<(ResolverConfig, ResolverOpts)> {
    let contents = std::fs::read_to_string("/etc/resolv.conf").ok()?;
    let nameservers = parse_nameservers(&contents);
    if nameservers.is_empty() {
        return None;
    }

    debug!(
        "Salvaged {} usable system nameserver(s) from /etc/resolv.conf",
        nameservers.len()
    );

    let mut config = ResolverConfig::default();
    for ip in nameservers {
        config.add_name_server(NameServerConfig::udp_and_tcp(ip));
    }
    Some((config, ResolverOpts::default()))
}

#[cfg(unix)]
fn get_configs() -> Result<(ResolverConfig, ResolverOpts), BoxError> {
    debug!("Using system DNS resolver configuration");
    match system_conf::read_system_conf() {
        Ok(configs) => Ok(configs),
        Err(err) => {
            debug!(
                "hickory-dns: failed to load system DNS configuration ({:?}); \
                attempting to salvage parseable nameservers from /etc/resolv.conf",
                err
            );
            Ok(salvage_system_configs().unwrap_or_else(|| {
                debug!("No usable system nameservers; falling back to public encrypted DNS");
                public_dns_configs()
            }))
        }
    }
}

#[cfg(windows)]
fn get_configs() -> Result<(ResolverConfig, ResolverOpts), BoxError> {
    debug!("Using custom DNS resolver configuration");
    let interface = get_default_interface()?;

    if interface.dns_servers.is_empty() {
        warn!("No DNS servers found on default interface; falling back to system DNS config");

        return Ok(get_system_configs());
    }

    let mut config = ResolverConfig::default();
    let mut opts = ResolverOpts::default();

    interface.dns_servers.into_iter().for_each(|addr| {
        tracing::trace!("Adding DNS server: {}", addr);
        config.add_name_server(NameServerConfig::new(
            addr,
            true,
            vec![
                ConnectionConfig::quic(Arc::from(addr.to_string())),
                ConnectionConfig::tls(Arc::from(addr.to_string())),
                ConnectionConfig::udp(),
                ConnectionConfig::tcp(),
            ],
        ));
    });

    opts.timeout = Duration::from_millis(750);

    Ok((config, opts))
}

#[instrument]
fn new_resolver() -> Result<TokioAsyncResolver, BoxError> {
    let (config, mut opts) = get_configs()?;

    debug!("Resolver configuration complete");

    opts.ip_strategy = LookupIpStrategy::Ipv4AndIpv6;

    let mut builder = TokioAsyncResolver::builder_with_config(config, Default::default());
    *builder.options_mut() = opts;
    Ok(builder.build()?)
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

#[cfg(all(test, unix))]
mod tests {
    use super::parse_nameservers;
    use std::net::IpAddr;

    fn ips(contents: &str) -> Vec<IpAddr> {
        parse_nameservers(contents)
    }

    #[test]
    fn skips_scoped_link_local_keeps_ipv4() {
        // The exact failure case on the affected macOS machine: a router-advertised
        // link-local IPv6 nameserver with a zone id that hickory cannot parse,
        // followed by a usable IPv4 nameserver.
        let resolv = "search home\n\
                      nameserver fe80::1%en0\n\
                      nameserver 192.168.1.254\n";
        let got = ips(resolv);
        assert_eq!(got, vec!["192.168.1.254".parse::<IpAddr>().unwrap()]);
    }

    #[test]
    fn keeps_plain_ipv6_nameserver() {
        let got = ips("nameserver 2606:4700:4700::1111\n");
        assert_eq!(got, vec!["2606:4700:4700::1111".parse::<IpAddr>().unwrap()]);
    }

    #[test]
    fn all_unparseable_yields_empty() {
        let got = ips("nameserver fe80::1%en0\nnameserver not-an-ip\n");
        assert!(got.is_empty());
    }

    #[test]
    fn ignores_comments_and_other_directives() {
        let resolv = "# a comment\n\
                      ; another comment\n\
                      domain example.com\n\
                      options edns0\n\
                      search a.example b.example\n\
                      nameserver 8.8.8.8\n\
                      garbage line without keyword\n\
                      nameserver\n";
        let got = ips(resolv);
        assert_eq!(got, vec!["8.8.8.8".parse::<IpAddr>().unwrap()]);
    }

    #[test]
    fn preserves_order_and_mix() {
        let resolv = "nameserver 1.1.1.1\n\
                      nameserver fe80::1%en0\n\
                      nameserver 2606:4700:4700::1111\n";
        let got = ips(resolv);
        assert_eq!(
            got,
            vec![
                "1.1.1.1".parse::<IpAddr>().unwrap(),
                "2606:4700:4700::1111".parse::<IpAddr>().unwrap(),
            ]
        );
    }
}
