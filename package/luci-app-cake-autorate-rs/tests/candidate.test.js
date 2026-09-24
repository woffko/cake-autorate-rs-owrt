'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');

const resource = name => path.join(__dirname, '../htdocs/luci-static/resources/cake-autorate-rs/', name);
const clientSource = fs.readFileSync(resource('candidate.js'), 'utf8');
const hashSource = fs.readFileSync(resource('sha256.js'), 'utf8');
const L = { Class: { extend(methods) { function Class() {} Object.assign(Class.prototype, methods); return Class; } } };
const HashClass = new Function('L', hashSource)(L);
assert.equal(typeof HashClass, 'function', 'LuCI requires a class constructor, not a plain module object');
const hash = new HashClass();
for (const name of ['candidate.js', 'sha256.js', 'sha256.LICENSE']) {
    assert.equal(fs.readFileSync(resource(name), 'utf8'), fs.readFileSync(path.join(__dirname,
        '../../luci-app-cake-autorate-rs-lite/htdocs/luci-static/resources/cake-autorate-rs/', name), 'utf8'),
    name + ' must be the identical shared implementation in Full and Lite');
}
for (const size of [0, 1, 55, 56, 63, 64, 65, 1024, 262144]) {
    const bytes = new Uint8Array(size).fill(0xa5);
    assert.equal(hash.digest(bytes), crypto.createHash('sha256').update(bytes).digest('hex'));
}
assert.equal(hash.digest(new TextEncoder().encode('тест 🌍')), crypto.createHash('sha256').update('тест 🌍').digest('hex'));

const nativeSource = fs.readFileSync(path.join(__dirname, '../../cake-autorate-rs/src/src/main.rs'), 'utf8');
const parser = nativeSource.slice(nativeSource.indexOf('    fn from_uci_values('), nativeSource.indexOf('    fn load_reflectors_url('));
const fieldsSource = fs.readFileSync(path.join(__dirname, '../../cake-autorate-rs/src/src/config_fields.rs'), 'utf8');
const fields = [...fieldsSource.matchAll(/^\s*"([^"]+)",$/gm)].map(match => match[1]);
for (const match of parser.matchAll(/set_(?:bool|string|f64|u64|usize)\(\s*&?single,\s*"([^"]+)"/g))
    assert(fields.includes(match[1]), 'native candidate inventory omitted ' + match[1]);

function fixture(options = {}) {
    const events = [], notices = [];
    let record = null, submitted = null, writes = 0;
    let pending = options.pending || { network: [['set', 'wan', 'proto', 'unchanged']] };
    const data = {
        state: { values: { 'cake-autorate': {
            wan: { '.name': 'wan', '.type': 'cake_autorate', base_dl_shaper_rate_kbps: '30000',
                min_dl_shaper_rate_kbps: '10000', mqtt_password: 'private-browser-only' },
            globals: { '.name': 'globals', '.type': 'globals', graph_history_ram_budget_kib: 'auto' }
        }, sqm: {}, network: { wan: { '.name': 'wan', proto: 'dhcp' } } },
        creates: {}, changes: {}, deletes: {}, reorder: {} },
        get(config, name, option) {
            const value = Object.assign({}, this.state.values[config]?.[name], this.state.creates[config]?.[name], this.state.changes[config]?.[name]);
            return option ? value[option] : value;
        },
        sections(config, type) {
            return [...new Set(Object.keys(this.state.values[config] || {}).concat(Object.keys(this.state.creates[config] || {})))]
                .map(name => this.get(config, name)).filter(section => section['.type'] === type);
        },
        set(config, section, key, value) {
            this.state.changes[config] ||= {};
            this.state.changes[config][section] ||= {};
            this.state.changes[config][section][key] = value;
        },
        save() {
            events.push('uci.save');
            writes++;
            if (options.saveFailure) return Promise.reject(new Error('private-rpc-save-value'));
            return Promise.resolve();
        }
    };
    const rpc = { declare(spec) {
        assert.equal(spec.reject, true);
        return async (...args) => {
            const request = Object.fromEntries(spec.params.map((name, index) => [name, args[index]]));
            assert(Buffer.byteLength(JSON.stringify(request)) < 3000, 'RPC chunks must remain below the small pipe limit');
            events.push(spec.method);
            if (spec.object === 'uci') return { changes: pending };
            assert.equal(spec.object, 'cake-autorate-config');
            const reply = result => ({ schema_version: 1, ok: true, request_id: request.request_id, result });
            if (spec.method === 'schema') {
                if (options.schemaFailure) throw new Error('private-rpc-schema-value');
                return { schema_version: 1, ok: true, result: { protocol: 'chunked-candidate-v1',
                    validation_scope: 'controller-sqm', max_input_bytes: 262144, chunk_bytes: 1024,
                    fields: options.emptySchema ? [] : fields, global_fields: ['graph_history_ram_budget_kib'],
                    rate_rules: { min: 1, max: options.rateMax || 100000000, disabled_zero_tuple: true,
                        defaults: { dl: [5000, 20000, 80000], ul: [5000, 20000, 35000] } } } };
            }
            if (spec.method === 'begin') {
                record = { id: request.request_id, digest: request.sha256, length: request.length,
                    bytes: Buffer.alloc(0), token: '0123456789abcdef0123456789abcdef' };
                if (options.lostBegin) throw new Error('private-rpc-begin-value');
                return reply({ token: record.token, chunk_bytes: 1024 });
            }
            if (spec.method === 'cancel') {
                if (record) {
                    assert.equal(request.request_id, record.id);
                    assert.equal(request.sha256, record.digest);
                }
                record = null;
                return reply({ cancelled: true });
            }
            assert(record);
            assert.equal(request.request_id, record.id);
            assert.equal(request.token, record.token);
            if (spec.method === 'append') {
                if (options.appendFailure) throw new Error('Native candidate private-rpc-append-value');
                assert.equal(request.offset, record.bytes.length);
                record.bytes = Buffer.concat([record.bytes, Buffer.from(request.data, 'hex')]);
                if (options.onAppend) options.onAppend(data);
                return reply({ next_offset: options.wrongOffset ? 0 : record.bytes.length });
            }
            assert.equal(spec.method, 'finish');
            assert.equal(record.bytes.length, record.length);
            assert.equal(crypto.createHash('sha256').update(record.bytes).digest('hex'), record.digest);
            submitted = JSON.parse(record.bytes);
            assert.equal(submitted.sections[0].options.mqtt_password, undefined, 'unconsumed credentials must not be transferred');
            const values = submitted.sections[0].options;
            const valid = Number(values.min_dl_shaper_rate_kbps) <= Number(values.base_dl_shaper_rate_kbps);
            const validation = { schema_version: 1, validation_scope: 'controller-sqm',
                request_id: record.id, candidate_sha256: options.wrongDigest ? '0'.repeat(64) : record.digest,
                valid, errors: valid ? [] : [{ instance: 'wan', message: 'min <= base <= max required' }] };
            record = null;
            if (options.onFinish) options.onFinish(data);
            return reply({ validation });
        };
    } };
    const ui = { addNotification: (_title, node, kind) => notices.push({ text: node.textContent, kind }) };
    const ClientClass = new Function('rpc', 'uci', 'ui', 'cakeHash', '_', 'globalThis', 'document', 'L', clientSource)(
        rpc, data, ui, hash, value => value, { crypto: { getRandomValues: bytes => crypto.randomFillSync(bytes) } },
        { createElement: tag => ({ tag, textContent: '' }) }, L);
    assert.equal(typeof ClientClass, 'function');
    const client = new ClientClass();
    const map = {
        data, input: '10000',
        save(callback) {
            // Same documented Map.save sequence as LuCI; use late lookup too,
            // so the check cannot rely on only the old eager .bind() behavior.
            return Promise.resolve().then(() => {
                events.push('parse');
                this.data.set('cake-autorate', 'wan', 'min_dl_shaper_rate_kbps', this.input);
            }).then(callback).then(() => {
                if (options.beforeSave) options.beforeSave(data);
                return this.data.save();
            });
        }
    };
    client.attach(map);
    return { data, map, client, events, notices, get record() { return record; },
        get submitted() { return submitted; }, get writes() { return writes; },
        setPending: value => { pending = value; } };
}

(async () => {
    const rates = fixture();
    await rates.client.loadSchema();
    for (const [values, dl, ul] of [
        [{}, false, false], [{enabled: '1'}, true, true],
        [{enabled: '1', sqm_enabled: '0'}, false, false],
        [{enabled: '0', sqm_enabled: '1'}, true, true],
        [{sqm_enabled: '1', sqm_direction_mode: 'upload_only'}, false, true],
        [{sqm_enabled: '1', adjust_dl_shaper_rate: '0'}, false, true]
    ]) {
        assert.equal(rates.client.directionEnabled('dl', key => values[key]), dl);
        assert.equal(rates.client.directionEnabled('ul', key => values[key]), ul);
    }
    for (const [values, enabled, valid] of [
        [['1', '50', '99'], true, true], [['1.5', '50.5', '99.5'], true, true],
        [['0', '0', '0'], false, true], [['0', '0', '0'], true, false],
        [['0', '10', '20'], false, false], [['3', '2', '4'], true, false],
        [['1', '2', '100000001'], true, false], [['10000junk', '20000', '30000'], true, false],
        [['NaN', '20000', '30000'], true, false], [['1', '2', '1e400'], true, false],
        [[null, null, null], true, true]
    ]) assert.equal(rates.client.validateRateTuple(values, 'dl', enabled) === true, valid, JSON.stringify(values));
    const changed = fixture({ rateMax: 123456 });
    await changed.client.loadSchema();
    assert.notEqual(changed.client.validateRateTuple(['1', '2', '123457'], 'dl', true), true,
        'inline bounds must come from the native schema, not a copied maximum');
    assert.equal(changed.writes, 0, 'schema loading must not stage or save UCI');
    const good = fixture();
    good.map.input = '20000';
    await good.map.save();
    assert.equal(good.writes, 1);
    assert.equal(good.submitted.sections[0].options.min_dl_shaper_rate_kbps, '20000');
    assert(good.events.indexOf('finish') < good.events.indexOf('uci.save'));
    assert.equal(good.map.data, good.data, 'the temporary proxy must be released');

    const invalid = fixture();
    const before = JSON.stringify(invalid.data.state);
    invalid.map.input = '40000';
    await assert.rejects(() => invalid.map.save(null, true), /min <= base/);
    assert.equal(invalid.writes, 0);
    assert.equal(JSON.stringify(invalid.data.state), before, 'native rejection must restore exact own cache and preserve foreign values');
    assert.equal(invalid.map.input, '40000', 'typed input must remain available for correction');
    assert.equal(invalid.notices.length, 1, 'silent modal saves need a visible text-only native error');
    assert.equal(invalid.record, null);

    for (const options of [{ appendFailure: true }, { lostBegin: true }, { wrongOffset: true }, { wrongDigest: true }]) {
        const f = fixture(options);
        await assert.rejects(() => f.map.save(), cause => !cause.message.includes('private-rpc'));
        assert.equal(f.writes, 0);
        assert(f.events.includes('cancel'));
        assert.equal(f.record, null, 'every failure with a started transfer must attempt cleanup');
        assert.equal(f.events.filter(name => name === 'begin').length, 1, 'lost begin ACK must not start a duplicate');
    }
    const drift = fixture({ beforeSave: data => data.set('cake-autorate', 'wan', 'min_dl_shaper_rate_kbps', '99999') });
    await assert.rejects(() => drift.map.save(), /changed while/);
    assert.equal(drift.writes, 0, 'the actual data.save boundary must reject a post-validation change');
    assert.equal(drift.data.get('cake-autorate', 'wan', 'min_dl_shaper_rate_kbps'), '99999', 'do not revert a concurrent change');

    let cancelled;
    const cancel = fixture({ onAppend: () => cancelled.handleModalCancel(cancel.map, null, false) });
    cancelled = { handleModalCancel: () => {} };
    cancel.client.protectModal(cancelled);
    await assert.rejects(() => cancel.map.save(null, true), /cancelled/);
    assert.equal(cancel.writes, 0);
    assert.equal(cancel.notices.length, 0, 'explicit Cancel must not produce a late modal error');
    assert.equal(cancel.record, null);

    const foreign = fixture();
    foreign.data.set('network', 'wan', 'proto', 'pppoe');
    const foreignBefore = JSON.stringify(foreign.data.state);
    await assert.rejects(() => foreign.map.save(null, true), /Unrelated local/);
    assert.equal(JSON.stringify(foreign.data.state), foreignBefore);
    assert(!foreign.events.includes('parse'));
    assert.equal(foreign.notices.length, 1, 'pre-parse scope errors must be visible in silent modals');
    let applied = 0;
    await assert.rejects(() => fixture().client.apply(() => { applied++; }), /Unrelated pending/);
    const lateForeign = fixture({ pending: {}, onFinish: () => lateForeign.setPending({ network: [['set', 'wan', 'proto', 'pppoe']] }) });
    await assert.rejects(() => lateForeign.client.apply(() => { applied++; }), /Unrelated pending/);
    assert.equal(applied, 0, 'foreign pending packages must be checked again after validation');
    const allowed = fixture({ pending: { 'cake-autorate': [['set', 'wan', 'enabled', '1']] } });
    await allowed.client.apply(() => { applied++; });
    assert.equal(applied, 1);
    for (const options of [{ schemaFailure: true }, { emptySchema: true }]) {
        const f = fixture(options);
        await assert.rejects(() => f.map.save(), cause => !cause.message.includes('private-rpc'));
        assert.equal(f.writes, 0);
        assert(!f.events.includes('begin'));
    }
    console.log('candidate guard, HTTP-capable SHA256, cache/Apply/Cancel/identity/privacy tests passed');
})().catch(error => { console.error(error); process.exitCode = 1; });
