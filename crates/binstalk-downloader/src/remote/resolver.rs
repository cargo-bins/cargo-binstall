use std::{fmt, net::SocketAddr, sync::Arc, time::Duration};

#[cfg(windows)]
use std::io;

#[cfg(windows)]
use hickory_resolver::config::ConnectionConfig;
use hickory_resolver::{
    TokioResolver as TokioAsyncResolver,
    config::{
        CLOUDFLARE, GOOGLE, LookupIpStrategy, NameServerConfig, ResolverConfig, ResolverOpts,
        ServerGroup,
    },
    net::NetError,
};

#[cfg(any(windows, target_vendor = "apple"))]
use hickory_resolver::system_conf;
use once_cell::sync::OnceCell;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use tracing::{debug, instrument, warn};

#[cfg(windows)]
use netdev::Interface;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Resolves names with hickory-dns, optionally retrying lookups it cannot answer with
/// the operating system's resolver (`getaddrinfo`).
#[derive(Debug, Clone)]
pub struct TrustDnsResolver(Arc<Inner>);

struct Inner {
    resolver: OnceCell<TokioAsyncResolver>,
    /// Resolver to retry with when hickory-dns fails, `None` to report its error as is.
    fallback: Option<Arc<dyn Resolve>>,
}

impl fmt::Debug for Inner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TrustDnsResolver")
            .field("resolver", &self.resolver)
            .field("fallback_to_system", &self.fallback.is_some())
            .finish()
    }
}

impl TrustDnsResolver {
    /// hickory-dns only, its errors are returned as is.
    pub fn new() -> Self {
        Self::with_fallback(None)
    }

    /// hickory-dns, falling back to the operating system's resolver (`getaddrinfo`).
    pub fn with_system_fallback() -> Self {
        Self::with_fallback(Some(Arc::new(SystemResolver)))
    }

    fn with_fallback(fallback: Option<Arc<dyn Resolve>>) -> Self {
        Self(Arc::new(Inner {
            resolver: OnceCell::new(),
            fallback,
        }))
    }

    #[cfg(test)]
    fn from_parts(resolver: TokioAsyncResolver, fallback: Option<Arc<dyn Resolve>>) -> Self {
        let this = Self::with_fallback(fallback);
        this.0.resolver.set(resolver).unwrap();
        this
    }

    async fn lookup_hickory(&self, name: &str) -> Result<Addrs, HickoryError> {
        let resolver = self
            .0
            .resolver
            .get_or_try_init(new_resolver)
            .map_err(HickoryError::Init)?;

        let lookup = resolver
            .lookup_ip(name)
            .await
            .map_err(HickoryError::Lookup)?;
        Ok(Box::new(
            lookup
                .iter()
                .map(|ip| SocketAddr::new(ip, 0))
                .collect::<Vec<_>>()
                .into_iter(),
        ))
    }
}

impl Resolve for TrustDnsResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let this = self.clone();
        Box::pin(async move {
            let inner = &*this.0;

            let Some(fallback) = &inner.fallback else {
                return Ok(this.lookup_hickory(name.as_str()).await?);
            };

            let err = match this.lookup_hickory(name.as_str()).await {
                Ok(addrs) => return Ok(addrs),
                Err(err) if err.is_definitive() => return Err(err.into()),
                Err(err) => err,
            };

            debug!(
                "hickory-dns failed to resolve {}: {err}; \
                retrying with the system DNS resolver (getaddrinfo)",
                name.as_str()
            );

            fallback.resolve(name).await.map_err(|system| {
                Box::new(FallbackError {
                    hickory: err,
                    system,
                }) as BoxError
            })
        })
    }
}

#[derive(Debug)]
enum HickoryError {
    /// The resolver could not be built, e.g. the system DNS configuration is unusable.
    Init(BoxError),
    Lookup(NetError),
}

impl HickoryError {
    /// Whether a nameserver answered that the name has no address (NXDOMAIN, or NODATA).
    ///
    /// The system resolver would get the same answer, so retrying with it only costs
    /// another round trip.
    fn is_definitive(&self) -> bool {
        matches!(self, Self::Lookup(err) if err.is_no_records_found())
    }
}

impl fmt::Display for HickoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Init(err) => fmt::Display::fmt(err, f),
            Self::Lookup(err) => fmt::Display::fmt(err, f),
        }
    }
}

impl std::error::Error for HickoryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Init(err) => err.source(),
            Self::Lookup(err) => err.source(),
        }
    }
}

/// Both hickory-dns and the system resolver failed to resolve a name.
#[derive(Debug, thiserror::Error)]
#[error("hickory-dns failed: {hickory}; the system DNS resolver (getaddrinfo) failed too")]
struct FallbackError {
    hickory: HickoryError,
    #[source]
    system: BoxError,
}

/// The operating system's resolver (`getaddrinfo`), which is what reqwest uses when no
/// resolver is configured: `getaddrinfo` blocks, so it runs on tokio's blocking pool.
#[derive(Debug)]
struct SystemResolver;

impl Resolve for SystemResolver {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(async move {
            let addrs = tokio::net::lookup_host((name.as_str(), 0)).await?;
            Ok(Box::new(addrs.collect::<Vec<_>>().into_iter()) as Addrs)
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

#[cfg(test)]
mod fallback_tests {
    use std::{
        net::{IpAddr, Ipv4Addr, SocketAddr},
        str::FromStr,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use hickory_resolver::config::{
        ConnectionConfig, NameServerConfig, ResolveHosts, ResolverConfig, ResolverOpts,
    };
    use reqwest::dns::{Addrs, Name, Resolve, Resolving};

    use super::{FallbackError, TokioAsyncResolver, TrustDnsResolver};

    const FALLBACK_IP: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1));

    /// Stands in for `getaddrinfo`, counting how often it is asked.
    #[derive(Debug, Default)]
    struct StubSystem {
        calls: AtomicUsize,
        fail: bool,
    }

    impl Resolve for StubSystem {
        fn resolve(&self, _name: Name) -> Resolving {
            self.calls.fetch_add(1, Ordering::Relaxed);
            let fail = self.fail;
            Box::pin(async move {
                if fail {
                    Err("stub system resolver failed".into())
                } else {
                    Ok(Box::new(vec![SocketAddr::new(FALLBACK_IP, 0)].into_iter()) as Addrs)
                }
            })
        }
    }

    /// A hickory resolver whose only nameserver refuses TCP connections, so every
    /// lookup that reaches the network fails fast without depending on it.
    fn unreachable_hickory() -> TokioAsyncResolver {
        let port = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let mut connection = ConnectionConfig::tcp();
        connection.port = port;

        let mut config = ResolverConfig::default();
        config.add_name_server(NameServerConfig::new(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            true,
            vec![connection],
        ));
        let mut opts = ResolverOpts::default();
        opts.timeout = Duration::from_millis(500);
        opts.attempts = 0;
        opts.use_hosts_file = ResolveHosts::Never;

        let mut builder = TokioAsyncResolver::builder_with_config(config, Default::default());
        *builder.options_mut() = opts;
        builder.build().unwrap()
    }

    fn ips(addrs: Addrs) -> Vec<IpAddr> {
        addrs.map(|addr| addr.ip()).collect()
    }

    fn name(s: &str) -> Name {
        Name::from_str(s).unwrap()
    }

    #[tokio::test]
    async fn falls_back_to_system_per_lookup() {
        let system = Arc::new(StubSystem::default());
        let resolver = TrustDnsResolver::from_parts(unreachable_hickory(), Some(system.clone()));

        let addrs = resolver.resolve(name("example.com")).await.unwrap();
        assert_eq!(ips(addrs), [FALLBACK_IP]);
        assert_eq!(system.calls.load(Ordering::Relaxed), 1);

        // Later lookups still try hickory-dns first: `localhost` it answers locally.
        let addrs = resolver.resolve(name("localhost")).await.unwrap();
        assert!(ips(addrs).iter().all(IpAddr::is_loopback));
        assert_eq!(system.calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn explicit_hickory_does_not_fall_back() {
        let resolver = TrustDnsResolver::from_parts(unreachable_hickory(), None);

        let err = resolver.resolve(name("example.com")).await.err().unwrap();
        assert!(err.downcast_ref::<FallbackError>().is_none(), "{err:?}");
    }

    #[tokio::test]
    async fn nxdomain_is_not_retried() {
        let system = Arc::new(StubSystem::default());
        let resolver = TrustDnsResolver::from_parts(unreachable_hickory(), Some(system.clone()));

        // hickory-dns answers NXDOMAIN for `.invalid` (RFC 6761) without a query.
        assert!(resolver.resolve(name("binstall.invalid")).await.is_err());
        assert_eq!(system.calls.load(Ordering::Relaxed), 0);

        // An answer from hickory-dns is used as is.
        let addrs = resolver.resolve(name("localhost")).await.unwrap();
        assert!(ips(addrs).iter().all(IpAddr::is_loopback));
        assert_eq!(system.calls.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn reports_both_errors_when_system_fails_too() {
        let system = Arc::new(StubSystem {
            fail: true,
            ..Default::default()
        });
        let resolver = TrustDnsResolver::from_parts(unreachable_hickory(), Some(system.clone()));

        let err = resolver.resolve(name("example.com")).await.err().unwrap();
        let err = err.downcast_ref::<FallbackError>().expect("FallbackError");
        assert!(err.to_string().starts_with("hickory-dns failed: "), "{err}");
        assert_eq!(
            std::error::Error::source(err).unwrap().to_string(),
            "stub system resolver failed"
        );
    }

    #[tokio::test]
    async fn system_resolver_resolves_localhost() {
        let addrs = super::SystemResolver
            .resolve(name("localhost"))
            .await
            .unwrap();
        let ips = ips(addrs);
        assert!(!ips.is_empty());
        assert!(ips.iter().all(IpAddr::is_loopback), "{ips:?}");
    }
}
