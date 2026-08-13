#!/bin/sh

set -eu

test_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
shipped_helper="$test_dir/../root/usr/libexec/cake-autorate-rs/rpcd-helper"
target="$test_dir/fixtures/rpcd-helper-target"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT INT TERM
helper="$work/rpcd-helper"
sed \
	-e "s#speedtest_bin=\"/usr/libexec/cake-autorate-rs/speedtest\"#speedtest_bin=\"$target\"#" \
	-e "s#autotune_bin=\"/usr/libexec/cake-autorate-rs/autotune\"#autotune_bin=\"$target\"#" \
	-e "s#apply_guard_bin=\"/usr/libexec/cake-autorate-rs/apply-guard\"#apply_guard_bin=\"$target\"#" \
	"$shipped_helper" > "$helper"
chmod 700 "$helper"

fail() {
	printf 'rpcd-helper test failed: %s\n' "$1" >&2
	exit 1
}

run_helper() {
	"$helper" "$@"
}

assert_dispatch() {
	operation="$1"
	expected_mode="$2"
	shift 2
	output="$(run_helper "$operation" "$@")" || fail "$operation was rejected"
	printf '%s\n' "$output" | grep -Fx "argc=$#" >/dev/null ||
		fail "$operation changed the argument count"
	printf '%s\n' "$output" | grep -Fx "arg3=<$expected_mode>" >/dev/null ||
		fail "$operation did not preserve its pinned legacy mode"
}

assert_rejected() {
	if run_helper "$@" >/dev/null 2>&1; then
		fail "unsafe request was accepted: $1"
	fi
}

assert_apply_dispatch() {
	operation="$1"
	expected_mode="$2"
	shift 2
	expected_count=$(($# + 1))
	output="$(run_helper "$operation" "$@")" || fail "$operation was rejected"
	printf '%s\n' "$output" | grep -Fx "argc=$expected_count" >/dev/null ||
		fail "$operation changed the argument count"
	printf '%s\n' "$output" | grep -Fx "arg1=<$expected_mode>" >/dev/null ||
		fail "$operation did not preserve its pinned Apply Guard mode"
	argument_index=2
	for expected_argument in "$@"; do
		printf '%s\n' "$output" | grep -Fx "arg$argument_index=<$expected_argument>" >/dev/null ||
			fail "$operation changed Apply Guard argument $argument_index"
		argument_index=$((argument_index + 1))
	done
}

assert_dispatch speedtest-job-start job-start \
	test_instance eth0 job-start speedtest-go '' main ''
assert_dispatch speedtest-job-status job-status \
	test_instance eth0 job-status speedtest-go
assert_dispatch speedtest-status status \
	test_instance eth0 status auto
assert_dispatch speedtest-install install \
	test_instance eth0 install speedtest-go

assert_dispatch autotune-start start \
	test_instance eth0 start speedtest-go main '' best_overall 0 '' 0 \
	shaped_only unknown user_selected 100 verified_only '' ''
assert_dispatch autotune-start-conservative start-conservative \
	test_instance eth0 start-conservative speedtest-go main '' best_overall 1 '' 0 \
	shaped_only unknown user_selected 100 verified_only '' ''
assert_dispatch autotune-status-summary status-summary \
	test_instance eth0 status-summary speedtest-go main '' best_overall 0 '' 0 \
	shaped_only unknown user_selected 100 verified_only '' ''
assert_dispatch autotune-result result \
	test_instance eth0 result speedtest-go main '' best_overall 0 '' 0 \
	shaped_only unknown user_selected 100 verified_only '' ''
assert_dispatch autotune-cancel cancel \
	test_instance eth0 cancel speedtest-go '' '' best_overall
assert_dispatch autotune-status status \
	test_instance eth0 status speedtest-go main '' best_overall
assert_dispatch autotune-attest attest \
	test_instance eth0 attest speedtest-go main '' best_overall

token=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
fingerprint=sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
proposal=p-cccccccccccccccccccccccc
assert_apply_dispatch apply-guard-arm arm \
	test_instance eth0 speedtest-go main '' 1 0 apply_sqm "$fingerprint" "$proposal"
assert_apply_dispatch apply-guard-abort abort "$token"
assert_apply_dispatch apply-guard-verify-rollback verify-rollback "$token"
assert_apply_dispatch apply-guard-finalize finalize "$token"
assert_apply_dispatch apply-guard-reconcile reconcile "$token"
assert_apply_dispatch apply-guard-status status "$token"

assert_rejected speedtest-job-start test_instance eth0 job-run speedtest-go '' main ''
assert_rejected speedtest-job-start test_instance eth0 job-start speedtest-go attacker main ''
assert_rejected speedtest-job-start test_instance eth0 job-start speedtest-go '' main '' extra
assert_rejected speedtest-job-start test_instance eth0 job-start unsupported '' main ''
assert_rejected speedtest-job-start test_instance eth0
assert_rejected speedtest-job-start 'test instance' eth0 job-start speedtest-go '' main ''
assert_rejected speedtest-job-start test_instance eth0 job-start speedtest-go '' invalid ''
assert_rejected speedtest-job-start test_instance eth0 job-start speedtest-go '' mwan3 'bad/member'
assert_rejected autotune-status test_instance eth0 history speedtest-go main '' best_overall
assert_rejected autotune-start test_instance eth0 start speedtest-go main '' best_overall 0 attacker 0 \
	shaped_only unknown user_selected 100 verified_only '' ''
assert_rejected autotune-start test_instance eth0 start speedtest-go main '' best_overall 0 '' 1 \
	shaped_only unknown user_selected 100 verified_only '' ''
assert_rejected autotune-start test_instance eth0 start speedtest-go main '' best_overall 0 '' 0 \
	invalid unknown user_selected 100 verified_only '' ''
assert_rejected autotune-start test_instance eth0 start speedtest-go main '' best_overall 0 '' 0 \
	shaped_only unknown user_selected 101 verified_only '' ''
assert_rejected autotune-start test_instance eth0 start speedtest-go main '' best_overall 0 '' 0 \
	shaped_only unknown user_selected 100 verified_only bad-cap ''
assert_rejected autotune-worker test_instance eth0 job-run speedtest-go main '' best_overall
assert_rejected apply-guard-arm test_instance eth0 speedtest-go main '' 1 0 apply_sqm "$fingerprint"
assert_rejected apply-guard-arm 'test instance' eth0 speedtest-go main '' 1 0 apply_sqm "$fingerprint" "$proposal"
assert_rejected apply-guard-arm test_instance eth0 librespeed-cli main '' 1 0 apply_sqm "$fingerprint" "$proposal"
assert_rejected apply-guard-arm test_instance eth0 speedtest-go main wanb 1 0 apply_sqm "$fingerprint" "$proposal"
assert_rejected apply-guard-arm test_instance eth0 speedtest-go mwan3 '' 1 0 apply_sqm "$fingerprint" "$proposal"
assert_rejected apply-guard-arm test_instance eth0 speedtest-go main '' 0 0 apply_sqm "$fingerprint" "$proposal"
assert_rejected apply-guard-arm test_instance eth0 speedtest-go main '' 1 0 disable_sqm "$fingerprint" "$proposal"
assert_rejected apply-guard-arm test_instance eth0 speedtest-go main '' 1 0 apply_sqm sha256:bad "$proposal"
assert_rejected apply-guard-arm test_instance eth0 speedtest-go main '' 1 0 apply_sqm \
	bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb "$proposal"
assert_rejected apply-guard-arm test_instance eth0 speedtest-go main '' 1 0 apply_sqm "$fingerprint" p-bad
assert_rejected apply-guard-arm test_instance eth0 speedtest-go main '' 1 0 apply_sqm "$fingerprint" \
	cccccccccccccccccccccccc
assert_rejected apply-guard-status
assert_rejected apply-guard-status bad
assert_rejected apply-guard-status "$token" extra
assert_rejected apply-guard-verify-init
assert_rejected apply-guard-recover-stale "$token"
assert_rejected apply-guard-supervise "$token"
assert_rejected apply-guard-postcheck "$token"
assert_rejected apply-guard-prepare-confirm "$token"

oversized="$(printf '%0257d' 0)"
assert_rejected speedtest-status "$oversized" eth0 status speedtest-go

override_output="$(CAKE_AUTORATE_RPCD_HELPER_TESTING=1 \
	CAKE_AUTORATE_RPCD_SPEEDTEST=/bin/false run_helper \
	speedtest-status test_instance eth0 status speedtest-go)" ||
	fail "caller environment changed the fixed speed-test target"
printf '%s\n' "$override_output" | grep -Fx 'arg3=<status>' >/dev/null ||
	fail "environment override regression did not reach the fixed test target"

override_output="$(CAKE_AUTORATE_RPCD_APPLY_GUARD=/bin/false run_helper \
	apply-guard-status "$token")" ||
	fail "caller environment changed the fixed Apply Guard target"
printf '%s\n' "$override_output" | grep -Fx 'arg1=<status>' >/dev/null ||
	fail "Apply Guard environment override regression did not reach the fixed test target"

if grep -q 'CAKE_AUTORATE_RPCD_' "$shipped_helper"; then
	fail "shipped rpcd helper retains an environment-controlled target seam"
fi

printf 'rpcd-helper tests passed\n'
