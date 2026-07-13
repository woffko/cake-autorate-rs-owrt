#!/bin/sh
set -eu

base="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
script="$base/root/usr/libexec/cake-autorate-rs/speedtest"
work="${TMPDIR:-/tmp}/cake-speedtest-routing-test.$$"
mkdir -p "$work/bin"
trap 'rm -rf "$work"' EXIT INT TERM

set -- test eth0 '' '' '' '' ''
CAKE_AUTORATE_SPEEDTEST_SOURCE_ONLY=1 . "$script"

safe_route_name wanb
if safe_route_name 'wanb;reboot'; then
	echo "unsafe mwan3 member was accepted" >&2
	exit 1
fi

cat > "$work/bin/mwan3" <<'EOF'
#!/bin/sh
printf '%s\n' "$@" > "$CAKE_TEST_ARGV"
EOF
cat > "$work/bin/ip" <<'EOF'
#!/bin/sh
cat <<'RULES'
0: from all lookup local
1001: from all iif pppoe-wan lookup 1
1002: from all iif eth0 lookup 2
2001: from all fwmark 0x100/0x3f00 lookup 1
2002: from all fwmark 0x200/0x3f00 lookup 2
RULES
EOF
chmod +x "$work/bin/mwan3" "$work/bin/ip"

CAKE_TEST_ARGV="$work/argv"
export CAKE_TEST_ARGV
PATH="$work/bin:$PATH"
export PATH
route_mode=mwan3
mwan3_member=wanb
route_exec probe-command 'argument with spaces'

expected="use
wanb
exec
probe-command
argument with spaces"
actual="$(cat "$work/argv")"
[ "$actual" = "$expected" ] || {
	echo "routed command argv changed" >&2
	printf 'expected:\n%s\nactual:\n%s\n' "$expected" "$actual" >&2
	exit 1
}

[ "$(route_rule_for_device eth0)" = '0x200|2' ] || {
	echo "failed to resolve mwan3 fwmark/table for eth0" >&2
	exit 1
}

speedtest_job_dir="$work/jobs"
mkdir -p "$speedtest_job_dir/interface-eth0.lock"
printf '%s\n' "$$" > "$speedtest_job_dir/interface-eth0.lock/pid"
CAKE_AUTORATE_INTERFACE_LOCK_OWNER="$$"
export CAKE_AUTORATE_INTERFACE_LOCK_OWNER
target_if=eth0
interface_lock_dir=""
interface_lock_shared=0
acquire_interface_lock
[ "$interface_lock_shared" = 1 ] || {
	echo "Full Auto-Tune child did not borrow its parent interface lock" >&2
	exit 1
}
release_interface_lock
[ -d "$speedtest_job_dir/interface-eth0.lock" ] || {
	echo "borrowed interface lock was incorrectly released by child speed test" >&2
	exit 1
}
unset CAKE_AUTORATE_INTERFACE_LOCK_OWNER

download_kbps=900000
upload_kbps=700000
download_size=100
upload_size=50
download_elapsed=10.0
upload_elapsed=10.0
source=speedtest-go
backend=speedtest-go
test_server_id=1
test_server_name=test
test_server_sponsor=test
bind_mode=source-ip
target_if=eth0
route_device=eth0
route_source_ip=10.0.100.101
route_fwmark=0x200
route_table=2
external_ip_before=46.1.1.1
external_ip_after=46.1.1.1
calibration_shaper_bypassed=false
calibration_autorate_paused=false
calibration_sqm_paused=false
warning=
emit_result > "$work/result.json"

node -e '
const fs = require("fs");
const value = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
if (value.download_kbps !== 900000 || value.upload_kbps !== 700000)
  throw new Error("DL/UL fields were crossed");
if (value.route_mode !== "mwan3" || value.mwan3_member !== "wanb" || value.route_fwmark !== "0x200" || value.route_table !== "2")
  throw new Error("route metadata missing from speedtest result");
' "$work/result.json"

echo "speedtest routing tests passed"
