'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const makefile = fs.readFileSync(path.join(__dirname, '..', 'Makefile'), 'utf8');
const daemonMakefile = fs.readFileSync(path.join(__dirname, '..', '..', 'cake-autorate-rs',
	'Makefile'), 'utf8');
const daemonMain = fs.readFileSync(path.join(__dirname, '..', '..', 'cake-autorate-rs', 'src',
	'src', 'main.rs'), 'utf8');
const daemonCoordinator = fs.readFileSync(path.join(__dirname, '..', '..', 'cake-autorate-rs',
	'src', 'src', 'operations', 'coordinator.rs'), 'utf8');
const daemonCargo = fs.readFileSync(path.join(__dirname, '..', '..', 'cake-autorate-rs', 'src',
	'Cargo.toml'), 'utf8');
const daemonAutotune = fs.readFileSync(path.join(__dirname, '..', '..', 'cake-autorate-rs', 'src',
	'src', 'autotune.rs'), 'utf8');
const settingsView = fs.readFileSync(path.join(__dirname, '..', 'htdocs', 'luci-static',
	'resources', 'view', 'cake-autorate-rs', 'settings.js'), 'utf8');
const initRenderer = fs.readFileSync(path.join(__dirname, '..', '..', 'cake-autorate-rs',
	'scripts', 'render-init-variant.sh'), 'utf8');
const daemonInitTemplate = fs.readFileSync(path.join(__dirname, '..', '..', 'cake-autorate-rs',
	'files', 'etc', 'init.d', 'cake-autorate'), 'utf8');
const autotuneInit = fs.readFileSync(path.join(__dirname, '..', '..', 'cake-autorate-rs',
	'files', 'etc', 'init.d', 'cake-autorate-autotune'), 'utf8');
const retiredQualityTest = path.join(__dirname, '..', 'root', 'usr', 'libexec',
	'cake-autorate-rs', 'quality-test');
const retiredScheduler = path.join(__dirname, '..', 'root', 'usr', 'libexec',
	'cake-autorate-rs', 'autotune-scheduler');
const retiredAutotune = path.join(__dirname, '..', 'root', 'usr', 'libexec',
	'cake-autorate-rs', 'autotune');
const retiredAutotuneRecovery = path.join(__dirname, '..', 'root', 'usr', 'libexec',
	'cake-autorate-rs', 'autotune-recover');
const retiredApplyGuard = path.join(__dirname, '..', 'root', 'usr', 'libexec',
	'cake-autorate-rs', 'apply-guard');
const retiredApplyGuardInit = path.join(__dirname, '..', 'root', 'etc', 'init.d',
	'cake-autorate-apply-guard');
const retiredRpcdHelper = path.join(__dirname, '..', 'root', 'usr', 'libexec',
	'cake-autorate-rs', 'rpcd-helper');
const retiredSpeedtest = path.join(__dirname, '..', 'root', 'usr', 'libexec',
	'cake-autorate-rs', 'speedtest');
const retiredProcdJob = path.join(__dirname, '..', 'root', 'usr', 'libexec',
	'cake-autorate-rs', 'procd-job');
const retiredRuntimeHealth = path.join(__dirname, '..', 'root', 'usr', 'libexec',
	'cake-autorate-rs', 'runtime-health');
const retiredTrafficClassifier = path.join(__dirname, '..', '..', 'cake-autorate-rs',
	'files', 'usr', 'libexec', 'cake-autorate-rs', 'traffic-classifier');
const retiredPingerPlan = path.join(__dirname, '..', 'root', 'usr', 'libexec',
	'cake-autorate-rs', 'pinger-plan');
const retiredSqmRecover = path.join(__dirname, '..', '..', 'cake-autorate-rs',
	'files', 'usr', 'libexec', 'cake-autorate-rs', 'sqm-recover');
const retiredPackageVersions = path.join(__dirname, '..', 'root', 'usr', 'libexec',
	'cake-autorate-rs', 'package-versions');
const retiredMwan3Info = path.join(__dirname, '..', 'root', 'usr', 'libexec',
	'cake-autorate-rs', 'mwan3-info');
const retiredGraphHistory = path.join(__dirname, '..', 'root', 'usr', 'libexec',
	'cake-autorate-rs', 'graph-history');
const retiredStatusColumns = path.join(__dirname, '..', 'root', 'usr', 'libexec',
	'cake-autorate-rs', 'status-columns');
const retiredMqttStatus = path.join(__dirname, '..', 'root', 'usr', 'libexec',
	'cake-autorate-rs', 'mqtt-status');
const retiredMqttPublisher = path.join(__dirname, '..', '..', 'cake-autorate-rs',
	'files', 'usr', 'libexec', 'cake-autorate-rs', 'mqtt-publisher');
const retiredMqttInit = path.join(__dirname, '..', '..', 'cake-autorate-rs',
	'files', 'etc', 'init.d', 'cake-autorate-mqtt');
const retiredLogBundle = path.join(__dirname, '..', 'root', 'usr', 'libexec',
	'cake-autorate-rs', 'log-bundle');

function makeDefine(source, name) {
	const escaped = name.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
	const match = source.match(new RegExp(`^define ${escaped}\\n([\\s\\S]*?)^endef$`, 'm'));
	assert(match, `missing Make definition: ${name}`);
	return match[1];
}

const luciPostinst = makeDefine(makefile, 'Package/luci-app-cake-autorate-rs/postinst');
const daemonDefaults = makeDefine(daemonMakefile, 'Package/cake-autorate-rs/Default');
const daemonFull = makeDefine(daemonMakefile, 'Package/cake-autorate-rs');
const daemonLite = makeDefine(daemonMakefile, 'Package/cake-autorate-rs-lite');
const daemonDefaultInstall = makeDefine(daemonMakefile,
	'Package/cake-autorate-rs/install/Default');
const daemonFullInstall = makeDefine(daemonMakefile, 'Package/cake-autorate-rs/install');
const daemonLiteInstall = makeDefine(daemonMakefile, 'Package/cake-autorate-rs-lite/install');

const daemonCapability = daemonMain.match(
	/const CALIBRATION_CAPABILITIES_V3: &str =\s*"([^"]+)";/,
);
assert(daemonCapability,
	'the public calibration capability protocol must remain declared by Rust');

const rustProfileSearchSchema = daemonAutotune.match(
	/pub const PROFILE_SEARCH_SCHEMA_VERSION: u32 = (\d+);/);
const luciProfileSearchSchema = settingsView.match(
	/var AUTOTUNE_PROFILE_SEARCH_SCHEMA_VERSION = (\d+);/);
assert(rustProfileSearchSchema && luciProfileSearchSchema,
	'profile-search schema declarations are incomplete');
for (const consumer of [ luciProfileSearchSchema ]) {
	assert.equal(consumer[1], rustProfileSearchSchema[1],
		'Rust, Apply Guard and LuCI must share one profile-search schema');
}
assert.match(daemonAutotune,
	/PROFILE_SEARCH_SCHEMA_VERSION,\n\s*self\.profile\.as_str\(\),/,
	'the Rust profile-search serializer must emit its declared schema constant');

assert.match(makefile, /rm -f \/tmp\/luci-indexcache \/tmp\/luci-indexcache\.\*/);
assert.match(makefile, /rm -rf \/tmp\/luci-modulecache \/tmp\/luci-modulecache\.\*/);
assert.match(makefile, /\/etc\/init\.d\/rpcd reload >\/dev\/null 2>&1 \|\| true/);
assert.doesNotMatch(luciPostinst, /cake-autorate-autotune/,
	'the UI package must not own the daemon calibration lifecycle');
assert.match(makefile, /DEPENDS:=.*\+cake-autorate-rs-full/,
	'the Full-only daemon capability must own calibration independently of LuCI');
assert.doesNotMatch(makefile, /define Package\/luci-app-cake-autorate-rs\/prerm/,
	'the UI package must not retain a daemon removal lifecycle');
assert.doesNotMatch(makefile, /rm -f \/tmp\/luci-indexcache\.\*;/,
	'the unsuffixed LuCI index cache must not survive package replacement');
assert.doesNotMatch(makefile,
	/quality-test|apply-guard|rpcd-helper|procd-job|runtime-health/,
	'the final package recipe must not retain cleanup rules for removed helper payloads');
assert.equal(fs.existsSync(retiredQualityTest), false,
	'the retired shell Rating worker body must not remain in the source payload');
assert.equal(fs.existsSync(retiredScheduler), false,
	'the retired shell scheduler body must not remain in the source payload');
assert.equal(fs.existsSync(retiredAutotune), false,
	'the retired shell Auto-Tune worker body must not remain in the source payload');
assert.equal(fs.existsSync(retiredAutotuneRecovery), false,
	'the retired shell Auto-Tune recovery body must not remain in the source payload');
assert.equal(fs.existsSync(retiredApplyGuard), false,
	'the retired shell Apply Guard body must not remain in the source payload');
assert.equal(fs.existsSync(retiredApplyGuardInit), false,
	'the retired shell Apply Guard supervisor must not remain in the source payload');
assert.equal(fs.existsSync(retiredRpcdHelper), false,
	'the retired one-operation rpcd shell dispatcher must not remain in the source payload');
assert.equal(fs.existsSync(retiredSpeedtest), false,
	'the retired shell Speed Test worker must not remain in the source payload');
assert.equal(fs.existsSync(retiredProcdJob), false,
	'the retired shell worker launcher must not remain in the source payload');
assert.equal(fs.existsSync(retiredRuntimeHealth), false,
	'the retired shell runtime-health reader must not remain in the source payload');
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
for (const retiredVerb of [ '--legacy-apply-recover', '--legacy-autotune-recover',
	'--scheduler-adopt-legacy', '--scheduler-owner-seed' ])
	assert.doesNotMatch(daemonMain, new RegExp(retiredVerb),
		`the final daemon must reject retired compatibility verb ${retiredVerb}`);
assert.match(daemonMain, /Some\("--calibration-service"\)/,
	'the daemon must expose the single native calibration lifecycle endpoint');
const coordinatorOwnerLock = daemonCoordinator.indexOf(
	'.then(|| SchedulerOwnerLock::open(Path::new(PRODUCTION_SCHEDULER_OWNER_LOCK)))');
const coordinatorApplyRecovery = daemonCoordinator.indexOf(
	'let _ = native_autotune_apply_recovery()?;');
const coordinatorBind = daemonCoordinator.indexOf(
	'let mut daemon = CalibrationDaemon::bind(&options.state_dir)?;');
assert(coordinatorOwnerLock >= 0 && coordinatorOwnerLock < coordinatorApplyRecovery &&
	coordinatorApplyRecovery < coordinatorBind,
	'every coordinator launch and procd respawn must lock ownership and settle native Apply before admission');
const lifecyclePrepare = autotuneInit.indexOf('--calibration-service prepare-start');
const lifecycleStopReady = autotuneInit.indexOf('--calibration-service prepare-stop');
const lifecycleStarted = autotuneInit.indexOf('--calibration-service confirm-started');
const lifecycleStopped = autotuneInit.indexOf('--calibration-service confirm-stopped');
const coordinatorStart = autotuneInit.indexOf('procd_open_instance coordinator');

assert(lifecyclePrepare >= 0 && lifecyclePrepare < coordinatorStart,
	'the single Rust lifecycle transaction must complete before coordinator registration');
assert(lifecycleStopped >= 0,
	'the calibration service must attest exact process exit after procd stop');
assert(lifecycleStopReady >= 0 && lifecycleStopReady < lifecycleStopped,
	'native Apply state must authorize stop before the post-procd exit attestor');
assert(lifecycleStarted > coordinatorStart,
	'the calibration service must attest coordinator readiness after procd registration');
assert.match(autotuneInit, /service_started\(\)/,
	'the readiness attestation must run from the post-procd lifecycle hook');
assert.match(autotuneInit, /service_stopped\(\)/,
	'the stop attestation must run from the post-procd lifecycle hook');
assert.doesNotMatch(autotuneInit, /sleep|usleep/,
	'the calibration stop boundary must remain process-event driven');
assert.match(daemonInitTemplate, /--service-lifecycle confirm-started/,
	'the controller readiness hook must use the typed Rust lifecycle endpoint');
assert.match(daemonInitTemplate, /SERVICE_START_CONFIRM_PENDING/,
	'reload must defer controller readiness until its runtime lock is released');
assert.match(daemonInitTemplate, /service-start-deferred-v1/,
	'package replacement must use a distinct typed deferral instead of a genuine empty plan');
assert.doesNotMatch(daemonInitTemplate,
	/\[ "\$controller_instances" != - \] \|\| SERVICE_START_CONFIRM_DEFERRED=1/,
	'a genuine empty controller plan must still attest that no stale controller survives');
assert.doesNotMatch(daemonInitTemplate, /sleep|usleep/,
	'the controller readiness boundary must remain event driven');
assert.match(daemonCoordinator,
	/confirm_native_apply_controllers_after_unlock\(\)/,
	'native Apply must prove controller readiness only after its transaction returns');
for (const retiredDecision of [ '--native-apply-recover', '--legacy-apply-recover',
	'--legacy-autotune-recover', '--scheduler-adopt-legacy', '--calibration-capabilities',
	'config_load', 'config_get', 'autotune_scheduler_engine' ])
	assert.doesNotMatch(autotuneInit, new RegExp(retiredDecision),
		`calibration init must not retain ${retiredDecision} policy`);
assert.match(autotuneInit,
	/expected="\$\(printf 'cake-autorate-calibration-service\\t1\\tcoordinator'\)"/,
	'the init bridge must validate the exact bounded Rust start protocol');
assert.match(autotuneInit,
	/deferred="\$\(printf 'cake-autorate-calibration-service\\t1\\tdeferred'\)"/,
	'default_postinst must receive an exact Rust package-upgrade deferral plan');
assert.match(autotuneInit,
	/expected="\$\(printf 'cake-autorate-calibration-service\\t1\\tready'\)"/,
	'the init bridge must validate the exact bounded Rust readiness protocol');
assert.match(autotuneInit,
	/expected="\$\(printf 'cake-autorate-calibration-service\\t1\\tstop-ready'\)"/,
	'the init bridge must validate exact state-driven native Apply stop readiness');
assert.match(autotuneInit,
	/expected="\$\(printf 'cake-autorate-calibration-service\\t1\\tstopped'\)"/,
	'the init bridge must validate the exact bounded Rust stop protocol');
assert.match(autotuneInit,
	/NATIVE_SCHEDULER_STORE="\/etc\/cake-autorate-rs-scheduler"/,
	'the scheduler ledger must use the exact bounded persistent OpenWrt path');
assert.doesNotMatch(autotuneInit, /--help 2>&1 \| grep/,
	'usage prose must never authorize production scheduler ownership');
assert.doesNotMatch(daemonMakefile,
	/files\/etc\/uci-defaults\/99-cake-autorate-rs-scheduler-engine/,
	'the final package must not install the retired scheduler ownership seed');
const daemonUpgradeNativeApply = daemonMakefile.indexOf('--native-apply-recover');
const daemonUpgradeRestart = daemonMakefile.indexOf(
	'PKG_UPGRADE=0 /etc/init.d/cake-autorate restart');
const daemonUpgradeCalibrationStop = daemonMakefile.indexOf(
	'/etc/init.d/cake-autorate-autotune stop >/dev/null 2>&1 || exit 1');
const daemonCalibrationStart = daemonMakefile.indexOf(
	'/etc/init.d/cake-autorate-autotune start >/dev/null 2>&1 || exit 1');
assert(daemonUpgradeCalibrationStop >= 0 &&
	daemonUpgradeCalibrationStop < daemonUpgradeNativeApply &&
	daemonUpgradeNativeApply < daemonUpgradeRestart &&
	daemonUpgradeRestart < daemonCalibrationStart,
	'an in-place Full daemon upgrade must stop calibration before native recovery and the main restart');
assert.match(daemonInitTemplate, /service_started\(\)/,
	'the main restart must own its event-driven controller readiness gate');
assert.match(daemonMakefile,
	/\$\(INSTALL_BIN\) \.\/files\/etc\/init\.d\/cake-autorate-autotune \$\(1\)\/etc\/init\.d\/cake-autorate-autotune/,
	'the Full daemon package must ship its exact-version calibration init bridge');
assert.match(daemonMakefile,
	/PKG_UPGRADE=0 \/etc\/init\.d\/cake-autorate-autotune start >\/dev\/null 2>&1 \|\| exit 1;/,
	'the Full daemon package must start its own newly installed bridge and propagate failure');
assert.match(daemonMakefile,
	/define Package\/cake-autorate-rs\/prerm[\s\S]*cake-autorate-autotune stop[\s\S]*cake-autorate-autotune disable/,
	'removing the Full daemon must settle and disable its calibration service');
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
assert.match(daemonCargo, /default = \["calibration"\]/,
	'Full must remain the default Cargo feature set');
assert.match(daemonCargo, /calibration = \["dep:ring", "transport-probes"\]/,
	'Full calibration must retain transport/TLS capability explicitly');
for (const dependency of [ 'ring', 'rustls', 'socket2', 'tungstenite', 'webpki-roots' ]) {
	assert.match(daemonCargo,
		new RegExp(`^${dependency} = \\{[^\\n]*optional = true[^\\n]*\\}$`, 'm'),
		`${dependency} must be physically absent from --no-default-features Lite builds`);
}
assert.match(daemonMain,
	/#\[cfg\(feature = "transport-probes"\)\]\nmod quality_grade;/,
	'Lite must not compile the Full quality-grade state machine');
assert.match(daemonMain,
	/#\[cfg\(feature = "transport-probes"\)\]\nmod rating_load;/,
	'Lite must not compile Rating load/capture tracking');
assert.match(daemonMain,
	/#\[cfg\(feature = "transport-probes"\)\]\nmod transport_probe;/,
	'Lite must not compile network transport probes');
assert.doesNotMatch(daemonDefaults, /nftables-json/,
	'Lite must not inherit the Full-only nftables dependency through common package defaults');
assert.match(daemonFull, /DEPENDS\+=\+nftables-json/,
	'Full must retain nftables-json for calibrated traffic attribution');
assert.match(daemonFull, /DEPENDS\+=.*\+speedtest-go/,
	'Full must install the one native Speed Test backend used by calibration');
assert.doesNotMatch(daemonLite, /nftables-json/,
	'the manual-only Lite package must not regain nftables-json directly');
assert.doesNotMatch(daemonLite, /speedtest-go/,
	'the manual-only Lite package must not install a calibration traffic generator');
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
assert.match(daemonDefaultInstall,
	/\$\(SHELL\) \.\/scripts\/render-init-variant\.sh \$\(BUILD_VARIANT\)/,
	'every daemon variant must render its init script from the audited variant template');
assert.doesNotMatch(daemonDefaultInstall,
	/cake-autorate-mqtt|mqtt-publisher|mqtt-status|traffic-classifier/,
	'the common Lite payload must not contain Full-only services or helpers');
assert.doesNotMatch(daemonDefaultInstall, /runtime-lock|sqm-recover/,
	'the common Lite payload must not ship Full lifecycle locking or retired SQM recovery shell');
assert.doesNotMatch(daemonFullInstall, /cake-autorate-mqtt/,
	'Full must not ship a second MQTT init lifecycle');
assert.equal(fs.existsSync(retiredMqttInit), false,
	'the retired separate MQTT init service must be absent from the source payload');
assert.doesNotMatch(daemonFullInstall, /usr\/libexec\/cake-autorate-rs\/mqtt-publisher/,
	'Full must not ship the retired MQTT shell publisher');
assert.equal(fs.existsSync(retiredMqttPublisher), false,
	'the retired MQTT shell publisher body must be absent from the source payload');
assert.doesNotMatch(daemonFullInstall, /mqtt-status/,
	'Full must not ship a separate shell MQTT status authority');
assert.match(daemonMain, /Some\("--mqtt-status"\)/,
	'Full must retain bounded native MQTT readiness status');
assert.doesNotMatch(daemonMain, /Some\("--mqtt-service-plan"\)/,
	'the retired separate MQTT service-plan CLI must stay absent');
assert.match(daemonMain, /Some\("--mqtt-publisher"\)/,
	'Full must run the MQTT publisher in Rust');
assert.match(daemonInitTemplate,
	/procd_set_param command "\$DAEMON" --mqtt-publisher "\$1"/,
	'Full must register MQTT under the unified main procd service');
assert.doesNotMatch(daemonInitTemplate, /\/etc\/init\.d\/cake-autorate-mqtt/,
	'the main service must not delegate MQTT lifecycle to a retired second init');
assert.match(daemonFullInstall, /runtime-lock/,
	'Full must retain the minimal rc.common global-lock bridge');
assert.equal(fs.existsSync(retiredSqmRecover), false,
	'the retired shell SQM recovery executable must be absent from the source payload');
for (const retired of [ retiredPackageVersions, retiredMwan3Info, retiredGraphHistory,
	retiredStatusColumns, retiredMqttStatus, retiredLogBundle ])
	assert.equal(fs.existsSync(retired), false,
		'the retired LuCI readout helper must be absent from the source payload');
assert.doesNotMatch(makefile,
	/package-versions|mwan3-info|graph-history|status-columns|mqtt-status|log-bundle/,
	'the final recipe must not retain removed readout-helper cleanup entries');
for (const verb of [ '--package-versions', '--mwan3-info', '--graph-history' ])
	assert.match(daemonMain, new RegExp(`Some\\("${verb}"\\)`),
		`the daemon must retain the native ${verb} readout`);
assert.doesNotMatch(daemonFullInstall, /traffic-classifier/,
	'Full must not ship a separate shell traffic-classifier authority');
assert.match(daemonMain, /Some\("--traffic-classifier"\)/,
	'Full must retain traffic-priority classification through the native daemon');
assert.equal(fs.existsSync(retiredTrafficClassifier), false,
	'the retired shell traffic-classifier executable must be absent from the source payload');
assert.match(daemonMain, /Some\("--pinger-plan"\)/,
	'Full must retain pinger planning through the native daemon');
assert.equal(fs.existsSync(retiredPingerPlan), false,
	'the retired shell pinger-plan executable must be absent from the LuCI payload');
assert.match(settingsView, /'--pinger-plan'/,
	'the Settings wizard must call the native pinger planner');
assert.doesNotMatch(settingsView, /\/usr\/libexec\/cake-autorate-rs\/pinger-plan/,
	'the Settings wizard must not retain the retired shell pinger-plan path');
assert.match(settingsView, /fs\.exec\('\/usr\/sbin\/cake-autorated', \[\s*'--mqtt-status'/,
	'the Settings page must call the bounded native MQTT endpoint');
assert.doesNotMatch(settingsView, /\/usr\/libexec\/cake-autorate-rs\/mqtt-status/,
	'the Settings page must not retain the retired shell MQTT endpoint');
assert.match(daemonMain, /Some\("--log-bundle"\)/,
	'Full must retain bounded diagnostic export through the native daemon');
assert.doesNotMatch(daemonLiteInstall,
	/cake-autorate-mqtt|mqtt-publisher|mqtt-status|runtime-lock|traffic-classifier/,
	'Lite install must remain a strict call to the common manual-controller payload');
assert.match(initRenderer, /case "\$variant" in\n\s*full\|lite\)/,
	'the renderer must accept only the two audited package variants');
assert.match(initRenderer, /nested init variant marker/,
	'the renderer must fail closed on nested variant sections');
assert.match(initRenderer, /mismatched init variant marker/,
	'the renderer must fail closed on mismatched variant sections');
assert.match(initRenderer, /unterminated init variant marker/,
	'the renderer must fail closed on unterminated variant sections');
assert.doesNotMatch(daemonMakefile, /autotune_scheduler_engine|scheduler-owner-seed/,
	'the final package must not retain scheduler owner migration policy');
assert.match(autotuneInit, /procd_set_param term_timeout 5/,
	'calibrationd must receive SIGTERM and clean up its owned control socket');
assert.equal((autotuneInit.match(/--calibrationd/g) || []).length, 1,
	'the service must expose exactly one Rust coordinator command');
assert.equal((autotuneInit.match(/procd_open_instance coordinator/g) || []).length, 1,
	'the package must start exactly one global calibration coordinator');
assert.doesNotMatch(autotuneInit, /autotune-scheduler|procd_open_instance scheduler/,
	'the init topology must not retain the retired shell scheduler');
assert.doesNotMatch(autotuneInit,
	/\/usr\/libexec\/cake-autorate-rs\/autotune-recover|procd_open_instance recovery/,
	'the retired shell recovery monitor must not remain in the init topology');
assert.equal((autotuneInit.match(/--calibration-service prepare-start/g) || []).length, 1,
	'all recovery and adoption policy must enter Rust through one synchronous preflight');
assert.equal((autotuneInit.match(/--calibration-service prepare-stop/g) || []).length, 1,
	'calibration stop must have exactly one state-driven native Apply preflight');
assert.equal((autotuneInit.match(/--calibration-service confirm-started/g) || []).length, 1,
	'coordinator readiness must have exactly one native post-procd attestor');

console.log('package post-install tests passed');
