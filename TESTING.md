# Testing and observed results

This page separates reproducible validation from product claims. Internet
speed tests depend on the selected server, routing, client and background
traffic; the measurements below describe one test setup and are not a
universal benchmark. Older release evidence is kept in the
[testing archive](TESTING_ARCHIVE.md).

## Current source acceptance

This summary covers the development source after the RC27 r318/r127 release
(audit remediation of commit 074e221, September 2026). It is not a release
announcement; publication has its own package, checksum and manifest checks.

### Source gate

Every change passes one scripted source gate: Full and Lite Rust tests,
formatting, LuCI JavaScript unit tests, TypeScript checks, a real-Chromium DOM
text-safety test, shell syntax, the speedtest-go backend patch tests and a
hash of all sources before and after the run. A gate is void if any source
changes while it runs.

### Package matrix

Full, Full LuCI and Lite LuCI packages are built in the OpenWrt 25.12 SDK for
all 12 ABIs and verified for payload, metadata, dependencies and ELF
interpreter/ABI. The speedtest-go backend (1.7.10-r5, CLI only) is built and
verified for the same 12 ABIs.

### VM acceptance (x86_64, OpenWrt 25.12)

- Full Raw Auto-Tune with an explicit Unlimited policy reached Review with
  three independent qualified servers; configuration and CAKE topology were
  restored exactly. A capped shaped-only run stayed within its budget.
- Speed Test and Auto-Tune over an explicit policy route (source, table, mark
  and route DNS): Speed Test completed with owned-traffic accounting within
  0.4% of interface counters; a Full Raw run reached Review (97.9%); the
  scheduler launched and completed an explicit-route run by itself.
- A dead split-default "VPN" route (0/1 and 128/1 via a black-hole interface):
  main-table mode refused probes and tests before any traffic; the explicit
  route kept working and sent no bytes into the VPN interface.
- Full → Lite → Full replacement with the documented `apk` procedure restored
  services, UCI, qdiscs and the apk world.
- Privacy: no unsolicited TCP/UDP by default; external transport probing only
  after opt-in, verified across restarts.
- Browser: the installed LuCI wizard ran a capped Auto-Tune to Review with all
  UCI writes blocked; desktop and phone layouts without horizontal overflow.

### Physical routers

- x86_64 router, PPPoE uplink (~900 Mbit/s) and a secondary uplink: Full Raw
  Unlimited Auto-Tune reached Review on both; owned-traffic accounting was 97–98%
  of interface counters; a capped browser-started run reached Review without
  UCI writes; a real Apply of the recommended option updated UCI and CAKE
  rates and left network, firewall and mwan3 unchanged.
- ARM64 (aarch64_generic) LTE router with a 100 MB root filesystem: the
  CLI-only speedtest-go package fits where the previous 25 MB package failed
  with ENOSPC; Full Raw Auto-Tune reached Review (accounting 99.3%).

### Known limits

- IPv6-only uplinks are not supported.
- Explicit policy routes were tested with a simulated VPN interface, not a
  real WireGuard or OpenVPN tunnel.
- Lite was verified on the VM; physical routers ran Full.
