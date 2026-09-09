'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const source = fs.readFileSync(path.join(__dirname,
    '../htdocs/luci-static/resources/view/cake-autorate-rs/settings.js'), 'utf8');

function fixture(entries, script = 'layer_cake.qos', failList = false, revertStatus = 0) {
    const sections = [{ '.name': 'primary', '.type': 'cake_autorate', sqm_script: script }];
    const options = [];
    const calls = [];
    const section = {
        children: options,
        tab() {},
        option(type, name) {
            const option = { option: name, type, choices: [], dependencies: [],
                value(value) { this.choices.push(value); },
                depends(...args) { this.dependencies.push(args); } };
            options.push(option);
            return option;
        },
        taboption(tab, type, name) {
            const option = this.option(type, name);
            option.tab = tab;
            return option;
        }
    };
    const context = vm.createContext({
        form: { Map: class { section() { return section; } render() { return this; } },
            GridSection: 'GridSection', DummyValue: 'DummyValue', Value: 'Value',
            Flag: 'Flag', ListValue: 'ListValue', DynamicList: 'DynamicList' },
        uci: { load: () => Promise.resolve(), sections: () => sections,
            get: (_config, _section, key) => sections[0][key],
            unload: configs => calls.push(['unload', ...configs]) },
        rpc: { declare: spec => {
            assert.equal(spec.object, 'uci');
            assert.equal(spec.method, 'revert');
            assert.equal(spec.reject, false);
            return config => {
                calls.push(['revert', config]);
                return revertStatus instanceof Error ? Promise.reject(revertStatus) : Promise.resolve(revertStatus);
            };
        } },
        window: { location: { reload: () => calls.push(['reload']) } },
        fs: { list: (directory) => {
            assert.equal(directory, '/usr/lib/sqm');
            return failList ? Promise.reject(new Error('denied')) : Promise.resolve(entries);
        } },
        ui: {}, _: s => s,
        L: { view: { extend: view => view }, resolveDefault: (p, fallback) => p.catch(() => fallback) }
    });
    vm.runInContext('String.prototype.format = function(...args) { let i=0; return this.replace(/%s/g, () => args[i++]); };', context);
    const view = vm.runInContext('(function() {\n' + source + '\n})()', context);
    return { view, options, calls };
}

(async () => {
    const main = fixture([{ name: 'layer_cake.qos', type: 'file' },
        { name: 'custom_cake.qos', type: 'file' }, { name: 'cake.qos', type: 'file' },
        { name: '../escape.qos', type: 'file' }, { name: 'not-a-script.txt', type: 'file' },
        { name: 'directory.qos', type: 'directory' }]);
    main.view.render(await main.view.load());
    assert.deepEqual(main.options.filter(o => !o.modalonly).map(o => o.option),
        ['_enabled', '_target', '_rates'], 'grid must contain only its three summary columns');
    const editable = main.options.filter(o => o.type !== 'DummyValue');
    assert(editable.length > 30);
    for (const option of editable) {
        assert.equal(option.modalonly, true, option.option + ' must stay in the editor');
        assert.equal(option.retain, true, option.option + ' must preserve dependency-hidden values');
    }
    const choices = main.options.find(o => o.option === 'sqm_script').choices;
    assert(choices.includes('layer_cake.qos'), 'must preserve the real configured SQM script');
    assert(choices.includes('custom_cake.qos'), 'must discover installed scripts, not just hardcode layer_cake');
    assert(!choices.includes('../escape.qos'));
    assert(!choices.includes('not-a-script.txt'));
    assert(!choices.includes('directory.qos'));
    assert.equal(new Set(choices).size, choices.length);
    const fallback = fixture([], 'retained_custom.qos', true);
    fallback.view.render(await fallback.view.load());
    assert(fallback.options.find(o => o.option === 'sqm_script').choices.includes('retained_custom.qos'),
        'failed directory discovery must not silently replace a configured script');
    assert.equal(typeof main.view.handleReset, 'function', 'Reset must discard the saved session delta explicitly');
    await main.view.handleReset();
    assert.deepEqual(main.calls, [['revert', 'cake-autorate'], ['unload', 'cake-autorate'], ['reload']],
        'Reset must affect only its owned config and reload only after successful revert');
    for (const status of [7, new Error('transport failed')]) {
        const failed = fixture([], 'layer_cake.qos', false, status);
        await assert.rejects(() => failed.view.handleReset());
        assert.deepEqual(failed.calls, [['revert', 'cake-autorate']],
            'failed revert must not clear cache or falsely reload as if Reset succeeded');
    }
    console.log('Lite form layout, dormant values and SQM script discovery tests passed');
})().catch(error => { console.error(error); process.exitCode = 1; });
