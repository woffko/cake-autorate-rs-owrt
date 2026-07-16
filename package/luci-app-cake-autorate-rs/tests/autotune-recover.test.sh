#!/bin/sh
set -eu

test_dir="$(CDPATH= cd -- "$(dirname "$0")" && pwd)"
helper="$test_dir/../root/usr/libexec/cake-autorate-rs/autotune-recover"
fixtures="$test_dir/fixtures/autotune-recover"
work="$(mktemp -d "${TMPDIR:-/tmp}/cake-autotune-recover-test.XXXXXX")"

cleanup() {
	[ -z "${owner_pid_under_test:-}" ] || kill -KILL "$owner_pid_under_test" 2>/dev/null || true
	[ -z "${child_pid_under_test:-}" ] || kill -KILL "$child_pid_under_test" 2>/dev/null || true
	[ -z "${active_test_pid_under_test:-}" ] || kill -KILL "$active_test_pid_under_test" 2>/dev/null || true
	[ -z "${active_test_child_pid_under_test:-}" ] || kill -KILL "$active_test_child_pid_under_test" 2>/dev/null || true
	[ -z "${watcher_pid:-}" ] || kill -TERM "$watcher_pid" 2>/dev/null || true
	[ -z "${monitor_pid:-}" ] || kill -TERM "$monitor_pid" 2>/dev/null || true
	[ -z "${stale_recover_pid:-}" ] || kill -TERM "$stale_recover_pid" 2>/dev/null || true
	rm -rf "$work"
}
trap cleanup EXIT INT TERM

mkdir -p "$work/proc" "$work/sys" "$work/state"
: > "$work/actions"

export CAKE_RECOVER_TEST_ROOT="$work"
export CAKE_AUTORATE_RECOVER_PROC_ROOT="$work/proc"
export CAKE_AUTORATE_RECOVER_SYS_CLASS_NET="$work/sys"
export CAKE_AUTORATE_RECOVER_TC="$fixtures/tc"
export CAKE_AUTORATE_RECOVER_IP="$fixtures/ip"
export CAKE_AUTORATE_RECOVER_NFT="$fixtures/nft"
export CAKE_AUTORATE_RECOVER_KILL="$fixtures/kill"
export CAKE_AUTORATE_JSONFILTER="$test_dir/fixtures/autotune/jsonfilter"
export CAKE_AUTORATE_RECOVER_SQM_RECOVER="$fixtures/sqm-recover"
export CAKE_AUTORATE_RECOVER_POLL_SECONDS=0.05
export CAKE_AUTORATE_RUNTIME_LOCK_LIB="$test_dir/../../cake-autorate-rs/files/usr/libexec/cake-autorate-rs/runtime-lock"
export CAKE_AUTORATE_RUNTIME_LOCK_ROOT="$work/runtime-locks"
interface_record="$CAKE_AUTORATE_RUNTIME_LOCK_ROOT/interface-eth0.lock"
temp_fixture_ifb="catfdeadbeef"

create_temp_fixture_ifb() {
	mkdir -p "$work/sys/$temp_fixture_ifb"
	printf '%s\n' 42 > "$work/sys/$temp_fixture_ifb/ifindex"
	printf '%s\n' cake-autotune-recoverytest > "$work/sys/$temp_fixture_ifb/ifalias"
}

write_fake_process() {
	pid="$1"
	starttime="$2"
	state="$3"
	shift 3
	mkdir -p "$work/proc/$pid"
	printf '%s (fixture) S 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 %s 0\n' "$pid" "$starttime" > "$work/proc/$pid/stat"
	printf 'State:\t%s (fixture)\n' "$state" > "$work/proc/$pid/status"
	: > "$work/proc/$pid/cmdline"
	for argument in "$@"; do
		printf '%s\0' "$argument" >> "$work/proc/$pid/cmdline"
	done
}

write_journal() {
	journal="$1"
	owner="$2"
	owner_start="$3"
	autorate="${4:-}"
	autorate_start="${5:-}"
	active_test_pgid="${6:-}"
	active_test_start="${7:-}"
	mkdir -p "$CAKE_AUTORATE_RUNTIME_LOCK_ROOT"
	snapshot="$CAKE_AUTORATE_RUNTIME_LOCK_ROOT/sqm-snapshot.recoverytest"
	printf '%s\n' 'config queue cake_wan_sqm' > "$snapshot"
	chmod 600 "$snapshot"
	cat > "$interface_record" <<EOF
version=1
pid=$owner
proc_starttime=$owner_start
role=autotune
token=recoverytest
recovery_journal=$journal
EOF
	cat > "$journal" <<EOF
journal_version=4
job=wan_sqm
section=wan_sqm
owner_pid=$owner
owner_pgid=$owner
proc_starttime=$owner_start
target_if=eth0
temp_ifb=$temp_fixture_ifb
temp_ifb_alias=cake-autotune-recoverytest
temp_ifb_ifindex=42
temp_target_handle=adea:
temp_ifb_handle=bdea:
temp_ingress_handle=ffff:
temp_redirect_pref=49152
shaper_started=1
sqm_was_active=1
sqm_restore_mode=managed
sqm_section=cake_wan_sqm
sqm_target_if=eth0
sqm_ul_if=eth0
sqm_dl_if=ifb4eth0
sqm_config_fingerprint=sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
sqm_config_snapshot=$snapshot
sqm_restore_required=1
autorate_pid=$autorate
autorate_starttime=$autorate_start
active_test_pgid=$active_test_pgid
active_test_starttime=$active_test_start
nft_table=cake_autotune_wan_sqm_123
interface_lock=$interface_record
interface_lock_token=recoverytest
done_marker=$work/done
error_file=$work/error.json
result_file=$work/result.json
status_file=$work/status.json
pending_result_file=$work/result.pending.json
pending_error_file=$work/error.pending.json
heartbeat_file=$work/heartbeat
EOF
	printf '%s\n' '{"state":"failed","schema_version":5,"producer":"cake-autorate-rs-autotune","profile":"best_overall","runtime_restored":false,"recovery_pending":true}' > "$work/error.pending.json"
	: > "$work/heartbeat"
}

set_no_sqm_journal() {
	journal="$1"
	snapshot_to_remove="$(sed -n 's/^sqm_config_snapshot=//p' "$journal")"
	[ -z "$snapshot_to_remove" ] || rm -f "$snapshot_to_remove"
	sed -i 's/^sqm_was_active=1$/sqm_was_active=0/' "$journal"
	sed -i 's/^sqm_restore_mode=managed$/sqm_restore_mode=none/' "$journal"
	sed -i 's/^sqm_config_fingerprint=.*$/sqm_config_fingerprint=/' "$journal"
	sed -i 's|^sqm_config_snapshot=.*$|sqm_config_snapshot=|' "$journal"
	sed -i 's/^sqm_restore_required=1$/sqm_restore_required=0/' "$journal"
}

expected_sqm_call="sqm-recover wan_sqm cake_wan_sqm eth0 eth0 ifb4eth0 sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa $CAKE_AUTORATE_RUNTIME_LOCK_ROOT/sqm-snapshot.recoverytest"

reset_case() {
	rm -rf "$work/proc" "$work/sys" "$work/state" "$CAKE_AUTORATE_RUNTIME_LOCK_ROOT"
	mkdir -p "$CAKE_AUTORATE_RUNTIME_LOCK_ROOT"
	rm -f "$work/done" "$work/heartbeat" "$work/error.json" "$work/result.json" "$work/status.json" \
		"$work/error.pending.json" "$work/result.pending.json" "$work"/*.cleanup-done "$work"/nft-* \
		"$work"/tc-* \
		"$work/sqm-restored" "$work/sqm-entered" "$work/sqm-release" "$work/orphan-child.pid" \
		"$work/speedtest-child.pid"
	mkdir -p "$work/proc" "$work/sys" "$work/state"
	: > "$work/actions"
}

# Strict publishing is independently testable: a second JSON document is
# discarded and replaced only after runtime restoration has already succeeded.
CAKE_AUTORATE_RECOVER_SOURCE_ONLY=1 sh -c '
	set -eu
	. "$1"
	job=wan_sqm
	section=wan_sqm
	pending_result_file="$2/direct-result.pending.json"
	pending_error_file="$2/direct-error.pending.json"
	result_file="$2/direct-result.json"
	error_file="$2/direct-error.json"
	status_file="$2/direct-status.json"
	printf "%s\n%s\n" \
		"{\"state\":\"failed\",\"schema_version\":4,\"producer\":\"cake-autorate-rs-autotune\",\"profile\":\"best_overall\",\"runtime_restored\":false,\"recovery_pending\":true}" \
		"{\"forged\":true}" > "$pending_error_file"
	publish_pending_terminal
	grep -q "\"state\":\"recovery-failed\"" "$error_file"
	grep -q "\"runtime_restored\":true" "$error_file"
	! grep -q "\"forged\":true" "$error_file"
' sh "$helper" "$work"

# A normally completed owner creates the done marker before disappearing.
# Recovery must be a complete no-op even if stale-looking journal data remains.
reset_case
write_journal "$work/normal.journal" 200 100
sed -i 's/^sqm_restore_required=1$/sqm_restore_required=0/' "$work/normal.journal"
rm -f "$CAKE_AUTORATE_RUNTIME_LOCK_ROOT/sqm-snapshot.recoverytest"
: > "$work/done"
"$helper" recover "$work/normal.journal"
[ ! -s "$work/actions" ]
[ ! -e "$work/normal.journal" ]
[ ! -e "$work/heartbeat" ]

# A watcher in its own process observes a real owner killed with SIGKILL, then
# removes only journal-owned temporary state, resumes the exact daemon, and
# delegates SQM health verification to sqm-recover.
reset_case
export AUTOTUNE_RECOVER_CHILD_PID_FILE="$work/orphan-child.pid"
setsid "$fixtures/autotune" wan_sqm eth0 job-run &
owner_pid_under_test="$!"
attempt=0
while [ "$attempt" -lt 40 ] && [ ! -s "$AUTOTUNE_RECOVER_CHILD_PID_FILE" ]; do
	sleep 0.02
	attempt=$((attempt + 1))
done
child_pid_under_test="$(sed -n '1p' "$AUTOTUNE_RECOVER_CHILD_PID_FILE")"
owner_start="$(sed 's/^.*) //' "/proc/$owner_pid_under_test/stat" | awk '{print $20}')"
owner_group="$(sed 's/^.*) //' "/proc/$owner_pid_under_test/stat" | awk '{print $3}')"
[ "$owner_group" = "$owner_pid_under_test" ]
export AUTOTUNE_RECOVER_SPEEDTEST_CHILD_PID_FILE="$work/speedtest-child.pid"
setsid "$fixtures/speedtest" wan_sqm eth0 run speedtest-go &
active_test_pid_under_test="$!"
attempt=0
while [ "$attempt" -lt 40 ] && [ ! -s "$AUTOTUNE_RECOVER_SPEEDTEST_CHILD_PID_FILE" ]; do
	sleep 0.02
	attempt=$((attempt + 1))
done
active_test_child_pid_under_test="$(sed -n '1p' "$AUTOTUNE_RECOVER_SPEEDTEST_CHILD_PID_FILE")"
active_test_start_under_test="$(sed 's/^.*) //' "/proc/$active_test_pid_under_test/stat" | awk '{print $20}')"
active_test_group="$(sed 's/^.*) //' "/proc/$active_test_pid_under_test/stat" | awk '{print $3}')"
[ "$active_test_group" = "$active_test_pid_under_test" ]
create_temp_fixture_ifb
: > "$work/nft-cake_autotune_wan_sqm_123"
write_journal "$work/killed.journal" "$owner_pid_under_test" "$owner_start"
CAKE_AUTORATE_RECOVER_PROC_ROOT=/proc CAKE_AUTORATE_RECOVER_KILL=/bin/kill \
	"$helper" watch "$work/killed.journal" &
watcher_pid="$!"
sleep 0.1
# The detached watcher deliberately loaded the prepared snapshot above.  Replace
# it atomically with the final destructive-state identities and prove that the
# watcher reloads this version after the owner dies.
write_journal "$work/killed.updated" "$owner_pid_under_test" "$owner_start" "" "" \
	"$active_test_pid_under_test" "$active_test_start_under_test"
mv "$work/killed.updated" "$work/killed.journal"
sed -i "s|^recovery_journal=.*|recovery_journal=$work/killed.journal|" "$interface_record"
kill -KILL "$owner_pid_under_test"
# Deliberately do not wait yet: the owner remains a zombie while recovery must
# still terminate the live child in its original setsid group.
if ! wait "$watcher_pid"; then
	cat "$work/error.json" >&2
	ps -eo pid,ppid,pgid,sid,state,cmd | awk -v group="$owner_pid_under_test" '$3 == group' >&2
	exit 1
fi
watcher_pid=""
! kill -0 "$child_pid_under_test" 2>/dev/null
child_pid_under_test=""
! kill -0 "$active_test_child_pid_under_test" 2>/dev/null
active_test_child_pid_under_test=""
wait "$active_test_pid_under_test" 2>/dev/null || true
active_test_pid_under_test=""
wait "$owner_pid_under_test" 2>/dev/null || true
owner_pid_under_test=""
unset AUTOTUNE_RECOVER_CHILD_PID_FILE AUTOTUNE_RECOVER_SPEEDTEST_CHILD_PID_FILE
	[ ! -e "$work/done" ]
	[ ! -e "$interface_record" ]
[ ! -e "$work/sys/$temp_fixture_ifb" ]
[ ! -e "$work/nft-cake_autotune_wan_sqm_123" ]
grep -Fqx "$expected_sqm_call" "$work/actions"
[ -e "$work/sqm-restored" ]
grep -q '"runtime_restored":true' "$work/error.json"

# A syntactically valid first object followed by a second JSON document must
# never be published from the private recovery transaction.  Runtime cleanup
# completes, but the untrusted diagnostic is replaced with a bounded internal
# recovery-failed object.
reset_case
write_journal "$work/malformed-pending.journal" 242 100
printf '%s\n%s\n' \
	'{"state":"failed","schema_version":5,"producer":"cake-autorate-rs-autotune","profile":"best_overall","runtime_restored":false,"recovery_pending":true}' \
	'{"forged":true}' > "$work/error.pending.json"
"$helper" recover "$work/malformed-pending.journal"
node -e 'JSON.parse(require("fs").readFileSync(process.argv[1], "utf8"))' "$work/error.json"
grep -q '"state":"failed"' "$work/error.json"
grep -q 'Full Auto-Tune was interrupted' "$work/error.json"
grep -q '"runtime_restored":true' "$work/error.json"
! grep -q '"forged":true' "$work/error.json"
[ ! -e "$work/malformed-pending.journal" ]
[ ! -e "$interface_record" ]
grep -q '"recovery_pending":false' "$work/error.json"

# A worker killed after journal preparation but before arming has not changed
# network state.  Recovery only garbage-collects its journal and stale lock.
reset_case
write_journal "$work/prepared.journal" 240 100
rm -f "$work/heartbeat"
"$helper" recover "$work/prepared.journal"
[ ! -e "$work/prepared.journal" ]
	[ ! -e "$interface_record" ]
[ ! -s "$work/actions" ]

# A SIGKILL after `ip link add` but before the complete alias+ifindex identity
# is atomically journalled must never suppress managed-SQM restoration.  The
# uncommitted random IFB is deliberately not deleted without a durable identity;
# it is harmless, while the production link is immediately re-shaped.
reset_case
write_journal "$work/partial-ifb.journal" 241 100
sed -i 's/^shaper_started=1$/shaper_started=0/' "$work/partial-ifb.journal"
sed -i 's/^temp_ifb_ifindex=42$/temp_ifb_ifindex=/' "$work/partial-ifb.journal"
create_temp_fixture_ifb
"$helper" recover "$work/partial-ifb.journal"
grep -Fqx "$expected_sqm_call" "$work/actions"
[ -e "$work/sqm-restored" ]
[ -e "$work/sys/$temp_fixture_ifb" ]
[ ! -e "$work/partial-ifb.journal" ]
[ ! -e "$interface_record" ]
grep -q '"runtime_restored":true' "$work/error.json"

# RC17 journals bind restoration to a complete private SQM snapshot. The exact
# SHA256 identity and root-owned RAM file are both forwarded to the strict
# seven-argument helper.
reset_case
write_journal "$work/v4.journal" 242 100
"$helper" recover "$work/v4.journal"
grep -Fqx "$expected_sqm_call" "$work/actions"
[ ! -e "$CAKE_AUTORATE_RUNTIME_LOCK_ROOT/sqm-snapshot.recoverytest" ]

# Legacy journals and malformed fingerprints fail closed before network cleanup.
reset_case
write_journal "$work/v3-rejected.journal" 243 100
sed -i 's/^journal_version=4$/journal_version=3/' "$work/v3-rejected.journal"
if "$helper" recover "$work/v3-rejected.journal" >/dev/null 2>&1; then
	echo 'legacy SQM recovery journal unexpectedly passed validation' >&2
	exit 1
fi
[ ! -s "$work/actions" ]
reset_case
write_journal "$work/v4-invalid.journal" 244 100
sed -i 's/^sqm_config_fingerprint=.*/sqm_config_fingerprint=sha256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA/' "$work/v4-invalid.journal"
if "$helper" recover "$work/v4-invalid.journal" >/dev/null 2>&1; then
	echo 'invalid SQM fingerprint unexpectedly passed recovery journal validation' >&2
	exit 1
fi
[ ! -s "$work/actions" ]

# A procd/manual recovery process may parse the prepared snapshot and be
# descheduled before it acquires the mutex. The authoritative reload under that
# mutex must see a later atomic active-test identity rather than recovering from
# the stale snapshot.
reset_case
write_journal "$work/stale-snapshot.journal" 245 100
write_fake_process 245 100 S /usr/libexec/cake-autorate-rs/autotune wan_sqm eth0 job-run
export CAKE_AUTORATE_RECOVER_TEST_BEFORE_LOCK_MARKER="$work/before-lock"
export CAKE_AUTORATE_RECOVER_TEST_BEFORE_LOCK_RELEASE="$work/release-before-lock"
"$helper" recover "$work/stale-snapshot.journal" &
stale_recover_pid="$!"
attempt=0
while [ "$attempt" -lt 40 ] && [ ! -e "$CAKE_AUTORATE_RECOVER_TEST_BEFORE_LOCK_MARKER" ]; do
	sleep 0.02
	attempt=$((attempt + 1))
done
[ -e "$CAKE_AUTORATE_RECOVER_TEST_BEFORE_LOCK_MARKER" ]
mkdir -p "$work/proc/246"
create_temp_fixture_ifb
awk 'BEGIN {
	printf "246 (fixture) S 1 246 246"
	for (i = 5; i < 20; i++) printf " 0"
	printf " 200 0\n"
}' > "$work/proc/246/stat"
printf 'State:\tS (fixture)\n' > "$work/proc/246/status"
printf '/usr/libexec/cake-autorate-rs/speedtest\0wan_sqm\0eth0\0run\0speedtest-go\0' > "$work/proc/246/cmdline"
: > "$work/nft-cake_autotune_wan_sqm_123"
write_journal "$work/stale-snapshot.updated" 245 100 "" "" 246 200
mv "$work/stale-snapshot.updated" "$work/stale-snapshot.journal"
sed -i "s|^recovery_journal=.*|recovery_journal=$work/stale-snapshot.journal|" "$interface_record"
rm -rf "$work/proc/245"
: > "$CAKE_AUTORATE_RECOVER_TEST_BEFORE_LOCK_RELEASE"
wait "$stale_recover_pid"
stale_recover_pid=""
unset CAKE_AUTORATE_RECOVER_TEST_BEFORE_LOCK_MARKER CAKE_AUTORATE_RECOVER_TEST_BEFORE_LOCK_RELEASE
[ ! -e "$work/proc/246" ]
[ ! -e "$work/sys/$temp_fixture_ifb" ]
[ ! -e "$work/nft-cake_autotune_wan_sqm_123" ]
grep -q '"runtime_restored":true' "$work/error.json"

# Marker aliasing could otherwise make an armed journal look already complete.
# Reject malformed snapshots before any recovery action.
reset_case
write_journal "$work/aliased.journal" 247 100
sed -i "s|^done_marker=.*|done_marker=$work/heartbeat|" "$work/aliased.journal"
if "$helper" recover "$work/aliased.journal" 2> "$work/alias.stderr"; then
	echo 'aliased recovery markers unexpectedly passed validation' >&2
	exit 1
fi
grep -q 'paths must be distinct' "$work/alias.stderr"

# An exact stopped cake-autorated identity is resumed.  This is separate from
# the real SIGKILL test so both process trees can be controlled deterministically.
reset_case
write_journal "$work/continue.journal" 250 100 301 901
write_fake_process 301 901 T /usr/sbin/cake-autorated --instance wan_sqm
"$helper" recover "$work/continue.journal"
grep -q '^kill -CONT 301$' "$work/actions"
sqm_line="$(sed -n '/^sqm-recover /=' "$work/actions")"
cont_line="$(sed -n '/^kill -CONT /=' "$work/actions")"
[ -n "$sqm_line" ] && [ -n "$cont_line" ] && [ "$sqm_line" -lt "$cont_line" ]
	[ ! -e "$interface_record" ]

# Numeric PID reuse must never authorize signalling the unrelated process, but
# it must not leak journal-owned qdisc/SQM state either.
reset_case
write_journal "$work/reused-owner.journal" 401 100
write_fake_process 401 101 S /usr/libexec/cake-autorate-rs/autotune wan_sqm eth0 job-run
"$helper" recover "$work/reused-owner.journal"
grep -q 'owner PID identity changed' "$work/error.json"
grep -q '"runtime_restored":true' "$work/error.json"
! grep -q '^kill ' "$work/actions"
grep -Fqx "$expected_sqm_call" "$work/actions"
[ -d "$work/proc/401" ]
	[ ! -e "$work/done" ]

# The same rule applies independently to the paused cake-autorated process:
# cleanup may proceed, but a reused PID is never sent CONT and is reported.
reset_case
write_journal "$work/reused-autorate.journal" 501 100 601 200
write_fake_process 601 201 T /usr/sbin/cake-autorated --instance wan_sqm
if "$helper" recover "$work/reused-autorate.journal"; then
	echo 'stopped reused autorate PID unexpectedly satisfied recovery' >&2
	exit 1
fi
! grep -q '^kill ' "$work/actions"
grep -q 'no verified running replacement exists' "$work/error.json"
[ -d "$work/proc/601" ]
	[ -e "$interface_record" ]

# A stopped daemon is not considered resumed merely because CONT returned 0.
reset_case
write_journal "$work/still-stopped.journal" 650 100 651 201
write_fake_process 651 201 T /usr/sbin/cake-autorated --instance wan_sqm
export CAKE_RECOVER_KEEP_STOPPED=1
if "$helper" recover "$work/still-stopped.journal"; then
	echo 'stopped autorate process unexpectedly passed postconditions' >&2
	exit 1
fi
unset CAKE_RECOVER_KEEP_STOPPED
grep -q 'did not leave the stopped state' "$work/error.json"
	[ -e "$interface_record" ]

# Orphan helpers in the worker's setsid group are terminated before qdisc/SQM
# restoration.  The fixture removes the fake group on TERM.
reset_case
write_journal "$work/orphan-group.journal" 670 100
mkdir -p "$work/proc/671"
printf '671 (child) S 1 670 0\n' > "$work/proc/671/stat"
"$helper" recover "$work/orphan-group.journal"
grep -q '^kill -TERM -- -670$' "$work/actions"
[ ! -e "$work/proc/671" ]

# An originally unshaped link is recovered without inventing SQM state, but
# only after proving that temporary CAKE and ingress redirect are gone.
reset_case
write_journal "$work/no-sqm.journal" 675 100
set_no_sqm_journal "$work/no-sqm.journal"
"$helper" recover "$work/no-sqm.journal"
! grep -q '^sqm-recover ' "$work/actions"
	[ ! -e "$work/done" ]

# Deleting a root qdisc on a physical OpenWrt device normally exposes the
# kernel-owned handle-0 mq root again.  It is the clean absence of our random
# CAKE handle, so normal finalize/recovery must continue to managed-SQM restore.
reset_case
write_journal "$work/kernel-default.journal" 675 101
export CAKE_RECOVER_TC_KERNEL_DEFAULT=1
"$helper" recover "$work/kernel-default.journal"
unset CAKE_RECOVER_TC_KERNEL_DEFAULT
grep -Fqx "$expected_sqm_call" "$work/actions"
[ -e "$work/sqm-restored" ]
[ ! -e "$work/kernel-default.journal" ]
[ ! -e "$interface_record" ]

# A non-default replacement root is materially different: never overwrite it
# merely because the random temporary IFB has disappeared.
reset_case
write_journal "$work/foreign-root.journal" 675 102
export CAKE_RECOVER_TC_FOREIGN_ROOT=1
if "$helper" recover "$work/foreign-root.journal"; then
	echo 'foreign root qdisc unexpectedly passed recovery' >&2
	exit 1
fi
unset CAKE_RECOVER_TC_FOREIGN_ROOT
grep -q 'ownership no longer matches' "$work/error.json"
! grep -q '^sqm-recover ' "$work/actions"
[ -e "$interface_record" ]

# A complete temporary shaper is deleted only when its qdisc handles, redirect
# preference, random-token IFB alias, and ifindex all match the journal.
reset_case
write_journal "$work/exact-owned.journal" 676 100
set_no_sqm_journal "$work/exact-owned.journal"
create_temp_fixture_ifb
: > "$work/tc-target-root"
: > "$work/tc-ingress"
: > "$work/tc-ifb-root"
: > "$work/tc-filter"
export CAKE_RECOVER_TC_OWNED=1
"$helper" recover "$work/exact-owned.journal"
unset CAKE_RECOVER_TC_OWNED
[ ! -e "$work/sys/$temp_fixture_ifb" ]
[ "$(grep -c '^tc qdisc del dev ' "$work/actions")" -eq 3 ]

# A foreign replacement with the same conventional qdisc handles but a
# different IFB ownership alias is never deleted.
reset_case
write_journal "$work/foreign-owned.journal" 677 100
set_no_sqm_journal "$work/foreign-owned.journal"
create_temp_fixture_ifb
printf '%s\n' foreign-owner > "$work/sys/$temp_fixture_ifb/ifalias"
: > "$work/tc-target-root"
: > "$work/tc-ingress"
: > "$work/tc-ifb-root"
: > "$work/tc-filter"
export CAKE_RECOVER_TC_OWNED=1
if "$helper" recover "$work/foreign-owned.journal"; then
	echo 'foreign temporary shaper unexpectedly passed ownership recovery' >&2
	exit 1
fi
unset CAKE_RECOVER_TC_OWNED
grep -q 'ownership no longer matches' "$work/error.json"
! grep -q '^tc qdisc del dev ' "$work/actions"
[ -e "$work/sys/$temp_fixture_ifb" ]

# A zero exit from a delete command is irrelevant if the real postcondition
# still shows temporary CAKE/redirect state.  Keep the interface lock armed.
reset_case
write_journal "$work/delete-leak.journal" 680 100
set_no_sqm_journal "$work/delete-leak.journal"
export CAKE_RECOVER_TC_LEAK=1
create_temp_fixture_ifb
if "$helper" recover "$work/delete-leak.journal"; then
	echo 'leftover temporary shaper unexpectedly passed recovery' >&2
	exit 1
fi
unset CAKE_RECOVER_TC_LEAK
grep -q 'temporary shaper postconditions are not clean' "$work/error.json"
	[ -e "$interface_record" ]

reset_case
write_journal "$work/tc-unreadable.journal" 681 100
set_no_sqm_journal "$work/tc-unreadable.journal"
export CAKE_RECOVER_TC_SHOW_FAIL=1
if "$helper" recover "$work/tc-unreadable.journal"; then
	echo 'unreadable tc postcondition unexpectedly passed recovery' >&2
	exit 1
fi
unset CAKE_RECOVER_TC_SHOW_FAIL
grep -q 'ownership state could not be read' "$work/error.json"
	[ -e "$interface_record" ]

reset_case
write_journal "$work/nft-unreadable.journal" 682 100
export CAKE_RECOVER_NFT_LIST_FAIL=1
if "$helper" recover "$work/nft-unreadable.journal"; then
	echo 'unreadable nftables postcondition unexpectedly passed recovery' >&2
	exit 1
fi
unset CAKE_RECOVER_NFT_LIST_FAIL
grep -q 'nftables state could not be read' "$work/error.json"
grep -Fqx "$expected_sqm_call" "$work/actions"
[ -e "$work/sqm-restored" ]
[ ! -e "$CAKE_AUTORATE_RUNTIME_LOCK_ROOT/sqm-snapshot.recoverytest" ]
grep -q '^sqm_restore_required=0$' "$work/nft-unreadable.journal"
[ -e "$interface_record" ]
# Once the ancillary nftables inspection recovers, retry completes without a
# second SQM mutation because the durable journal already records restoration.
"$helper" recover "$work/nft-unreadable.journal"
[ "$(grep -Fxc "$expected_sqm_call" "$work/actions")" -eq 1 ]
[ ! -e "$work/nft-unreadable.journal" ]
[ ! -e "$interface_record" ]
grep -q '"runtime_restored":true' "$work/error.json"

# Detached watch and the procd scanner serialize on a per-journal mutex.
reset_case
write_journal "$work/concurrent.journal" 690 100
export CAKE_RECOVER_SQM_BLOCK=1
"$helper" recover "$work/concurrent.journal" &
first_recovery="$!"
attempt=0
while [ "$attempt" -lt 40 ] && [ ! -e "$work/sqm-entered" ]; do
	sleep 0.02
	attempt=$((attempt + 1))
done
[ -e "$work/sqm-entered" ]
if "$helper" recover "$work/concurrent.journal"; then
	echo 'concurrent recovery unexpectedly acquired the same journal' >&2
	exit 1
else
	[ "$?" -eq 75 ]
fi
: > "$work/sqm-release"
wait "$first_recovery"
unset CAKE_RECOVER_SQM_BLOCK
[ "$(grep -Fxc "$expected_sqm_call" "$work/actions")" -eq 1 ]

# A real SQM postcondition failure remains visible in the RAM-only error file;
# the helper must not claim completion or touch UCI/network services.
reset_case
write_journal "$work/sqm-fail.journal" 701 100
export CAKE_RECOVER_TC_KERNEL_DEFAULT=1
export CAKE_RECOVER_SQM_FAIL=1
if "$helper" recover "$work/sqm-fail.journal"; then
	echo 'failed SQM restoration unexpectedly passed recovery' >&2
	exit 1
fi
unset CAKE_RECOVER_SQM_FAIL
grep -q 'managed SQM failed its CAKE, IFB, or redirect recovery postconditions' "$work/error.json"
grep -q '"runtime_restored":false' "$work/error.json"
grep -q '"recovery_pending":true' "$work/error.json"
[ ! -e "$work/done" ]
grep -Fqx "$expected_sqm_call" "$work/actions"
! grep -q 'uci\|network' "$work/actions"
[ -e "$work/sqm-fail.journal.cleanup-done" ]
# This is the exact idempotence boundary seen on real OpenWrt: temporary state
# is already clean, while an earlier SQM restoration attempt failed. A retry
# must not require the random CAKE/IFB topology to still exist.
"$helper" recover "$work/sqm-fail.journal"
unset CAKE_RECOVER_TC_KERNEL_DEFAULT
[ "$(grep -Fxc "$expected_sqm_call" "$work/actions")" -eq 2 ]
[ -e "$work/sqm-restored" ]
[ ! -e "$work/sqm-fail.journal" ]
[ ! -e "$interface_record" ]
grep -q '"runtime_restored":true' "$work/error.json"

# Normal completion is a synchronous postcondition barrier: the worker first
# removes its lock and temporary state, then finalize publishes done and
# disarms the heartbeat.  A result must not be published before this succeeds.
reset_case
write_journal "$work/finalize.journal" 801 100 802 200
write_fake_process 801 100 S /usr/libexec/cake-autorate-rs/autotune wan_sqm eth0 job-run
write_fake_process 802 200 T /usr/sbin/cake-autorated --instance wan_sqm
"$helper" finalize "$work/finalize.journal"
	[ ! -e "$work/done" ]
[ ! -e "$work/heartbeat" ]
[ -e "$work/sqm-restored" ]
grep -q '^kill -CONT 802$' "$work/actions"

# A normal worker must clear and prove the separately supervised speed-test
# group gone before finalize can disarm recovery or publish an apply-ready result.
reset_case
write_journal "$work/finalize-active.journal" 805 105 "" "" 806 206
write_fake_process 805 105 S /usr/libexec/cake-autorate-rs/autotune wan_sqm eth0 job-run
if "$helper" finalize "$work/finalize-active.journal"; then
	echo 'finalize unexpectedly accepted a nonempty active speed-test identity' >&2
	exit 1
fi
grep -q 'active speed-test identity is still present during finalize' "$work/error.json"
[ ! -e "$work/done" ]
[ -e "$work/heartbeat" ]
	[ -e "$interface_record" ]

# Finalize is fail-closed: a worker cannot publish completion while any part of
# its temporary shaper topology is still present, and the armed journal remains
# available to procd recovery.
reset_case
write_journal "$work/finalize-fail.journal" 811 110
write_fake_process 811 110 S /usr/libexec/cake-autorate-rs/autotune wan_sqm eth0 job-run
create_temp_fixture_ifb
if "$helper" finalize "$work/finalize-fail.journal"; then
	echo 'finalize unexpectedly accepted a leftover IFB' >&2
	exit 1
fi
grep -q 'temporary shaper ownership postconditions are not clean during finalize' "$work/error.json"
[ ! -e "$work/done" ]
[ -e "$work/heartbeat" ]

# The procd-facing monitor discovers an armed dead-owner journal without any
# detached watcher.  It is terminated after observing recovery.
reset_case
mkdir -p "$work/monitor-root"
write_journal "$work/monitor-root/orphan.journal" 901 100
CAKE_AUTORATE_RECOVER_POLL_SECONDS=0.05 "$helper" monitor "$work/monitor-root" &
monitor_pid="$!"
attempt=0
while [ "$attempt" -lt 40 ] && [ -e "$work/monitor-root/orphan.journal" ]; do
	sleep 0.05
	attempt=$((attempt + 1))
done
[ ! -e "$work/monitor-root/orphan.journal" ]
grep -q '"runtime_restored":true' "$work/error.json"
kill -TERM "$monitor_pid"
wait "$monitor_pid" 2>/dev/null || true
monitor_pid=""

# procd starts the crash monitor independently of a worker.  Its nested RAM
# root must therefore be created successfully even when no Auto-Tune job has
# created the parent yet (the normal state immediately after boot/cleanup).
missing_monitor_parent="$work/missing-monitor-parent"
missing_monitor_root="$missing_monitor_parent/recovery"
CAKE_AUTORATE_RECOVER_POLL_SECONDS=0.05 "$helper" monitor "$missing_monitor_root" &
monitor_pid="$!"
attempt=0
while [ "$attempt" -lt 40 ] && [ ! -d "$missing_monitor_root" ]; do
	sleep 0.05
	attempt=$((attempt + 1))
done
[ -d "$missing_monitor_parent" ]
[ -d "$missing_monitor_root" ]
[ ! -L "$missing_monitor_parent" ]
[ ! -L "$missing_monitor_root" ]
kill -TERM "$monitor_pid"
wait "$monitor_pid" 2>/dev/null || true
monitor_pid=""

# Static procd contract: scheduler and crash monitor are independent named
# instances, both supervised with respawn.
procd_log="$work/procd.log"
procd_open_instance() { printf 'open %s\n' "$1" >> "$procd_log"; }
procd_set_param() { printf 'param %s\n' "$*" >> "$procd_log"; }
procd_close_instance() { printf 'close\n' >> "$procd_log"; }
. "$test_dir/../root/etc/init.d/cake-autorate-autotune"
start_service
grep -q '^open scheduler$' "$procd_log"
grep -q '^open recovery$' "$procd_log"
grep -q 'autotune-recover monitor /tmp/cake-autorate-autotune/recovery' "$procd_log"

echo 'autotune crash-recovery helper tests passed'
