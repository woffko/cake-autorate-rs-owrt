# Testing and observed results

This page separates reproducible validation from product claims. Internet
speed tests depend on the selected server, routing, client and background
traffic; the measurements below describe one test setup and are not a
universal benchmark. Older release evidence is kept in the
[testing archive](TESTING_ARCHIVE.md).

## RC27 r320/r128 release acceptance

This summary covers the RC27 r320/r128 release (daemon r320, Full LuCI r128,
Lite r320/r8, speedtest-go 1.7.10-r5), built after the audit remediation of
commit 074e221 in September 2026.

### Source gate

Every change passes one scripted source gate of 53 commands: Full (1,777) and
Lite (419) Rust tests, formatting, LuCI JavaScript unit tests, TypeScript
checks, a real-Chromium DOM text-safety test, shell syntax, the speedtest-go
backend patch tests and a hash of all sources before and after the run. A gate
is void if any source changes while it runs.

### Package matrix

Full, Full LuCI and Lite LuCI packages are built in the OpenWrt 25.12 SDK for
all 12 ABIs and verified for payload, metadata, dependencies and ELF
interpreter/ABI; the LuCI payloads are identical across all SDKs. The
speedtest-go backend (1.7.10-r5, CLI only) is built from the repository recipe
after its race-detector patch tests and verified for the same 12 ABIs. The
release set (38 APKs) is bound to its source commit by `release-manifest.json`,
`matrix-report.json` and `SHA256SUMS`, and the new history, the decoded
payloads and the public metadata pass a pinned secret scan.

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
  UCI writes blocked. A scripted read-only visual audit covers Status, Graphs,
  Settings, Traffic priorities, the instance editor and the Auto-Tune wizard at
  desktop (1440 px) and phone (390 px) width in light and dark Bootstrap, on
  the VM and on a physical router, with no page-level horizontal overflow.

### Physical routers

- x86_64 router, PPPoE uplink (~900 Mbit/s) and a secondary uplink: Full Raw
  Unlimited Auto-Tune reached Review on both; owned-traffic accounting was 97–98%
  of interface counters; a capped browser-started run reached Review without
  UCI writes; a real Apply of the recommended option updated UCI and CAKE
  rates and left network, firewall and mwan3 unchanged.
- ARM64 (aarch64_generic) LTE router with a 100 MB root filesystem: the
  CLI-only speedtest-go package fits where the previous 25 MB package failed
  with ENOSPC; Full Raw Auto-Tune reached Review (accounting 99.3%).

### Auto-Tune robustness (server failures, raw controls, early stops, Gaming)

- Server failure retries: on the VM, TCP resets for the backend's user for
  5 s during a search measurement produced two logged retries
  (`speedtest-backend-failed`); the third attempt succeeded and the run
  reached Review. With 15 s of resets the retries were exhausted and the
  measurement was reported as unmeasurable instead of a terminal failure.
  Unit tests cover the retry limit (0–10), the request schema family and
  evidence replay up to eleven charged attempts. With an explicit limit the
  same fault logged no retry for 0 and exactly one for 1 before the
  measurement was marked unavailable; configuration and CAKE topology were
  restored both times.
- Launch from LuCI with a retry count: the daemon first refused every such
  start without a route mark (main routing) with "operation schema does not
  match its target lifecycle". After the fix, a wizard start on the VM
  (main routing, 1 GB, 1 retry) and on the LTE router (Variable link, Full
  raw, Unlimited, 2 retries) reached Review; the LTE Review offered
  recommended, lowest-latency, highest-throughput and download-without-shaping
  options. A shared fixture of the exact LuCI launch argv (new and existing
  instance, main, mwan3 and explicit routes, capped and Unlimited, with and
  without retries) is decoded by the daemon test through the same Start path.
- Server switch (`tools/audit-vm-server-switch.py --mode selected-server`):
  after the comparison selected server 17372, every backend connection to its
  address was reset for the rest of the run. The first raw download control
  failed three times and moved download to 29062; the raw upload control then
  failed three times and moved upload to 29062. Both switches restarted the
  measurements from the raw controls, and the Gaming run reached a complete
  Review with CAKE options (`recommended`, both shaped; `bypass_download`,
  upload shaped). The journal held two `server_switch` records; topology and
  configuration were restored and the fault table was removed.
- Incomplete upload search (`--mode upload-search`): backend connections were
  reset only while the job searched its upload rate. Upload failed on 17372
  and moved to 37193, the measurements restarted, and upload failed again
  with no qualified source left. Review (public schema 7) offered
  `partial_quality_first` (preselected for Gaming) and
  `partial_throughput_first`, both shaped with the exact rates that ran during
  the download search measurements, acknowledging
  `upload-search-incomplete` and `upload-shaped-load-unmeasured`, plus
  SQM disabled as a non-preferred option.
- x86_64 PPPoE router with mwan3, LuCI wizard, Gaming, Full raw,
  Unlimited, 2 retries: the run reached a complete Review with
  `recommended` (both shaped, 845.9/901.2 Mbit/s, class A+, Auto-Apply checks
  passed) and `bypass_download`. No switch was needed on server 12456, no
  route event occurred during the run, and nothing was applied.
- Raw control below the server comparison: with the raw upload control
  throttled to about 120 Mbit/s against a 317 Mbit/s comparison, the Full Raw
  run continued to Review; every option carried
  `upload-raw-below-server-comparison`, and the read-only Apply check
  returned `confirmation_ready` with the same acknowledgement list.
- Early stop: a DOM test checks that a stopped test states that the current
  settings are unchanged, explains the reason and offers to keep them. A
  start the service refuses is shown as "did not start", with nothing
  measured and no test traffic used.
- Re-calibration of an already tuned PPPoE instance on the x86_64 router
  reached Review with two options instead of stopping: a shaped option at
  about 904/913 Mbit/s that needed no acknowledgements, and a preselected
  no-shaping option. Accounting was 98% of interface counters.
- Gaming on the LTE router: the first candidate's samples did not repeat
  within 5%, so the search stepped down (to 219, 179, 159, 149 and
  144 Mbit/s download) instead of falling back to the raw result. Review
  offered a recommended lowest-delay option and a throughput-first option
  with a higher upload rate, each listing the capacity and latency
  acknowledgements it needs. Accounting was 99.6% of interface counters, and
  the CAKE topology was restored.

### Known limits

- IPv6-only uplinks are not supported.
- Explicit policy routes were tested with a simulated VPN interface, not a
  real WireGuard or OpenVPN tunnel.
- Lite was verified on the VM; physical routers ran Full.
