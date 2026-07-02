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
    TokioResolver as TokioAsyncResolver,
};

#[cfg(any(windows, target_vendor = "apple"))]
use hickory_resolver::system_conf;
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
    // Order for reachability, not raw speed: hickory queries servers in config order
    // (two at a time) until it has RTT statistics, so the cold-start order decides what
    // is attempted first. Lead with the TCP-based encrypted transports (DoT then DoH),
    // which traverse UDP-blocking / restrictive networks where DoQ and DoH3 (UDP-based)
    // silently stall. Keep plain unencrypted DNS as the universal last resort.
    dns_providers
        .iter()
        .flat_map(ServerGroup::tls)
        .chain(dns_providers.iter().flat_map(ServerGroup::https))
        // UDP-based encrypted transports: faster when reachable, often firewalled.
        .chain(dns_providers.iter().flat_map(ServerGroup::quic))
        .chain(dns_providers.iter().flat_map(ServerGroup::h3))
        // Last resort: plain, unencrypted DNS.
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

/// Build a resolver config from a parsed `resolv_conf::Config`, skipping scoped IPv6
/// nameservers (link-local with zone id, e.g. `fe80::1%en0`) that are unusable without
/// the scope id. Returns `None` when no usable nameservers remain.
#[cfg(unix)]
fn configs_from_resolv_conf(parsed: resolv_conf::Config) -> Option<(ResolverConfig, ResolverOpts)> {
    use hickory_resolver::proto::rr::Name as DnsName;
    use std::str::FromStr as _;

    let nameservers: Vec<NameServerConfig> = parsed
        .nameservers
        .iter()
        // Drop scoped IPv6 nameservers for now: hickory only accepts socket addresses here,
        // so link-local entries with a zone id from resolv.conf cannot be represented.
        // Revisit this once hickory supports scoped nameserver addresses directly:
        // https://github.com/hickory-dns/hickory-dns/issues/3713
        .filter(|ip| !matches!(ip, resolv_conf::ScopedIp::V6(_, Some(_))))
        .map(|ip| NameServerConfig::opportunistic_encryption(ip.into()))
        .collect();

    if nameservers.is_empty() {
        return None;
    }

    let domain = parsed
        .get_system_domain()
        .and_then(|d| DnsName::from_str(d.as_str()).ok());

    let search = parsed
        .get_last_search_or_domain()
        .filter(|s| *s != "--")
        .filter_map(|s| DnsName::from_str_relaxed(s).ok())
        .collect();

    let config = ResolverConfig::from_parts(domain, search, nameservers);

    let mut opts = ResolverOpts::default();
    opts.ndots = parsed.ndots as usize;
    opts.timeout = Duration::from_secs(u64::from(parsed.timeout));
    opts.attempts = parsed.attempts as usize;
    opts.edns0 = parsed.edns0;

    Some((config, opts))
}

/// Read `/etc/resolv.conf` and build a resolver config, keeping usable nameservers that
/// hickory's all-or-nothing `read_system_conf` would reject (e.g. scoped link-local IPv6
/// entries). This is our own parser, used in place of hickory's so a single unparseable
/// entry does not discard the whole configuration. Returns `None` when no usable
/// nameservers remain or the file cannot be read.
#[cfg(unix)]
fn read_system_configs() -> Option<(ResolverConfig, ResolverOpts)> {
    let data = std::fs::read("/etc/resolv.conf").ok()?;
    let (parsed, errors) = resolv_conf::Config::parse_with_errors(&data);
    if !errors.is_empty() {
        debug!(
            "Ignoring {} resolv.conf parse error(s) while keeping usable nameservers:",
            errors.len()
        );
        errors.iter().for_each(|err| debug!("    {err:?}"));
    }
    let result = configs_from_resolv_conf(parsed);
    if let Some((ref config, _)) = result {
        debug!(
            "Loaded {} usable system nameserver(s) from /etc/resolv.conf",
            config.name_servers().len()
        );
    }
    result
}

/// macOS reads its authoritative DNS configuration from the System Configuration dynamic
/// store, not `/etc/resolv.conf`: hickory's `read_system_conf` compiles `apple.rs` on
/// `target_vendor = "apple"` and queries `State:/Network/Global/DNS`, while
/// `/etc/resolv.conf` is only a best-effort mirror. So prefer `read_system_conf` here and
/// keep its result whenever it parses. It is all-or-nothing, though — a single scoped
/// link-local IPv6 nameserver (e.g. `fe80::1%en0`) hard-fails the whole read with
/// "invalid IP address syntax" — and only then do we salvage parseable entries from the
/// `/etc/resolv.conf` mirror before falling back to public encrypted DNS.
/// See https://github.com/hickory-dns/hickory-dns/issues/3713.
#[cfg(target_vendor = "apple")]
fn get_configs() -> Result<(ResolverConfig, ResolverOpts), BoxError> {
    debug!("Using system DNS resolver configuration");
    if let Ok(configs) = system_conf::read_system_conf() {
        return Ok(configs);
    }

    debug!(
        "hickory-dns: failed to load system DNS configuration; \
        attempting to salvage parseable nameservers from /etc/resolv.conf"
    );
    Ok(read_system_configs().unwrap_or_else(|| {
        debug!("No usable system nameservers; falling back to public encrypted DNS");
        public_dns_configs()
    }))
}

/// On non-apple unix, hickory's `read_system_conf` (`unix.rs`) reads the same
/// `/etc/resolv.conf` our parser does, but via `resolv_conf`'s fail-fast `parse` and an
/// `ip.into()` that silently drops IPv6 zone ids. Our parser reads that file directly,
/// tolerates malformed lines, and keeps usable nameservers a scoped entry would otherwise
/// discard, so it is the sole primary path here with no hickory call to fall back from.
#[cfg(all(unix, not(target_vendor = "apple")))]
fn get_configs() -> Result<(ResolverConfig, ResolverOpts), BoxError> {
    debug!("Using system DNS resolver configuration");
    Ok(read_system_configs().unwrap_or_else(|| {
        debug!("No usable system nameservers; falling back to public encrypted DNS");
        public_dns_configs()
    }))
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

    opts.ip_strategy = LookupIpStrategy::Ipv6AndIpv4;

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
    use super::configs_from_resolv_conf;
    use std::{net::IpAddr, time::Duration};

    fn nameserver_ips(resolv: &str) -> Vec<IpAddr> {
        let (parsed, _) = resolv_conf::Config::parse_with_errors(resolv.as_bytes());
        configs_from_resolv_conf(parsed)
            .map(|(cfg, _)| cfg.name_servers().iter().map(|ns| ns.ip).collect())
            .unwrap_or_default()
    }

    fn configs(
        resolv: &str,
    ) -> Option<(
        hickory_resolver::config::ResolverConfig,
        hickory_resolver::config::ResolverOpts,
    )> {
        let (parsed, _) = resolv_conf::Config::parse_with_errors(resolv.as_bytes());
        configs_from_resolv_conf(parsed)
    }

    #[test]
    fn skips_scoped_link_local_keeps_ipv4() {
        // The exact failure case on the affected macOS machine: a router-advertised
        // link-local IPv6 nameserver with a zone id that hickory cannot parse,
        // followed by a usable IPv4 nameserver.
        let resolv = "search home\n\
                      nameserver fe80::1%en0\n\
                      nameserver 192.168.1.254\n";
        assert_eq!(
            nameserver_ips(resolv),
            vec!["192.168.1.254".parse::<IpAddr>().unwrap()]
        );
    }

    #[test]
    fn keeps_plain_ipv6_nameserver() {
        assert_eq!(
            nameserver_ips("nameserver 2606:4700:4700::1111\n"),
            vec!["2606:4700:4700::1111".parse::<IpAddr>().unwrap()]
        );
    }

    #[test]
    fn all_scoped_yields_none() {
        // resolv_conf parses scoped addresses fine; configs_from_resolv_conf should
        // filter them all out and return None.
        let parsed =
            resolv_conf::Config::parse("nameserver fe80::1%en0\n".as_bytes()).expect("parse");
        assert!(configs_from_resolv_conf(parsed).is_none());
    }

    #[test]
    fn ignores_comments_and_other_directives() {
        let resolv = "# a comment\n\
                      domain example.com\n\
                      options edns0\n\
                      search a.example b.example\n\
                      nameserver 8.8.8.8\n";
        assert_eq!(
            nameserver_ips(resolv),
            vec!["8.8.8.8".parse::<IpAddr>().unwrap()]
        );
    }

    #[test]
    fn salvages_nameservers_despite_malformed_lines() {
        let resolv = "nameserver fe80::1%en0\n\
                      search a.example b.example\n\
                      garbage line without keyword\n\
                      nameserver 8.8.8.8\n\
                      nameserver\n";
        assert_eq!(
            nameserver_ips(resolv),
            vec!["8.8.8.8".parse::<IpAddr>().unwrap()]
        );
    }

    #[test]
    fn preserves_order_and_mix() {
        let resolv = "nameserver 1.1.1.1\n\
                      nameserver fe80::1%en0\n\
                      nameserver 2606:4700:4700::1111\n";
        assert_eq!(
            nameserver_ips(resolv),
            vec![
                "1.1.1.1".parse::<IpAddr>().unwrap(),
                "2606:4700:4700::1111".parse::<IpAddr>().unwrap(),
            ]
        );
    }

    #[test]
    fn extracts_resolver_opts() {
        let resolv = "nameserver 8.8.8.8\n\
                      options ndots:3 timeout:2 attempts:4 edns0\n";
        let (_, opts) = configs(resolv).expect("should produce config");
        assert_eq!(opts.ndots, 3);
        assert_eq!(opts.timeout, Duration::from_secs(2));
        assert_eq!(opts.attempts, 4);
        assert!(opts.edns0);
    }

    #[test]
    fn salvaged_nameservers_use_opportunistic_encryption() {
        use hickory_resolver::config::NameServerConfig;
        use std::net::IpAddr;

        let (config, _) = configs("nameserver 8.8.8.8\n").expect("should produce config");
        let nameserver = config
            .name_servers()
            .first()
            .expect("should contain a nameserver");
        let expected =
            NameServerConfig::opportunistic_encryption("8.8.8.8".parse::<IpAddr>().unwrap());

        let protocols: Vec<String> = nameserver
            .connections
            .iter()
            .map(|connection| format!("{:?}", connection.protocol))
            .collect();
        let expected_protocols: Vec<String> = expected
            .connections
            .iter()
            .map(|connection| format!("{:?}", connection.protocol))
            .collect();

        assert_eq!(protocols, expected_protocols);
    }

    /// The cold-resolver query order is the config order (hickory defaults to
    /// `QueryStatistics` ordering, which with no statistics yet preserves insertion
    /// order, and queries `num_concurrent_reqs == 2` servers at a time). The public-DNS
    /// fallback must therefore lead with the encrypted transports most likely to
    /// traverse restrictive/UDP-blocking networks (TCP-based DoT/DoH) before the
    /// UDP-based ones (DoQ/DoH3), and keep plain unencrypted DNS as the last resort.
    #[test]
    fn public_dns_fallback_orders_transports_for_reachability() {
        use hickory_resolver::config::ProtocolConfig;

        fn tier(p: &ProtocolConfig) -> u8 {
            match p {
                ProtocolConfig::Tls { .. } => 0,                // DoT  — TCP/853
                ProtocolConfig::Https { .. } => 1,              // DoH  — TCP/443
                ProtocolConfig::Quic { .. } => 2,               // DoQ  — UDP/853
                ProtocolConfig::H3 { .. } => 3,                 // DoH3 — UDP/443
                ProtocolConfig::Udp | ProtocolConfig::Tcp => 4, // plain, unencrypted
            }
        }

        let (config, opts) = super::public_dns_configs();
        let tiers: Vec<u8> = config
            .name_servers()
            .iter()
            .map(|ns| tier(&ns.connections[0].protocol))
            .collect();

        assert!(!tiers.is_empty());
        assert!(
            tiers.windows(2).all(|w| w[0] <= w[1]),
            "transport tiers not non-decreasing: {tiers:?}"
        );
        assert_eq!(
            tiers.first(),
            Some(&0),
            "DoT (TCP/853) should be tried first"
        );
        assert_eq!(
            tiers.last(),
            Some(&4),
            "plain unencrypted DNS should be last"
        );
        assert!(
            opts.timeout <= Duration::from_millis(750),
            "fallback per-query timeout should stay aggressive so blocked transports fail fast"
        );
    }
}
