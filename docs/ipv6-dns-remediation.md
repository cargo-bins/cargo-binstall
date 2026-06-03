# IPv6 / DNS Remediation Plan

**Status:** implemented (§5.1 option 1) and verified on an affected host; salvage parser refactored to use `resolv_conf` crate (PR #2579), with follow-up fixes to retain best-effort salvage on malformed `resolv.conf` lines, to log all (not just the first) parse errors at debug, and to apply opportunistic encryption to salvaged nameservers (§5.1.1)
**Scope:** restore `cargo-binstall` DNS resolution on macOS hosts where the failure
manifests as an IPv6-related error. Other (non-IPv6) failures are out of scope.

---

## 1. Executive summary

On affected hosts `cargo-binstall` fails with a fatal `dns error` for *every* request
(e.g. `https://index.crates.io/config.json`). The cause is **not** broken IPv6
connectivity — IPv6 routing on such hosts is healthy. The actual cause is:

> The system's **primary DNS nameserver is a link-local IPv6 address with a zone
> identifier** (e.g. `fe80::1%en0`). `hickory-resolver`'s
> `system_conf::read_system_conf()` cannot parse the `%en0` zone suffix, fails the
> **entire** system-config load, and discards the working IPv4 nameserver alongside it.
> binstall then falls back to public DNS (Cloudflare/Google), preferring encrypted
> transports (DoT/DoH over TCP, then DoQ/DoH3 over UDP) and dropping to **plain UDP/TCP**
> as a last resort — none of which restrictive networks reliably allow, so resolution
> fails completely.

A previously-considered change — switching `LookupIpStrategy::Ipv4AndIpv6` →
`Ipv4thenIpv6` ("prefer IPv4 to avoid broken-IPv6 hangs") — **does not address this**:
`ip_strategy` only selects A vs AAAA records for the *target host* after the resolver is
already built; it has no effect on which *nameserver* is used or on the
`read_system_conf()` parse failure. That approach was not pursued.

---

## 2. Evidence

### 2.1 OS-level IPv6 is healthy (rules out the "broken IPv6" theory)

Direct per-family TCP connects from an affected host succeed for both IPv4 and IPv6
(e.g. `cloudflare.com`, `google.com` connect over v6 and v4 with comparable latency;
`github.com` has no AAAA at the apex, which is expected). A default IPv6 route exists
(`gateway fe80::1%en0`, interface `en0`). IPv6 is not only present, it is as fast as
IPv4 — there is no broken-IPv6 hang to fix.

### 2.2 The real failure, from binstall debug logs

```
new_resolver: Using system DNS resolver configuration
new_resolver: hickory-dns: failed to load system DNS configuration; falling back to
  cloudflare and then google: Msg("failed to parse nameserver address: invalid IP
  address syntax")
new_resolver: Resolver configuration complete
INFO  Received timeout error from reqwest. Delay future request by 200ms   (x3)
ERROR Fatal error:
  │ error sending request for url (https://index.crates.io/config.json)
  ├─▶ client error (Connect)
  ├─▶ dns error
```

So: system config load fails on a parse error → fallback to public encrypted DNS →
that times out → fatal `dns error`.

### 2.3 The offending nameserver

A typical affected configuration lists two nameservers:

```
nameserver[0] : fe80::1%en0      <-- link-local IPv6 + zone id
nameserver[1] : <LAN IPv4>       <-- plain IPv4, fine
```

`nameserver[0]` is the router advertising itself as the IPv6 DNS server via RA. The
`%en0` zone id is what hickory's `IpAddr` parse chokes on.

On macOS this list does **not** come from `/etc/resolv.conf`: hickory's `read_system_conf`
uses `system_conf/apple.rs`, which reads `ServerAddresses` from the System Configuration
dynamic store (`State:/Network/Global/DNS`) and parses each entry with
`IpAddr::from_str`. `/etc/resolv.conf` only *mirrors* the same servers — it matters
because binstall's **salvage** path re-reads that file with the tolerant `resolv_conf`
parser (see §3 and §5.1). The original failure, though, originates in the SC store, not
in `/etc/resolv.conf`.

> **Why not `unix.rs` / `/etc/resolv.conf` on macOS?** `system_conf::read_system_conf` is
> picked per target in `hickory-resolver 0.26.1`'s `src/system_conf/mod.rs`. The
> `/etc/resolv.conf` reader (`unix.rs`) is gated
> `#[cfg(all(unix, not(any(target_os = "android", target_vendor = "apple"))))]`, so apple
> targets are *excluded* from it and instead compile `apple.rs`
> (`#[cfg(target_vendor = "apple")]`). The `apple.rs` path is real in this workspace, not
> theoretical: enabling hickory's `system-config` feature pulls `system-configuration` as
> an apple-only dependency (present in `Cargo.lock`), which `apple.rs` uses to read the SC
> dynamic store. The verbatim §2.2 error string (`failed to parse nameserver address: …`)
> is produced only by `apple.rs`. (docs.rs "latest" may show a different `unix.rs`; the
> pinned `0.26.1` in this tree is what determines the actual path.)

### 2.4 Both nameservers actually work

Querying either nameserver directly resolves `index.crates.io` correctly — the IPv4
server via plain DNS, and the link-local v6 server via the OS resolver (which honours the
scope id). So the salvage path is real: keeping just the parseable IPv4 nameserver
restores full DNS. (The link-local v6 server is also usable if handled with its scope
id.)

### 2.5 Why the fallback didn't save it

The fallback path (`get_system_configs`) builds Cloudflare/Google resolvers ordered
QUIC → H3 → TLS → HTTPS → UDP/TCP, with a 750 ms timeout. On restrictive networks the
encrypted transports are not reliably reachable (UDP/853 QUIC in particular), so the
fallback times out before succeeding. The fallback is a poor substitute for the working
LAN resolver that was wrongly discarded.

---

## 3. Root cause (precise)

`crates/binstalk-downloader/src/remote/resolver.rs`, `get_system_configs()`:

```rust
system_conf::read_system_conf().unwrap_or_else(|err| { /* public-DNS fallback */ })
```

`read_system_conf()` is **all-or-nothing on macOS**: a single unparseable nameserver entry
(here, the scoped link-local IPv6 address) makes the whole call return `Err`, taking the
valid IPv4 nameserver down with it. The code then jumps straight to encrypted public DNS
(or, with the §5.1 fix, into the salvage path) instead of using the still-valid system
nameservers.

The exact mechanism is **platform-specific**, because hickory's `read_system_conf` is a
different implementation per OS:

- **macOS (`system_conf/apple.rs`):** reads `ServerAddresses` from the System
  Configuration dynamic store and parses each with `IpAddr::from_str`, mapping a failure
  to `"failed to parse nameserver address: {e}"` and propagating it with `?`. So one
  scoped entry hard-fails the **entire** read with exactly the §2.2 error
  (`invalid IP address syntax`). This is the affected path for the reported hosts, and the
  hard `Err` is what routes binstall into the salvage path. Verified on macOS:
  `IpAddr::from_str("fe80::1%en0")` → `invalid IP address syntax`. Note this path never
  touches `resolv-conf`, so scoped-address support in `resolv-conf` alone would not help
  it — `apple.rs` would need its own `%zone` handling.
- **Linux (`system_conf/unix.rs`):** parses via `resolv_conf::Config::parse` (fail-fast)
  and maps nameservers with `ip.into()`. With `resolv-conf 0.7.6`, `parse` **succeeds** on
  a scoped address (it becomes a `ScopedIp::V6(_, Some(_))`) and `ip.into(): IpAddr`
  **silently drops the zone**, yielding a scope-0 `fe80::1`. So `read_system_conf` returns
  `Ok` with an unusable nameserver, and binstall's `Ok` arm uses it directly — the salvage
  path is **not** triggered. Non-fatal when a usable nameserver coexists (the dead entry
  is merely queried and times out); for an IPv6-only LAN whose only server is link-local
  this leaks a dead config with no public-DNS fallback. This is a latent, Linux-only edge
  case; the reported failure is macOS-only.
- **Windows — cannot reproduce the wholesale failure.** binstall does not even reach
  hickory's `read_system_conf` on the normal Windows path: `get_configs()` (`#[cfg(windows)]`)
  reads `netdev::get_default_interface().dns_servers` (a `Vec<IpAddr>`) and adds each server
  to the `ResolverConfig` in an independent loop; hickory's `system_conf/windows.rs`
  (the fallback used only when `netdev` returns zero servers) likewise iterates
  `ipconfig` adapter `dns_servers()` and pushes each `IpAddr` independently. Two
  consequences: (1) the IPv6 **zone is already gone** — both Win32 sources hand back
  parsed `IpAddr` values, so there is no `%zone` string for `IpAddr::from_str` to choke
  on, unlike `apple.rs`; (2) because servers are added one-by-one, a single unusable
  entry can **never** discard the working ones. The worst case on Windows is the milder
  Linux-style outcome — a scope-0 link-local entry added as a dead nameserver that is
  queried and times out — and only when such a server is active alongside no usable one.
  There is therefore no Windows analogue of the macOS all-or-nothing crash, and the §5.1
  salvage parser (which is inherently a `/etc/resolv.conf` reader) neither applies to nor
  is needed on Windows.

Both symptoms share one root: there is nowhere in hickory's `NameServerConfig` to carry a
scope id (tracked upstream as [hickory-dns/hickory-dns#3713](https://github.com/hickory-dns/hickory-dns/issues/3713)).
Versions in tree: `hickory-resolver 0.26.1`, `resolv-conf 0.7.6`, `reqwest 0.13.4`,
`hyper-util 0.1.20`.

---

## 4. Immediate workaround (unblock an affected host without a rebuild)

Any **one** of the following restores binstall immediately; (a) and (b) need no rebuild:

**(a) Pin a non-link-local DNS server on the active service (e.g. Wi-Fi / en0).**
This removes the unparseable entry from the resolver's view:
```sh
sudo networksetup -setdnsservers Wi-Fi <LAN-IPv4-resolver>
# revert later with: sudo networksetup -setdnsservers Wi-Fi empty
```

**(b) Bypass hickory and use the OS resolver** (getaddrinfo handles scoped addresses
fine). Build/install without the default `trust-dns` feature:
```sh
cargo install cargo-binstall --no-default-features \
  --features static,rustls,fancy-no-backtrace,zstd-thin,git
```

**(c)** Use a local binary built from a branch carrying the §5 fix.

Quickest unblock: **(a)** — one command, instantly verifiable with
`cargo-binstall binstall ripgrep --dry-run --no-confirm`.

---

## 5. Proper fix (code change in this repo)

Goal: never discard a usable system DNS config because one nameserver entry is
unparseable. Make the unix system-config path resilient to scoped/link-local
nameservers.

### 5.1 Preferred approach — salvage parseable system nameservers

In `get_system_configs()` (the `#[cfg(unix)]` path), when `read_system_conf()` fails (or
unconditionally on macOS), read `/etc/resolv.conf` / `scutil` ourselves and build a
`ResolverConfig` from the nameserver entries we *can* use, only falling back to public
encrypted DNS when **zero** usable nameservers remain.

Two sub-options for the scoped link-local entry:

1. **Skip it, keep the rest.** Drop the `fe80::…%en0` entry, keep the IPv4 nameserver.
   Minimal, robust, sufficient (verified working in §2.4). Recommended first cut.
2. **Honour it. — BLOCKED by hickory-resolver 0.26.1, not implementable.**
   The intent was to parse the zone id into a `scope_id` and build a
   `SocketAddrV6::new(addr, 53, 0, scope_id)` name server so the link-local resolver is
   actually used (for IPv6-only LANs whose only advertised resolver is link-local).
   This cannot be done with the current dependency:
   - `hickory_resolver::config::NameServerConfig.ip` is a bare `std::net::IpAddr`.
   - `std::net::Ipv6Addr` has **no** scope/zone field — a scope only exists on
     `SocketAddrV6` — and there is no `scope_id` anywhere in hickory-resolver 0.26.1
     (the current major). The remote `SocketAddr` is built from `config.ip` + a default
     port; `ConnectionConfig.bind_addr` is the *local* bind, not the destination scope.
   - So a link-local nameserver can only be stored as `fe80::…` with scope 0, and a
     connect to it fails (`EINVAL`/no route) on any multi-interface host.

   Realistic alternatives for the IPv6-only-LAN case, none cheap:
   (a) upstream hickory support for scoped nameserver addresses, then adopt it;
   (b) resolve via the OS (`getaddrinfo`) for this case — i.e. document that affected
       users build with `--no-default-features` (drops `trust-dns`, uses reqwest's
       system resolver, which handles `%zone` correctly);
   (c) a bespoke libc `getaddrinfo` fallback path inside `TrustDnsResolver` — significant
       unsafe/unix code for an uncommon configuration.

   Recommendation: document (b) as the supported workaround; revisit (a) if/when
   hickory gains scope support. This gap does **not** affect hosts that have a usable
   IPv4 nameserver.

Implemented as `configs_from_resolv_conf` (testable, takes a parsed `resolv_conf::Config`)
plus `salvage_system_configs` (reads the file and calls the former). Uses the `resolv_conf`
crate for parsing — already a transitive dep via `hickory-resolver` — instead of a
hand-rolled line scanner. Filters `ScopedIp::V6(_, Some(_))` entries (scoped link-local)
and propagates all other resolver options (`domain`, `search`, `ndots`, `timeout`,
`attempts`, `edns0`) so a salvaged config carries the same options hickory would have
built from a clean parse. The one intentional difference is the per-nameserver transport
set, described in §5.1.1.

Important correction after the initial refactor: `resolv_conf::Config::parse()` is
fail-fast, so using it directly reintroduced an all-or-nothing failure mode for malformed
`resolv.conf` content. The salvage path must use `Config::parse_with_errors()` and then
build a resolver config from the partial parse result, otherwise unrelated malformed lines
or a bad `nameserver` entry defeat the salvage path and incorrectly force the public-DNS
fallback.

```rust
#[cfg(unix)]
fn get_configs() -> Result<(ResolverConfig, ResolverOpts), BoxError> {
    match system_conf::read_system_conf() {
        Ok(cfg) => Ok(cfg),
        Err(err) => {
            debug!("read_system_conf failed ({err}); attempting to salvage \
                    parseable nameservers from /etc/resolv.conf");
            Ok(salvage_system_configs().unwrap_or_else(|| {
                debug!("No usable system nameservers; falling back to public encrypted DNS");
                public_dns_configs()
            }))
        }
    }
}
```

### 5.1.1 Salvaged nameservers use opportunistic encryption — DONE

Salvaged nameservers are built with `NameServerConfig::opportunistic_encryption(ip)`
rather than `udp_and_tcp(ip)`. This attaches DNS-over-TLS and DNS-over-QUIC connections
alongside the plaintext UDP/TCP ones, so a salvaged resolver opportunistically upgrades to
encrypted DNS where the server supports it (many ISP resolvers do) and silently stays on
plaintext where it does not. It is the same privacy preference §5.2 applies to the
public-DNS fallback, extended to system nameservers.

Two properties of hickory's `opportunistic_encryption` matter here:

- **Connection order is plaintext-first** (`udp`, `tcp`, then `tls`, `quic`), the inverse
  of the public-DNS fallback's encrypted-first ordering in §5.2. That is deliberate:
  salvaged servers are typically the LAN/ISP resolver and are reachable over plaintext, so
  there is no firewall-traversal problem to solve — plaintext-first keeps the common path
  fast and treats encryption as a best-effort upgrade.
- **TLS/QUIC peer certificates are not verified**, per RFC 9539 §4.6.3.4 (opportunistic
  encryption uses the nameserver IP as the TLS server name; there is no PKI name to verify
  against). This is the spec-mandated behaviour for opportunistic encryption and does not
  weaken the plaintext baseline.

This is a deliberate divergence from what a clean `read_system_conf()` parse would
produce (plain UDP/TCP only); see the note in §5.1. Requires hickory's `__tls`/`__quic`
features, which are already enabled in this workspace. Covered by the
`salvaged_nameservers_use_opportunistic_encryption` unit test.

### 5.2 Secondary hardening — order the public-DNS fallback for reachability — DONE

Implemented. hickory queries fallback servers in config order, two at a time
(`ServerOrderingStrategy::QueryStatistics` with `num_concurrent_reqs == 2`), until it has
RTT statistics — so the cold-start config order decides which transports are attempted
first. The old order led with DoQ/DoH3 (UDP-based, frequently firewalled) and placed
plain DNS last. Reordered to: DoT (853/tcp) → DoH (443/tcp) → DoQ → DoH3 → plain UDP/TCP.
The TCP-based encrypted transports traverse UDP-blocking networks, encrypted DNS is still
preferred for privacy, and plain DNS remains the universal last resort. Per-query timeout
stays 750ms so a blocked transport fails fast. Covered by the
`public_dns_fallback_orders_transports_for_reachability` unit test.

### 5.3 Leave `ip_strategy` alone

Keep `LookupIpStrategy::Ipv4AndIpv6`. The reverted `Ipv4thenIpv6` change:
- does not touch this bug,
- regresses IPv6-only / NAT64+DNS64 networks (returns A records the client can't route).
If broken-IPv6 *connect* hangs ever become a real, reproduced problem, address them at
the connection layer via Happy Eyeballs (RFC 8305) tuning in the reqwest/hyper-util
connector — not by suppressing AAAA in DNS.

---

## 6. Tests

Pure-function target: `configs_from_resolv_conf(resolv_conf::Config) -> Option<(ResolverConfig, ResolverOpts)>`.

1. **Scoped link-local v6 is skipped, IPv4 retained** (the core failure case):
   input two nameservers `fe80::1%en0` and a plain IPv4 address →
   output contains the IPv4 address, is non-empty.
2. **Plain IPv6 nameserver retained:** `2606:4700:4700::1111` → kept.
3. **All-scoped input → `None`**, so caller uses public-DNS fallback.
4. **Mixed garbage tolerated:** comments, `search` lines, malformed entries ignored
   without failing the whole parse (handled by `resolv_conf::Config::parse_with_errors`).
5. **Resolver options extracted:** `ndots`, `timeout`, `attempts`, `edns0` match
   the values in the `options` line.
6. **Salvaged nameservers use opportunistic encryption** (§5.1.1): a salvaged config's
   per-nameserver transport set matches `NameServerConfig::opportunistic_encryption`
   (plaintext UDP/TCP plus DoT/DoQ), confirming the wiring survives
   `ResolverConfig::from_parts`.
7. **Public-DNS fallback transport ordering** (§5.2):
   `public_dns_fallback_orders_transports_for_reachability` asserts the fallback leads
   with DoT and ends with plain DNS, with an aggressive per-query timeout.

Integration-level (best-effort, network-gated, not in CI default): with hickory enabled,
a resolver built from a config containing only the salvaged IPv4 nameserver resolves
`index.crates.io` successfully.

---

## 7. Verification checklist

- [x] Reproduce pre-fix: `cargo-binstall binstall ripgrep --dry-run --no-confirm
      --log-level debug` → `dns error` (baseline confirmed).
- [x] Unit tests in §6 pass (9 tests, `--features hickory-dns`), including a regression
      test proving malformed lines do not defeat salvage and one asserting salvaged
      nameservers carry opportunistic-encryption transports.
- [x] Post-fix, same command resolves and completes the dry-run with the scoped
      link-local nameserver still present in `scutil --dns` (i.e. without workaround §4a).
      Log shows `Salvaged 1 usable system nameserver(s)` then successful fetches.
- [x] No behavioural change on a host whose `read_system_conf()` already succeeds
      (the `Ok` arm is untouched).
- [x] `ip_strategy` remains `Ipv4AndIpv6`.

Implemented: §5.1 option 1 (skip scoped entries), §5.1.1 (opportunistic encryption for
salvaged nameservers) and §5.2 (public-DNS fallback transport ordering). Blocked: §5.1
option 2 (honour link-local servers via scope id) — not
possible with hickory-resolver 0.26.1, see §5.1 for the dependency limitation and the
`--no-default-features` workaround for the IPv6-only-LAN case.

---

## 8. Files

- `crates/binstalk-downloader/src/remote/resolver.rs` — `get_configs` (unix),
  `configs_from_resolv_conf`, `salvage_system_configs`.
- `crates/binstalk-downloader/Cargo.toml` — `resolv-conf = "0.7.6"` added as a
  unix-only optional dep, enabled by the `hickory-dns` feature (already a transitive dep,
  so no new binary weight).
- Tests: same crate, `#[cfg(all(test, unix))]` module.

---

## 9. Known limitations & upstream work required

### 9.1 What is fully resolved (in this repo)

- The originally-reported failure: a scoped link-local IPv6 nameserver
  (`fe80::…%en0`) crashing `read_system_conf()` and taking the usable IPv4 nameserver
  with it. Fixed by salvaging parseable nameservers (§5.1 option 1), verified on an
  affected host.
- Fragile public-DNS fallback ordering. Fixed by leading with firewall-traversable
  encrypted transports (§5.2).

Both fixes live in `crates/binstalk-downloader`, which is a **first-party** member of
this workspace (`repository = cargo-bins/cargo-binstall`, consumed by path) — i.e. this
repo *is* its upstream. There is no separate downstream/vendor project to notify.

### 9.2 What cannot be completed here, and why

**IPv6-only LAN whose only advertised resolver is link-local.** If a network provides a
single link-local IPv6 DNS server (e.g. `fe80::1%en0`) and **no** IPv4 / global-scope
nameserver, binstall's bundled hickory resolver cannot use it. Our salvage code skips
the scoped entry (it has no other choice), finds nothing else usable, and falls back to
public DNS — which fails if that network also blocks outbound public resolvers.

Root limitation is in the third-party dependency, **not fixable in this repo**:

- `hickory_resolver::config::NameServerConfig.ip` is a bare `std::net::IpAddr`.
- `std::net::Ipv6Addr` carries no zone/scope; a scope only exists on `SocketAddrV6`.
  There is no `scope_id` anywhere in `hickory-resolver` 0.26.1 (current major).
- A link-local server can therefore only be stored as scope-0 and will not connect on a
  multi-interface host.

This is an uncommon topology and does **not** affect hosts that have a usable IPv4
nameserver.

### 9.3 What would be needed to fix 9.2

In increasing order of cost / decreasing preference:

1. **Upstream `hickory-dns/hickory`: support scoped nameserver addresses** — carry a
   `scope_id` (or accept `SocketAddrV6`) through `NameServerConfig` and connection setup.
   Once available, adopt it here by parsing the `%zone` (numeric, or interface name via
   `libc::if_nametoindex`) and building scoped name servers. This is the correct fix and
   the only one that keeps hickory in the loop. → **Filed:**
   [hickory-dns/hickory-dns#3713](https://github.com/hickory-dns/hickory-dns/issues/3713).
2. **Supported workaround (no upstream needed):** affected users build/install with
   `--no-default-features` so the `trust-dns`/`hickory-dns` feature is off and binstall
   uses reqwest's system resolver (`getaddrinfo`), which handles `%zone` correctly.
   Document this in user-facing docs. ← recommended interim answer.
3. **Bespoke `getaddrinfo` fallback inside `TrustDnsResolver`** for the link-local-only
   case — significant unsafe/unix-specific code for a rare configuration; not justified.

### 9.4 Related upstream item (optional, would simplify this repo)

`hickory_resolver::system_conf::read_system_conf()` is all-or-nothing: one unparseable
`nameserver` entry fails the entire load. That strictness is what forced our §5.1 salvage
workaround. An upstream fix to skip/tolerate unparseable entries (or to parse scoped
addresses) would make `salvage_system_configs` redundant — it could be removed once a
fixed hickory is in `Cargo.lock`. → optional **bug report to hickory**; our workaround
is harmless until then.

### 9.5 Action items outside this repo

- [x] Open hickory feature request: scoped/link-local nameserver support (`scope_id`). (§9.3.1)
      → [hickory-dns/hickory-dns#3713](https://github.com/hickory-dns/hickory-dns/issues/3713)
- [ ] (Optional) Open hickory bug: `read_system_conf` should not fail wholesale on one
      bad `nameserver` line. (§9.4)
- [ ] Add user-facing note: on IPv6-only LANs with only a link-local resolver, install
      with `--no-default-features`. (§9.3.2)
