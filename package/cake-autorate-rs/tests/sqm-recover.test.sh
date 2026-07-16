#!/bin/sh
set -eu

test_dir="$(CDPATH= cd -- "$(dirname "$0")" && pwd)"
helper="$test_dir/../files/usr/libexec/cake-autorate-rs/sqm-recover"
runtime_lock_lib="$test_dir/../files/usr/libexec/cake-autorate-rs/runtime-lock"
fixtures="$test_dir/fixtures/sqm-recover"
work="${TMPDIR:-/tmp}/cake-sqm-recover.$$"
holder_pid=""

fail() {
	printf '%s\n' "$*" >&2
	exit 1
}

cleanup() {
	if [ -n "${holder_pid:-}" ] && kill -0 "$holder_pid" 2>/dev/null; then
		: > "$work/holder-release"
		wait "$holder_pid" 2>/dev/null || true
	fi
	rm -rf "$work"
}
trap cleanup EXIT INT TERM

action_count() {
	wc -l < "$work/actions" | tr -d ' '
}

assert_no_interface_record() {
	[ ! -e "$work/runtime-lock/interface-eth0.lock" ] ||
		fail "runtime interface ownership record was stranded"
}

expect_failure_rc_one() {
	label="$1"
	stdout="$2"
	stderr="$3"
	shift 3
	set +e
	"$@" > "$stdout" 2> "$stderr"
	rc="$?"
	set -e
	[ "$rc" -eq 1 ] || fail "$label returned rc $rc instead of rc 1"
}

mkdir -p "$work/sys/eth0/statistics" "$work/state" "$work/runtime-lock" "$work/config"
: > "$work/sys/eth0/statistics/rx_bytes"
: > "$work/sys/eth0/statistics/tx_bytes"
: > "$work/actions"
: > "$work/uci.log"
: > "$work/uci-config-dirs"
{
	printf "config queue 'cake_wanb_sqm'\n"
	printf "\toption _cake_autorate_managed 'wanb_sqm'\n"
	printf "\toption enabled '1'\n"
	printf "\toption interface 'eth0'\n"
	printf "\toption download '100000'\n"
	printf "\toption upload '20000'\n"
	printf "\toption qdisc 'cake'\n"
	printf "\toption script 'piece_of_cake.qos'\n"
} > "$work/config/sqm"

export CAKE_TEST_ROOT="$work"
export CAKE_TEST_UCI_LOG="$work/uci.log"
export CAKE_AUTORATE_UCI="$fixtures/uci"
export CAKE_AUTORATE_TC="$fixtures/tc"
export CAKE_AUTORATE_SQM_RUN="$fixtures/sqm-run"
export CAKE_AUTORATE_SYS_CLASS_NET="$work/sys"
export CAKE_AUTORATE_SQM_STATE_DIR="$work/state"
export CAKE_AUTORATE_SQM_CONFIG_FILE="$work/config/sqm"
export CAKE_AUTORATE_RUNTIME_LOCK_LIB="$runtime_lock_lib"
export CAKE_AUTORATE_RUNTIME_LOCK_ROOT="$work/runtime-lock"
export CAKE_SQM_RECOVER_HELPER="$helper"
unset CAKE_AUTORATE_RUNTIME_GLOBAL_LOCK_FD \
	CAKE_AUTORATE_INTERFACE_LOCK_OWNER CAKE_AUTORATE_INTERFACE_LOCK_STARTTIME \
	CAKE_AUTORATE_INTERFACE_LOCK_TOKEN CAKE_AUTORATE_INTERFACE_LOCK_RECOVERY_JOURNAL \
	CAKE_AUTORATE_INTERFACE_LOCK_FD

# Standalone recovery reads its mutable CAKE UCI configuration, repairs SQM,
# and releases both ownership layers after it exits.
"$helper" wanb_sqm
[ -f "$work/healthy" ] || fail "standalone recovery did not restore health"
grep -qx 'IFACE="eth0"' "$work/state/eth0.state" ||
	fail "standalone recovery did not replace stale SQM state"
[ "$(sed -n '1p' "$work/actions")" = "start eth0" ]
assert_no_interface_record

"$helper" wanb_sqm
[ "$(action_count)" -eq 1 ] || fail "healthy SQM was restarted"
assert_no_interface_record

# Production accepts only standalone health (one argument) and a complete
# seven-argument immutable restore.  No released RC16 package wrote a compatible
# recovery journal, so accepting weaker five/six-argument records would only
# create an unauthenticated downgrade path.
expect_failure_rc_one "legacy five-argument recovery" "$work/legacy-five.out" \
	"$work/legacy-five.err" \
	"$helper" wanb_sqm cake_wanb_sqm eth0 eth0 ifb4eth0
grep -q 'complete recovery snapshot' "$work/legacy-five.err"
[ "$(action_count)" -eq 1 ]

expect_failure_rc_one "legacy six-argument recovery" "$work/legacy-six.out" \
	"$work/legacy-six.err" \
	"$helper" wanb_sqm cake_wanb_sqm eth0 eth0 ifb4eth0 \
	sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
grep -q 'complete recovery snapshot' "$work/legacy-six.err"
[ "$(action_count)" -eq 1 ]

expect_failure_rc_one "malformed strict recovery fingerprint" \
	"$work/bad-fingerprint.out" "$work/bad-fingerprint.err" \
	"$helper" wanb_sqm cake_wanb_sqm eth0 eth0 ifb4eth0 sha256:ABC \
	"$work/runtime-lock/sqm-snapshot.missing"
grep -q 'Invalid managed SQM recovery fingerprint' "$work/bad-fingerprint.err"
zero_fingerprint="sha256:$(printf '%064d' 0)"

# Health must mean exact root CAKE on both devices and an exact IFB redirect.
# Child qdiscs, prefix matches, and unreadable tc state all require recovery.
exercise_bad_health() {
	mode="$1"
	before="$(action_count)"
	printf '%s\n' "$mode" > "$work/tc-mode"
	"$helper" wanb_sqm
	after="$(action_count)"
	[ "$after" -eq $((before + 2)) ] ||
		fail "$mode was incorrectly accepted as healthy"
	[ ! -e "$work/tc-mode" ] || fail "$mode did not pass through SQM restart"
	assert_no_interface_record
}

exercise_bad_health dl-child-cake
exercise_bad_health ul-child-cake
exercise_bad_health wrong-redirect
exercise_bad_health qdisc-read-fail
exercise_bad_health filter-read-fail
exercise_bad_health wrong-bandwidth
exercise_bad_health wrong-options
exercise_bad_health bad-handle
exercise_bad_health wrong-linklayer
exercise_bad_health missing-ingress

# A state file is sourced by sqm-run stop, so symlinks and malformed or
# foreign contents must fail closed without invoking the runner.  A missing
# state is safe: recovery can skip stop and start from the frozen UCI snapshot.
before="$(action_count)"
rm -f "$work/state/eth0.state"
: > "$work/foreign-state-target"
ln -s "$work/foreign-state-target" "$work/state/eth0.state"
expect_failure_rc_one "symlink SQM state" "$work/state-symlink.out" \
	"$work/state-symlink.err" "$helper" wanb_sqm
grep -q 'state is a symlink' "$work/state-symlink.err"
[ "$(action_count)" -eq "$before" ] || fail "symlink SQM state reached sqm-run"
[ -L "$work/state/eth0.state" ] || fail "symlink SQM state was unexpectedly changed"
rm -f "$work/state/eth0.state"
"$helper" wanb_sqm
[ "$(action_count)" -eq $((before + 1)) ] || fail "missing-state recovery did not start SQM once"

before="$(action_count)"
printf 'IFACE="eth0"\nIFACE="eth9"\n' > "$work/state/eth0.state"
expect_failure_rc_one "duplicate SQM state" "$work/state-duplicate.out" \
	"$work/state-duplicate.err" "$helper" wanb_sqm
grep -q 'state is malformed' "$work/state-duplicate.err"
[ "$(action_count)" -eq "$before" ] || fail "duplicate SQM state reached sqm-run"
rm -f "$work/state/eth0.state"
"$helper" wanb_sqm
[ "$(action_count)" -eq $((before + 1)) ] || fail "duplicate-state recovery setup failed"

before="$(action_count)"
sed -i 's/DOWNLINK="100000"/DOWNLINK="99999"/' "$work/state/eth0.state"
expect_failure_rc_one "foreign SQM state" "$work/state-foreign.out" \
	"$work/state-foreign.err" "$helper" wanb_sqm
grep -q 'does not match frozen UCI' "$work/state-foreign.err"
[ "$(action_count)" -eq "$before" ] || fail "foreign SQM state reached sqm-run"
rm -f "$work/state/eth0.state"
"$helper" wanb_sqm
[ "$(action_count)" -eq $((before + 1)) ] || fail "foreign-state recovery setup failed"

# A managed-SQM ownership/configuration change between the first snapshot and
# the post-flock revalidation must fail before stop/start and release its record.
: > "$work/uci.log"
before="$(action_count)"
set +e
CAKE_TEST_SQM_CHANGE_AFTER_INITIAL=1 "$helper" wanb_sqm \
	> "$work/config-race.out" 2> "$work/config-race.err"
rc="$?"
set -e
[ "$rc" -eq 1 ] || fail "post-lock configuration race returned rc $rc"
grep -q 'changed while locking' "$work/config-race.err"
[ "$(action_count)" -eq "$before" ] || fail "configuration race mutated SQM"
assert_no_interface_record
: > "$work/uci.log"

# Failure after lock acquisition must still release the interface record and
# global kernel lock through the EXIT trap.
rm -f "$work/healthy" "$work/sys/ifb4eth0/statistics/tx_bytes"
set +e
CAKE_TEST_SQM_START_FAIL=1 "$helper" wanb_sqm \
	> "$work/start-fail.out" 2> "$work/start-fail.err"
rc="$?"
set -e
[ "$rc" -eq 1 ] || fail "failed SQM start returned rc $rc"
grep -q 'Failed to start managed SQM' "$work/start-fail.err"
assert_no_interface_record
sh -c '. "$CAKE_AUTORATE_RUNTIME_LOCK_LIB"; runtime_lock_acquire_global_exclusive; runtime_lock_release_global'
"$helper" wanb_sqm
assert_no_interface_record

# A real runtime-lock collision is retryable (rc 75), and must not invoke SQM.
before="$(action_count)"
CAKE_HOLDER_READY="$work/holder-ready" \
CAKE_HOLDER_RELEASE="$work/holder-release" \
sh -c '
	set -eu
	. "$CAKE_AUTORATE_RUNTIME_LOCK_LIB"
	runtime_lock_acquire_global_shared
	runtime_lock_acquire_interface eth0 fixture-holder "" holdertoken
	trap '\''runtime_lock_release_interface >/dev/null 2>&1 || true; runtime_lock_release_global >/dev/null 2>&1 || true'\'' EXIT
	: > "$CAKE_HOLDER_READY"
	while [ ! -e "$CAKE_HOLDER_RELEASE" ]; do sleep 1; done
' &
holder_pid="$!"
attempt=0
while [ ! -e "$work/holder-ready" ]; do
	kill -0 "$holder_pid" 2>/dev/null || fail "runtime-lock holder exited before becoming ready"
	attempt=$((attempt + 1))
	[ "$attempt" -lt 5 ] || fail "runtime-lock holder did not become ready"
	sleep 1
done

set +e
"$helper" wanb_sqm > "$work/deferred.out" 2> "$work/deferred.err"
rc="$?"
set -e
[ "$rc" -eq 75 ] || fail "standalone lock collision returned rc $rc instead of 75"
grep -q 'recovery deferred' "$work/deferred.err"
[ "$(action_count)" -eq "$before" ] || fail "busy recovery changed SQM"
: > "$work/holder-release"
wait "$holder_pid"
holder_pid=""
assert_no_interface_record

# Acquire the coordinator lock in this process. Children inherit FDs 8/9 and
# may borrow them only when every identity field and journal matches the record.
# shellcheck source=../files/usr/libexec/cake-autorate-rs/runtime-lock
. "$runtime_lock_lib"

# Scheduler apply keeps one global exclusive OFD across commit, restart, and
# exact health verification.  The child helper may authenticate and borrow it,
# then acquire only its own interface lock; it must neither deadlock on a new
# shared open nor downgrade/release the parent's transaction lock.
runtime_lock_acquire_global_exclusive
before="$(action_count)"
CAKE_AUTORATE_RUNTIME_GLOBAL_LOCK_BORROW=1 "$helper" wanb_sqm
[ "$(action_count)" -eq "$before" ] || fail "scheduler-borrowed health check restarted healthy SQM"
if "$runtime_lock_flock_bin" -sn "$work/runtime-lock/runtime.guard" true; then
	fail "scheduler-borrowed health check downgraded parent exclusive lock"
fi
set +e
"$helper" wanb_sqm > "$work/no-borrow.out" 2> "$work/no-borrow.err"
rc="$?"
set -e
[ "$rc" -eq 75 ] || fail "health check without scheduler borrow returned rc $rc"
[ "$(action_count)" -eq "$before" ] || fail "non-borrowing health check mutated SQM"
runtime_lock_release_global

set +e
sh -c '
	set -eu
	. "$CAKE_AUTORATE_RUNTIME_LOCK_LIB"
	runtime_lock_prepare_root
	exec 8>>"$CAKE_AUTORATE_RUNTIME_LOCK_ROOT/runtime.guard"
	export CAKE_AUTORATE_RUNTIME_GLOBAL_LOCK_FD=8
	export CAKE_AUTORATE_RUNTIME_GLOBAL_LOCK_MODE=exclusive
	CAKE_AUTORATE_RUNTIME_GLOBAL_LOCK_BORROW=1 "$CAKE_SQM_RECOVER_HELPER" wanb_sqm
' > "$work/spoof-global.out" 2> "$work/spoof-global.err"
rc="$?"
set -e
[ "$rc" -eq 1 ] || fail "unlocked scheduler global descriptor returned rc $rc"
grep -q 'Unable to borrow the scheduler-owned exclusive' "$work/spoof-global.err"
[ "$(action_count)" -eq "$before" ] || fail "spoofed scheduler descriptor mutated SQM"

# The caller captures this full UCI file while it owns the interface lock in
# production.  The fixture creates it here before exercising the inherited-lock
# authentication path below.
sqm_hash="$("$fixtures/uci" -q show sqm.cake_wanb_sqm | LC_ALL=C sort | \
	sha256sum | awk 'NR == 1 { print $1; exit }')"
sqm_fingerprint="sha256:$sqm_hash"
snapshot="$work/runtime-lock/sqm-snapshot.fixturetoken"
cp "$work/config/sqm" "$snapshot"
chmod 600 "$snapshot"

expect_failure_rc_one "unlocked strict recovery" "$work/unlocked.out" \
	"$work/unlocked.err" "$helper" wanb_sqm cake_wanb_sqm eth0 eth0 \
	ifb4eth0 "$sqm_fingerprint" "$snapshot"
grep -q 'requires an inherited, verified runtime lock' "$work/unlocked.err"

journal="$work/autotune-journal"
: > "$journal"
runtime_lock_acquire_global_shared
runtime_lock_acquire_interface eth0 autotune "$journal" fixturetoken
owner="$CAKE_AUTORATE_INTERFACE_LOCK_OWNER"
starttime="$CAKE_AUTORATE_INTERFACE_LOCK_STARTTIME"
token="$CAKE_AUTORATE_INTERFACE_LOCK_TOKEN"
record="$work/runtime-lock/interface-eth0.lock"
[ -f "$record" ] || fail "coordinator ownership record was not created"

set +e
CAKE_AUTORATE_INTERFACE_LOCK_TOKEN=wrongtoken \
	"$helper" wanb_sqm cake_wanb_sqm eth0 eth0 ifb4eth0 \
	"$sqm_fingerprint" "$snapshot" \
	> "$work/bad-token.out" 2> "$work/bad-token.err"
rc="$?"
set -e
[ "$rc" -eq 1 ] || fail "bad inherited token returned rc $rc"
grep -q 'does not match SQM recovery ownership' "$work/bad-token.err"

set +e
CAKE_AUTORATE_INTERFACE_LOCK_RECOVERY_JOURNAL="$work/wrong-journal" \
	"$helper" wanb_sqm cake_wanb_sqm eth0 eth0 ifb4eth0 \
	"$sqm_fingerprint" "$snapshot" \
	> "$work/bad-journal.out" 2> "$work/bad-journal.err"
rc="$?"
set -e
[ "$rc" -eq 1 ] || fail "bad inherited journal returned rc $rc"
grep -q 'does not match SQM recovery ownership' "$work/bad-journal.err"

set +e
CAKE_AUTORATE_INTERFACE_LOCK_STARTTIME= \
	"$helper" wanb_sqm cake_wanb_sqm eth0 eth0 ifb4eth0 \
	"$sqm_fingerprint" "$snapshot" \
	> "$work/incomplete.out" 2> "$work/incomplete.err"
rc="$?"
set -e
[ "$rc" -eq 1 ] || fail "incomplete inherited metadata returned rc $rc"
grep -q 'metadata is incomplete' "$work/incomplete.err"
[ -f "$record" ] || fail "failed borrow removed the coordinator record"

# RC17 recovery carries the actual pre-mutation UCI data as well as its
# canonical section hash.  It must be a private direct child of the root-owned
# tmpfs runtime directory; live SQM and cake-autorate UCI may drift after stop
# and therefore must not participate in strict recovery at all.
outside_snapshot="$work/outside-snapshot"
cp "$work/config/sqm" "$outside_snapshot"
chmod 600 "$outside_snapshot"
expect_failure_rc_one "outside recovery snapshot" "$work/snapshot-outside.out" \
	"$work/snapshot-outside.err" "$helper" wanb_sqm cake_wanb_sqm eth0 eth0 \
	ifb4eth0 "$sqm_fingerprint" "$outside_snapshot"
grep -q 'snapshot is missing, unsafe' "$work/snapshot-outside.err"

private_target="$work/runtime-lock/snapshot-target"
cp "$work/config/sqm" "$private_target"
chmod 600 "$private_target"
ln -s "$private_target" "$work/runtime-lock/sqm-snapshot.symlink"
expect_failure_rc_one "symlink recovery snapshot" "$work/snapshot-symlink.out" \
	"$work/snapshot-symlink.err" "$helper" wanb_sqm cake_wanb_sqm eth0 eth0 \
	ifb4eth0 "$sqm_fingerprint" "$work/runtime-lock/sqm-snapshot.symlink"
grep -q 'snapshot is missing, unsafe' "$work/snapshot-symlink.err"

mode_snapshot="$work/runtime-lock/sqm-snapshot.badmode"
cp "$work/config/sqm" "$mode_snapshot"
chmod 644 "$mode_snapshot"
expect_failure_rc_one "public recovery snapshot" "$work/snapshot-mode.out" \
	"$work/snapshot-mode.err" "$helper" wanb_sqm cake_wanb_sqm eth0 eth0 \
	ifb4eth0 "$sqm_fingerprint" "$mode_snapshot"
grep -q 'snapshot is missing, unsafe' "$work/snapshot-mode.err"

if [ "$(id -u)" -eq 0 ]; then
	owner_snapshot="$work/runtime-lock/sqm-snapshot.foreignowner"
	cp "$work/config/sqm" "$owner_snapshot"
	chmod 600 "$owner_snapshot"
	if chown 65534 "$owner_snapshot" 2>/dev/null; then
		expect_failure_rc_one "foreign-owner recovery snapshot" \
			"$work/snapshot-owner.out" "$work/snapshot-owner.err" \
			"$helper" wanb_sqm cake_wanb_sqm eth0 eth0 ifb4eth0 \
			"$sqm_fingerprint" "$owner_snapshot"
		grep -q 'snapshot is missing, unsafe' "$work/snapshot-owner.err"
	fi
	rm -f "$owner_snapshot"
fi

before="$(action_count)"
expect_failure_rc_one "snapshot fingerprint mismatch" \
	"$work/snapshot-fingerprint.out" "$work/snapshot-fingerprint.err" \
	"$helper" wanb_sqm cake_wanb_sqm eth0 eth0 ifb4eth0 "$zero_fingerprint" "$snapshot"
grep -q 'could not be frozen safely' "$work/snapshot-fingerprint.err"
[ "$(action_count)" -eq "$before" ] || fail "snapshot hash mismatch mutated SQM"
[ -f "$snapshot" ] || fail "failed strict recovery consumed its caller snapshot"

: > "$work/strict-uci-context.log"
before="$(action_count)"
CAKE_TEST_UCI_CONTEXT_LOG="$work/strict-uci-context.log" \
CAKE_TEST_MUTABLE_CAKE_UCI=1 CAKE_TEST_LIVE_SQM_SHOW_CHANGED=1 \
	"$helper" wanb_sqm cake_wanb_sqm eth0 eth0 ifb4eth0 \
	"$sqm_fingerprint" "$snapshot"
[ "$(action_count)" -eq "$before" ] || fail "healthy strict recovery restarted SQM"
[ -f "$snapshot" ] || fail "helper consumed snapshot before caller journal commit"
if grep -q '|LIVE|' "$work/strict-uci-context.log"; then
	fail "strict recovery consulted mutable live UCI"
fi

# Simulate the precise crash window: SQM is unhealthy and live UCI has changed
# after the snapshot was journalled.  Recovery must still use the old snapshot,
# restore the old shaper exactly, and leave both live UCI and snapshot alone.
sed -i "s/option upload '20000'/option upload '21000'/" "$work/config/sqm"
rm -f "$work/healthy" "$work/sys/ifb4eth0/statistics/tx_bytes"
: > "$work/strict-uci-context.log"
before="$(action_count)"
CAKE_TEST_UCI_CONTEXT_LOG="$work/strict-uci-context.log" \
CAKE_TEST_MUTABLE_CAKE_UCI=1 CAKE_TEST_LIVE_SQM_SHOW_CHANGED=1 \
	"$helper" wanb_sqm cake_wanb_sqm eth0 eth0 ifb4eth0 \
	"$sqm_fingerprint" "$snapshot"
[ "$(action_count)" -eq $((before + 2)) ] ||
	fail "strict snapshot recovery did not restore SQM after live UCI drift"
grep -q "option upload '21000'" "$work/config/sqm" ||
	fail "strict recovery rewrote mutable live UCI"
[ -f "$snapshot" ] || fail "successful helper consumed snapshot before caller journal commit"
if grep -q '|LIVE|' "$work/strict-uci-context.log"; then
	fail "strict recovery consulted live UCI after drift"
fi
sed -i "s/option upload '21000'/option upload '20000'/" "$work/config/sqm"
# Model the caller's crash-safe order: it would first commit required=0 to its
# journal and only then remove this file.  The helper deliberately never does.
rm -f "$outside_snapshot" "$private_target" \
	"$work/runtime-lock/sqm-snapshot.symlink" "$mode_snapshot"

: > "$work/uci.log"
rm -f "$work/healthy" "$work/sys/ifb4eth0/statistics/tx_bytes"
before="$(action_count)"
CAKE_TEST_MUTABLE_CAKE_UCI=1 \
	"$helper" wanb_sqm cake_wanb_sqm eth0 eth0 ifb4eth0 \
	"$sqm_fingerprint" "$snapshot"
[ "$(action_count)" -eq $((before + 2)) ] || fail "borrowed recovery did not restart SQM"
grep -qx 'IFACE="eth0"' "$work/state/eth0.state" ||
	fail "borrowed recovery did not replace stale SQM state"
if grep -q '^cake-autorate\.' "$work/uci.log"; then
	fail "immutable recovery read mutable cake-autorate UCI fields"
fi
[ "$(grep -c '^sqm\.cake_wanb_sqm\.' "$work/uci.log")" -ge 9 ] ||
	fail "recovery did not validate frozen managed SQM ownership"
grep -qx "pid=$owner" "$record"
grep -qx "proc_starttime=$starttime" "$record"
grep -qx "token=$token" "$record"
grep -qx "recovery_journal=$journal" "$record"

# The borrower must leave ownership with the coordinator; the coordinator then
# commits restore-required=0, removes the snapshot, then releases the locks.
[ -f "$snapshot" ] || fail "successful borrowed recovery consumed caller snapshot"
rm -f "$snapshot"
runtime_lock_release_interface
runtime_lock_release_global
assert_no_interface_record
runtime_lock_acquire_global_exclusive
runtime_lock_release_global

# Standalone mode still honors mutable CAKE UCI enablement.
set +e
CAKE_TEST_MUTABLE_CAKE_UCI=1 "$helper" wanb_sqm \
	> "$work/standalone-disabled.out" 2> "$work/standalone-disabled.err"
rc="$?"
set -e
[ "$rc" -eq 1 ] || fail "standalone disabled instance returned rc $rc"
grep -q 'is disabled' "$work/standalone-disabled.err"
assert_no_interface_record

if "$helper" '../bad' > /dev/null 2>&1; then
	fail "invalid instance name unexpectedly passed"
fi

while IFS= read -r private_dir; do
	[ -n "$private_dir" ] || continue
	case "$private_dir" in "$work/runtime-lock"/sqm-recover-config.*) ;; *) fail "sqm-run received an unsafe UCI_CONFIG_DIR" ;; esac
	[ ! -e "$private_dir" ] || fail "private SQM UCI snapshot was not cleaned"
done < "$work/uci-config-dirs"

echo "sqm-recover helper tests passed"
