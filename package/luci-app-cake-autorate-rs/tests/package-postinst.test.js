'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const makefile = fs.readFileSync(path.join(__dirname, '..', 'Makefile'), 'utf8');
const daemonMakefile = fs.readFileSync(path.join(__dirname, '..', '..', 'cake-autorate-rs',
	'Makefile'), 'utf8');
const daemonMain = fs.readFileSync(path.join(__dirname, '..', '..', 'cake-autorate-rs', 'src',
	'src', 'main.rs'), 'utf8');
const autotuneInit = fs.readFileSync(path.join(__dirname, '..', 'root', 'etc', 'init.d',
	'cake-autorate-autotune'), 'utf8');
const procdJob = fs.readFileSync(path.join(__dirname, '..', 'root', 'usr', 'libexec',
	'cake-autorate-rs', 'procd-job'), 'utf8');
const rpcdHelper = fs.readFileSync(path.join(__dirname, '..', 'root', 'usr', 'libexec',
	'cake-autorate-rs', 'rpcd-helper'), 'utf8');
const schedulerSeeder = fs.readFileSync(path.join(__dirname, '..', '..', 'cake-autorate-rs',
	'files', 'etc', 'uci-defaults', '99-cake-autorate-rs-scheduler-engine'), 'utf8');
const retiredQualityTest = path.join(__dirname, '..', 'root', 'usr', 'libexec',
	'cake-autorate-rs', 'quality-test');

const initCapability = autotuneInit.match(
	/CALIBRATION_CAPABILITIES_V3="([^"]+)"/,
);
const daemonCapability = daemonMain.match(
	/const CALIBRATION_CAPABILITIES_V3: &str =\s*"([^"]+)";/,
);
const seederCapability = schedulerSeeder.match(
	/CALIBRATION_CAPABILITIES_V3="([^"]+)"/,
);
assert(initCapability && daemonCapability && seederCapability,
	'versioned calibration capability declarations are missing');
assert.equal(initCapability[1], daemonCapability[1],
	'init and daemon calibration capability protocols must remain byte-identical');
assert.equal(seederCapability[1], daemonCapability[1],
	'fresh-install scheduler seeding must use the exact daemon capability protocol');

assert.match(makefile, /rm -f \/tmp\/luci-indexcache \/tmp\/luci-indexcache\.\*/);
assert.match(makefile, /rm -rf \/tmp\/luci-modulecache \/tmp\/luci-modulecache\.\*/);
assert.match(makefile, /\/etc\/init\.d\/rpcd reload >\/dev\/null 2>&1 \|\| true/);
assert.match(makefile, /DEPENDS:=.*\+cake-autorate-rs-full/,
	'the Full-only daemon capability must be installed before LuCI starts calibration services');
assert.match(makefile, /\/etc\/init\.d\/cake-autorate-apply-guard disable >\/dev\/null 2>&1 \|\| true/,
	'the internal rollback supervisor must remain boot-disabled and start only for a verified transaction');
assert.match(makefile, /define Package\/luci-app-cake-autorate-rs\/prerm/,
	'the Full package must own explicit removal lifecycle cleanup');
assert.match(makefile, /cake-autorate-autotune stop >\/dev\/null 2>&1 \|\| true/,
	'removing Full must stop its calibration service before the init script disappears');
assert.match(makefile, /rm -f \/etc\/rc\.d\/S\*cake-autorate-apply-guard \/etc\/rc\.d\/K\*cake-autorate-apply-guard/,
	'removing Full must not leave dangling rollback-supervisor rc links');
assert.doesNotMatch(makefile, /\/etc\/init\.d\/cake-autorate-apply-guard enable/,
	'enabling the token-driven helper would emit a false error on every ordinary boot');
assert.doesNotMatch(makefile, /rm -f \/tmp\/luci-indexcache\.\*;/,
	'the unsuffixed LuCI index cache must not survive package replacement');
assert.match(makefile,
	/\$\(RM\) \$\(1\)\/usr\/libexec\/cake-autorate-rs\/quality-test/,
	'the package recipe must retain a defensive exclusion for the retired shell Rating worker');
assert.equal(fs.existsSync(retiredQualityTest), false,
	'the retired shell Rating worker body must not remain in the source payload');
assert.doesNotMatch(procdJob, /\/usr\/libexec\/cake-autorate-rs\/quality-test/,
	'the procd launcher must not retain an allowlist entry for an unshipped worker');
assert.match(rpcdHelper, /case "\$operation" in/,
	'legacy LuCI fallback operations must pass through the positionally pinned rpcd dispatcher');
assert.doesNotMatch(rpcdHelper, /eval|sh -c|\$\{operation\}.*exec/,
	'the rpcd dispatcher must not evaluate or dynamically construct commands');
assert.doesNotMatch(rpcdHelper, /CAKE_AUTORATE_RPCD_/,
	'the shipped rpcd dispatcher must not expose environment-controlled executable targets');
assert.match(autotuneInit, /procd_open_instance coordinator/,
	'the existing Auto-Tune service must supervise the single-binary Rust coordinator');
assert.match(autotuneInit, /DAEMON="\$\{CAKE_AUTORATE_DAEMON:-\/usr\/sbin\/cake-autorated\}"/,
	'the coordinator harness override must retain the production daemon as its exact default');
assert.match(autotuneInit,
	/procd_set_param command "\$DAEMON" --calibrationd --native-rating --native-speedtest --native-autotune/,
	'production Rating, Speed Test and Full Auto-Tune must be admitted only by the native Rust coordinator');
assert.doesNotMatch(daemonMain, /--lab-legacy-adapter|CAKE_AUTORATE_ENABLE_LAB_LEGACY_ADAPTER/,
	'the retired Rust lab legacy adapter must not remain in the production daemon source');
assert.doesNotMatch(autotuneInit, /--lab-legacy-adapter|CAKE_AUTORATE_ENABLE_LAB_LEGACY_ADAPTER/,
	'the production init topology must never revive the retired Rust lab legacy adapter');
assert.match(autotuneInit,
	/--native-scheduler --scheduler-store-dir "\$NATIVE_SCHEDULER_STORE"/,
	'the production native scheduler must share the one Rust coordinator and use the persistent ledger');
assert.match(autotuneInit,
	/NATIVE_SCHEDULER_STORE="\/etc\/cake-autorate-rs-scheduler"/,
	'the scheduler ledger must use the exact bounded persistent OpenWrt path');
assert.match(autotuneInit,
	/"\$DAEMON" --calibration-capabilities 2>\/dev\/null/,
	'an upgrade must use the versioned machine capability protocol before adding production flags');
assert.doesNotMatch(autotuneInit, /--help 2>&1 \| grep/,
	'usage prose must never authorize production scheduler ownership');
assert.match(autotuneInit, /config_get engine globals autotune_scheduler_engine\n/,
	'the init path must require the explicit scheduler ownership option');
assert.doesNotMatch(autotuneInit,
	/config_get engine globals autotune_scheduler_engine legacy/,
	'a missing ownership option must not silently revert a persistent native ledger to legacy');
assert.match(daemonMakefile,
	/files\/etc\/uci-defaults\/99-cake-autorate-rs-scheduler-engine/,
	'the package must install the one-shot scheduler ownership decision');
assert.match(daemonMakefile, /define Package\/cake-autorate-rs-lite\n/,
	'the manual-only daemon must be shipped as a distinct package');
assert.match(daemonMakefile, /Package\/cake-autorate-rs[\s\S]*VARIANT:=full[\s\S]*DEFAULT_VARIANT:=1/,
	'the accepted Full daemon must remain the default build variant');
assert.match(daemonMakefile, /Package\/cake-autorate-rs-lite[\s\S]*VARIANT:=lite/,
	'the Lite daemon must have an independently selected build variant');
assert.match(daemonMakefile, /PKG_BUILD_DIR:=.*\$\(BUILD_VARIANT\)/,
	'Full and Lite Cargo builds must use isolated variant build directories');
assert.match(daemonMakefile,
	/Build\/Compile[\s\S]*filter lite,\$\(BUILD_VARIANT\)[\s\S]*--no-default-features/,
	'the Lite package must disable the default calibration feature at the Cargo boundary');
assert.match(daemonMakefile,
	/Package\/cake-autorate-rs\n[\s\S]*CONFLICTS:=cake-autorate-rs-lite[\s\S]*DEFAULT_VARIANT:=1/,
	'the default Full package must own the one-way Kconfig conflict used by OpenWrt package variants');
assert.match(daemonMakefile,
	/Package\/cake-autorate-rs\n[\s\S]*PROVIDES:=cake-autorate-rs-full[\s\S]*CONFLICTS:=cake-autorate-rs-lite/,
	'the full UI must bind to a capability that the calibration-free Lite daemon cannot satisfy');
assert.match(daemonMakefile, /Package\/cake-autorate-rs-lite[\s\S]*PROVIDES:=cake-autorate-rs/,
	'the non-default Lite package must provide the Full package identity so apk replaces rather than co-installs it');
assert.doesNotMatch(daemonMakefile,
	/Package\/cake-autorate-rs-lite[\s\S]*CONFLICTS:=cake-autorate-rs/,
	'a reciprocal Kconfig conflict creates a dependency cycle and must not return');
assert.match(daemonMakefile,
	/define Package\/cake-autorate-rs-lite\/install\n\s*\$\(call Package\/cake-autorate-rs\/install\/Default,\$\(1\)\)\nendef/,
	'the Lite package must install only the common manual-controller payload');
assert.doesNotMatch(daemonMakefile,
	/uci -q set cake-autorate\.globals\.autotune_scheduler_engine=/,
	'postinst must not duplicate or bypass the UCI-defaults ownership decision');
assert.match(schedulerSeeder, /if \[ "\$prior_configuration" -eq 0 \]; then/,
	'only a genuine fresh installation may select native ownership');
assert.match(schedulerSeeder,
	/"\$\{PKG_UPGRADE:-0\}" = 1.*"\$NATIVE_SCHEDULER_STORE"/,
	'package upgrades and retained native scheduler state must prevent silent owner promotion');
assert.match(schedulerSeeder, /grep -Eq '\^cake-autorate\\\.\[\^=\]\+=cake_autorate\$'/,
	'a retained autorate instance must prevent silent owner promotion after sysupgrade');
assert.match(schedulerSeeder,
	/if uci -q get "\$ENGINE_OPTION" >\/dev\/null 2>&1; then\s*exit 0/,
	'every existing explicit scheduler owner, including invalid values, must be preserved');
assert.doesNotMatch(schedulerSeeder, /CAKE_AUTORATE_DAEMON|\bsleep\b|\busleep\b/,
	'the scheduler seed must use a fixed daemon target and no delayed action');
assert.match(autotuneInit,
	/procd_set_param command "\$DAEMON" --calibrationd\s*\n\s*fi/,
	'an older daemon must remain passive instead of entering a respawn loop');
assert.match(autotuneInit, /procd_set_param term_timeout 5/,
	'calibrationd must receive SIGTERM and clean up its owned control socket');
assert.equal((autotuneInit.match(/--calibrationd/g) || []).length, 3,
	'the one coordinator must have native-scheduler, legacy-scheduler and compatibility command variants');
assert.equal((autotuneInit.match(/procd_open_instance coordinator/g) || []).length, 1,
	'the package must start exactly one global calibration coordinator');
assert.match(autotuneInit,
	/procd_set_param command \/usr\/libexec\/cake-autorate-rs\/autotune-scheduler run/,
	'explicit legacy scheduler ownership must retain the production shell scheduler');
assert.match(autotuneInit,
	/procd_open_instance recovery[\s\S]*procd_set_param command \/usr\/libexec\/cake-autorate-rs\/autotune-recover monitor/,
	'the recovery monitor must remain independent of scheduler ownership');

console.log('package post-install tests passed');
