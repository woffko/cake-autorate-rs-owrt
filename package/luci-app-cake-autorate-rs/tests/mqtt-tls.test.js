'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const source = fs.readFileSync(path.join(__dirname,
    '../htdocs/luci-static/resources/view/cake-autorate-rs/settings.js'), 'utf8');

function between(startMarker, endMarker) {
    const start = source.indexOf(startMarker);
    const end = source.indexOf(endMarker, start);
    assert(start >= 0 && end > start, 'missing marker ' + startMarker);
    return source.slice(start, end);
}

// LuCI adds String.prototype.format; a minimal %s version is enough here.
String.prototype.format = function(...values) {
    let index = 0;
    return this.replace(/%s/g, () => String(values[index++]));
};

// ---- the form declarations, executed against a recording stand-in for LuCI --------

function buildForm(saved) {
    const options = {};
    const record = (key, kind, title) => {
        const option = { key, kind, title, dependencies: [],
            depends(first, second) {
                this.dependencies.push(typeof first === 'object' ? first : { [first]: second });
            } };
        options[key] = option;
        return option;
    };
    const section = { taboption(_tab, type, key, title) { return record(key, type, title); } };
    const form = { DummyValue: 'DummyValue', Button: 'Button' };
    const uci = { get: (_config, id, key) => (saved[id] || {})[key] };
    const flag = (_s, _tab, key, title, initial) => Object.assign(record(key, 'Flag', title), { default: initial });
    const optionalValue = (_s, _tab, key, title, datatype, placeholder) =>
        Object.assign(record(key, 'Value', title), { datatype, placeholder });
    const E = (tag, attributes, children) => ({ tag, attributes, children });
    const cakeUi = { text: value => value };
    const helper = between('function mqttNotice(', 'function runMqttStatus(');
    const block = between("flag(section, 'logging', 'mqtt_enabled'", "o = section.taboption('logging', form.Button, '_mqtt_status'");
    new Function('section', 'form', 'uci', 'flag', 'optionalValue', 'E', 'cakeUi', 'modal', '_',
        helper + '\nvar o;\n' + block)(section, form, uci, flag, optionalValue, E, cakeUi, x => x, x => x);
    return options;
}

const options = buildForm({});

// New settings: TLS is on by default, with certificate verification.
assert.equal(options.mqtt_tls.default, '1');
assert.equal(options.mqtt_tls.kind, 'Flag');
assert.deepEqual(options.mqtt_tls.dependencies, [{ mqtt_enabled: '1' }]);
assert.equal(options.mqtt_tls_verify.default, '1');
assert.deepEqual(options.mqtt_tls_verify.dependencies, [{ mqtt_enabled: '1', mqtt_tls: '1' }]);
assert.deepEqual(options.mqtt_tls_ca_file.dependencies,
    [{ mqtt_enabled: '1', mqtt_tls: '1', mqtt_tls_verify: '1' }]);
assert.equal(options.mqtt_tls_ca_file.placeholder, 'built-in public CA list');
assert.match(options.mqtt_port.placeholder, /8883.*1883/);

// An instance saved before the option existed runs plain MQTT; the form shows it.
for (const [label, saved, expected] of [
    ['legacy publisher without mqtt_tls', { wan: { mqtt_enabled: '1', mqtt_host: 'broker' } }, '0'],
    ['new section', { wan: {} }, undefined],
    ['publisher that was never enabled', { wan: { mqtt_enabled: '0' } }, undefined],
    ['explicit TLS', { wan: { mqtt_enabled: '1', mqtt_tls: '1' } }, '1'],
    ['explicit plain', { wan: { mqtt_enabled: '1', mqtt_tls: '0' } }, '0'],
]) {
    assert.equal(buildForm(saved).mqtt_tls.cfgvalue('wan'), expected, label);
}

// The CA file must be an absolute path to a file.
const validate = options.mqtt_tls_ca_file.validate;
for (const good of ['', '/etc/ssl/mqtt-ca.pem', '/a'])
    assert.equal(validate('wan', good), true, good);
for (const bad of ['mqtt-ca.pem', './ca.pem', '/etc/ssl/', '/etc/ssl/ca\n.pem', '/' + 'a'.repeat(1024)])
    assert.equal(typeof validate('wan', bad), 'string', JSON.stringify(bad));

// Notices appear only where they apply.
const notice = key => options[key];
assert.deepEqual(notice('_mqtt_plain_notice').dependencies, [{ mqtt_enabled: '1', mqtt_tls: '0' }]);
assert.deepEqual(notice('_mqtt_unverified_notice').dependencies,
    [{ mqtt_enabled: '1', mqtt_tls: '1', mqtt_tls_verify: '0' }]);
assert.deepEqual(notice('_mqtt_port_notice').dependencies,
    [{ mqtt_enabled: '1', mqtt_tls: '1', mqtt_port: '1883' }]);
assert.equal(notice('_mqtt_plain_notice').cfgvalue().attributes.class, 'alert-message notice');
assert.match(notice('_mqtt_plain_notice').cfgvalue().children, /unencrypted[^]*trusted local network/);
assert.equal(notice('_mqtt_unverified_notice').cfgvalue().attributes.class, 'alert-message warning');
assert.match(notice('_mqtt_unverified_notice').cfgvalue().children, /not authenticated/);
assert.match(notice('_mqtt_port_notice').cfgvalue().children, /8883/);
for (const key of ['_mqtt_plain_notice', '_mqtt_unverified_notice', '_mqtt_port_notice']) {
    assert.equal(notice(key).write(), undefined);
    assert.equal(notice(key).remove(), undefined);
}

// ---- help texts ---------------------------------------------------------------------

const description = key => {
    const match = source.match(new RegExp('\\n\\t' + key + ": '([^']*)'"));
    assert(match, 'missing description of ' + key);
    return match[1];
};
assert.doesNotMatch(description('mqtt_host'), /plain MQTT/, 'the plain-MQTT notice moved to the TLS-off notice');
assert.match(description('mqtt_host'), /certificate must be valid for exactly this name or address/);
assert.match(description('mqtt_port'), /8883 with TLS and 1883 without/);
assert.match(description('mqtt_tls'), /NTP/);
assert.match(description('mqtt_tls'), /restarted/);
assert.match(description('mqtt_tls_ca_file'), /only the certificates in it are trusted/);
assert.match(description('mqtt_tls_ca_file'), /owned by root/);
assert.match(description('mqtt_tls_verify'), /pose as the broker/);
assert.match(description('mqtt_password'), /Without TLS[^]*unencrypted/);

// ---- the Check MQTT result ----------------------------------------------------------

const formatMqttStatus = new Function('_',
    between('function yesNo(', 'function defaultReflectors(') + '\nreturn formatMqttStatus;')(x => x);
const base = { section: 'wan', enabled: true, installed: true, configured_host: true, log_to_file: true,
    summary_enabled: true, cpu_enabled: false, publish_cpu: false, available: true, reason: 'available' };
const text = result => formatMqttStatus(Object.assign({}, base, result));
assert.doesNotMatch(text({}), /TLS/, 'a status from a daemon that does not report TLS shows no TLS lines');
assert.match(text({ tls: false }), /TLS: no/);
assert.doesNotMatch(text({ tls: false }), /Certificate verification/);
assert.match(text({ tls: true, tls_verify: true, tls_ca_file: false }),
    /TLS: yes\nCertificate verification: yes\nCA file configured: no/);
assert.match(text({ tls: true, tls_verify: true, tls_ca_file: true }), /CA file configured: yes/);
assert.match(text({ tls: true, tls_verify: false, tls_ca_file: true }),
    /Certificate verification: no/);
assert.doesNotMatch(text({ tls: true, tls_verify: false, tls_ca_file: true }), /CA file configured/);
assert.match(text({ available: false, reason: 'unable to open the MQTT CA file: missing' }),
    /Ready: no\nStatus: unable to open the MQTT CA file/);
// The handshake result is shown only for TLS, and only when it was tried.
assert.match(text({ tls: true, tls_verify: true, tls_ca_file: false, tls_handshake: 'ok' }),
    /CA file configured: no\nTLS handshake with the broker: ok/);
assert.match(text({ tls: true, tls_verify: true, tls_ca_file: true, tls_handshake: 'failed',
    available: false, reason: 'MQTT TLS handshake failed: invalid peer certificate: UnknownIssuer' }),
    /TLS handshake with the broker: failed\nLog to file: yes[^]*Ready: no\nStatus: MQTT TLS handshake failed: invalid peer certificate: UnknownIssuer$/);
assert.doesNotMatch(text({ tls: true, tls_verify: true, tls_ca_file: true, tls_handshake: 'not_checked' }),
    /handshake/);
assert.doesNotMatch(text({ tls: false, tls_handshake: 'ok' }), /handshake/);
assert.match(description('_mqtt_status'), /no user name, password or data is sent/);

console.log('MQTT TLS settings: defaults, upgrade display, dependencies, notices and status lines passed');
