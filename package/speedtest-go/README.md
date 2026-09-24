# Auto-Tune backend patch

This overlay builds upstream speedtest-go 1.7.10 with the source archive hash
pinned in the Makefile. Revision 2 adds a shared per-direction budget of eight
failed requests and a cancellable 50 ms delay between failed attempts. Requests
already in flight can complete after the threshold. Healthy transfers have no
added delay. The budget is a request-failure guard, not the user's traffic cap.

Both single-server and multi-server entry points return an error when the
budget is exhausted. A failed aggregate multi-server run is not a valid speed
measurement. Auto-Tune qualifies candidates separately. Parent cancellation
is propagated. HTTP errors, empty downloads and response read failures are
not accepted as healthy measurements. CLI fatal errors exit with status 1.

The patches include offline regressions using an in-memory HTTP transport;
they make no network connections. From a patched source tree, run:

```sh
go test -vet=off -race . ./speedtest -run '^TestCake' -count=5
```

The vet override is for unrelated upstream tests incompatible with Go 1.26's
nonconstant-format check. It does not disable the race detector or assertions.

Revision 3 adds opt-in `--route-dns-ipv4 <address>` for explicit routing. It
requires a canonical unicast IPv4 `--source`; invalid addresses fail before
discovery. DNS connects only to the selected server on TCP port 53, bound to
that source. The Go resolver is forced and the legacy `--dns-bind-source`
resolver is not installed in this mode. Without the new option, legacy DNS
behavior is unchanged. Routing marks, interface enforcement and traffic
accounting remain the native operation owner's responsibility.

`tools/audit-r6-backend-dns-source.py --attempt N` reconstructs the pinned
archive in a fresh temporary directory and runs the offline guard and DNS
validation tests with the race detector. This is source evidence only; it does
not prove live DNS routing or build/deploy an APK. Explicit operations require
the new option and must never retry with system DNS if an old backend rejects it.

SDK integration must select this package instead of the stock feed package,
inside a disposable SDK only. Preserve the package name and executable path.
Verify the resulting speedtest-go APK version, binary hash and Full daemon
dependency separately. Passing these offline tests does not establish physical
throughput, server qualification or traffic-budget acceptance.

The Full daemon package requires `speedtest-go >= 1.7.10-r2`; Lite does not
depend on the calibration backend. An installed r1 must not satisfy a new Full
installation. Supply the verified patched APK together with the Full package
and test the package-manager transaction before installation. The version floor
prevents the known r1 dependency from being silently reused; it is not a proof
that an arbitrary third-party package carrying a newer version has this patch.
Release provenance must still bind the backend APK to its reviewed source.
