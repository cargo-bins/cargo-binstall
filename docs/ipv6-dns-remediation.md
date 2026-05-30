# IPv6 / DNS Remediation Plan

**Status:** implemented (§5.1 option 1) and verified on an affected host
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
> binstall then falls back to encrypted public DNS (Cloudflare/Google over
> QUIC/H3/TLS/HTTPS), which restrictive networks do not reliably allow, so resolution
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

`/etc/resolv.conf` mirrors this; `nameserver[0]` is the router advertising itself as the
IPv6 DNS server via RA. The `%en0` zone id is what hickory's `IpAddr` parse chokes on.

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

`read_system_conf()` is **all-or-nothing**: a single unparseable nameserver entry
(here, the scoped link-local IPv6 address) makes the whole call return `Err`, taking the
valid IPv4 nameserver down with it. The code then jumps straight to encrypted public DNS
instead of using the still-valid system nameservers.

This is a known class of issue with `resolv-conf` / `hickory-resolver` and macOS RA-advertised
link-local IPv6 DNS servers (zone-scoped addresses). Versions in tree:
`hickory-resolver 0.26.1`, `reqwest 0.13.4`, `hyper-util 0.1.20`.

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

Sketch (option 1):

```rust
#[cfg(unix)]
fn get_configs() -> Result<(ResolverConfig, ResolverOpts), BoxError> {
    match system_conf::read_system_conf() {
        Ok(cfg) => Ok(cfg),
        Err(err) => {
            debug!("read_system_conf failed ({err}); attempting to salvage \
                    parseable nameservers from /etc/resolv.conf");
            match build_config_from_resolv_conf() {        // strips scoped/link-local
                Some(cfg) => Ok(cfg),
                None => Ok(get_system_configs()),           // existing public-DNS fallback
            }
        }
    }
}
```

`build_config_from_resolv_conf()` parses nameserver lines, strips any `%zone` suffix,
attempts `IpAddr::from_str`, and adds each successful one with
`config.add_name_server(NameServerConfig::udp_and_tcp(SocketAddr::new(ip, 53)))`.
Returns `None` if no nameserver parses.

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

## 6. Tests (TDD — write first)

Pure-function target: a `parse_nameservers(resolv_conf_contents: &str) -> Vec<IpAddr>`
(or `Vec<NameServerConfig>`) extracted so it is unit-testable without touching the OS.

1. **Scoped link-local v6 is skipped, IPv4 retained** (the core failure case):
   input two nameservers `fe80::1%en0` and a plain IPv4 address →
   output contains the IPv4 address, is non-empty.
2. **Plain IPv6 nameserver retained:** `2606:4700:4700::1111` → kept.
3. **All-unparseable input → `None`/empty**, so caller uses public-DNS fallback.
4. **Mixed garbage tolerated:** comments, `search` lines, malformed entries ignored
   without failing the whole parse.
5. (If implementing 5.1-option-2) **zone id parsed into scope_id** for a link-local
   entry.

Integration-level (best-effort, network-gated, not in CI default): with hickory enabled,
a resolver built from a config containing only the salvaged IPv4 nameserver resolves
`index.crates.io` successfully.

---

## 7. Verification checklist

- [x] Reproduce pre-fix: `cargo-binstall binstall ripgrep --dry-run --no-confirm
      --log-level debug` → `dns error` (baseline confirmed).
- [x] Unit tests in §6 pass (5 tests, `--features hickory-dns`).
- [x] Post-fix, same command resolves and completes the dry-run with the scoped
      link-local nameserver still present in `scutil --dns` (i.e. without workaround §4a).
      Log shows `Salvaged 1 usable system nameserver(s)` then successful fetches.
- [x] No behavioural change on a host whose `read_system_conf()` already succeeds
      (the `Ok` arm is untouched).
- [x] `ip_strategy` remains `Ipv4AndIpv6`.

Implemented: §5.1 option 1 (skip scoped entries) and §5.2 (public-DNS fallback transport
ordering). Blocked: §5.1 option 2 (honour link-local servers via scope id) — not
possible with hickory-resolver 0.26.1, see §5.1 for the dependency limitation and the
`--no-default-features` workaround for the IPv6-only-LAN case.

---

## 8. Files

- `crates/binstalk-downloader/src/remote/resolver.rs` — `get_configs` (unix),
  `get_system_configs`, new `parse_nameservers` / `build_config_from_resolv_conf`.
- Tests: same crate, `#[cfg(test)]` module.
- Note: an unrelated pre-existing `unused_imports` warning (`QUAD9`, `ConnectionConfig`,
  `NameServerConfig`) exists after the quad9 fallback removal; tidy while in the file but
  it is not part of this fix.

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
   the only one that keeps hickory in the loop. → **File a hickory feature request.**
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
addresses) would make `parse_nameservers` / `salvage_system_configs` redundant — they
could be removed once a fixed hickory is in `Cargo.lock`. → optional **bug report to
hickory**; our workaround is harmless until then.

### 9.5 Action items outside this repo

- [ ] Open hickory feature request: scoped/link-local nameserver support (`scope_id`). (§9.3.1)
- [ ] (Optional) Open hickory bug: `read_system_conf` should not fail wholesale on one
      bad `nameserver` line. (§9.4)
- [ ] Add user-facing note: on IPv6-only LANs with only a link-local resolver, install
      with `--no-default-features`. (§9.3.2)
