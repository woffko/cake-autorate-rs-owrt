'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

function helper(source, name) {
    const begin = source.indexOf('function ' + name + '(');
    assert(begin >= 0, name);
    const end = source.indexOf('\nfunction ', begin + 1);
    return source.slice(begin, end < 0 ? source.length : end);
}
for (const variant of ['', '-lite']) {
    const source = fs.readFileSync(path.join(__dirname, '../../luci-app-cake-autorate-rs' + variant,
        'htdocs/luci-static/resources/view/cake-autorate-rs/settings.js'), 'utf8');
    const values = {}, stored = {};
    const uci = { get(_config, _id, key) { return stored[key]; } };
    const section = { formvalue(_id, key) { return values[key]; } };
    let observed;
    const sentinel = {};
    const clientSource = fs.readFileSync(path.join(__dirname, '../htdocs/luci-static/resources/cake-autorate-rs/candidate.js'), 'utf8');
    const guard = { directionEnabled: new Function('return (' + helper(clientSource, 'directionEnabled') + ');')(),
        validateRateTuple(tuple, direction, enabled) { observed = { tuple, direction, enabled }; return sentinel; } };
    const get = (_section, _id, key) => values[key] == null ? stored[key] : values[key];
    const checked = (_section, _id, key, fallback) => get(section, 'lab', key) == null ? fallback : get(section, 'lab', key) === '1';
    const configured = (_section, _id, key, fallback) => get(section, 'lab', key) == null ? fallback : get(section, 'lab', key);
    const validate = new Function('candidateGuard', 'uci', 'formOrUci', 'checkedFormOrUci', 'configured', 'checked',
        'return (' + helper(source, 'validateRateOrder') + ');')(guard, uci, get, checked, configured, checked);
    values.enabled = '1'; values.sqm_enabled = '1';
    stored.enabled = '1';
    values.min_dl_shaper_rate_kbps = '1'; values.base_dl_shaper_rate_kbps = '50'; values.max_dl_shaper_rate_kbps = '99';
    assert.equal(validate(section, 'lab', 'dl'), sentinel);
    assert.deepEqual(observed, { tuple: ['1', '50', '99'], direction: 'dl', enabled: true });
    values.adjust_dl_shaper_rate = '0';
    values.min_dl_shaper_rate_kbps = '0'; values.base_dl_shaper_rate_kbps = '0'; values.max_dl_shaper_rate_kbps = '0';
    assert.equal(validate(section, 'lab', 'dl'), sentinel);
    assert.equal(observed.enabled, false);
    values.adjust_dl_shaper_rate = '1'; values.sqm_direction_mode = 'upload_only';
    assert.equal(validate(section, 'lab', 'dl'), sentinel);
    assert.equal(observed.enabled, false, 'account for the directional write which happens during parse');
    values.sqm_direction_mode = 'both'; values.sqm_enabled = '0';
    assert.equal(validate(section, 'lab', 'dl'), sentinel);
    assert.equal(observed.enabled, false, 'disabled SQM overrides the retained adjust flag');
    if (variant) {
        values.enabled = '0'; values.sqm_enabled = '1';
        assert.equal(validate(section, 'lab', 'dl'), sentinel);
        assert.equal(observed.enabled, false, 'preview Lite enabled.write before native admission');
    }
    values.min_dl_shaper_rate_kbps = '10000junk'; values.base_dl_shaper_rate_kbps = '';
    assert.equal(validate(section, 'lab', 'dl'), sentinel);
    assert.deepEqual(observed.tuple, ['10000junk', '', '0'], 'do not coerce or hide invalid form input before shared validation');
    assert(!source.includes("'and(uinteger,min(100))'"), 'no separate 100 kbit/s rate floor');
    if (!variant) {
        const halfRate = new Function('return (' + helper(source, 'halfRate') + ');')();
        for (const [rate, half] of [['20000', '10000'], ['3', '2'], ['1', '1']])
            assert.equal(halfRate(rate), half);
        for (const [key, direction] of [['sqm_download', 'dl'], ['sqm_upload', 'ul']]) {
            const declaration = source.indexOf("o = value(section, 'setup', '" + key + "'");
            assert(declaration >= 0);
            const writer = source.slice(declaration).match(/o\.write = (function\(section_id, formvalue\) \{[\s\S]*?\n\t\});/);
            assert(writer, key);
            for (const manual of [false, true]) {
                const written = {};
                const write = new Function('manualRateLimitsEnabled', 'setCakeOption', 'halfRate',
                    'return (' + writer[1] + ');')(() => manual,
                    (_s, _id, option, value) => { written[option] = value; }, halfRate);
                write.call({ section: {} }, 'lab', '20000');
                assert.equal(written[key], '20000');
                for (const [bound, expected] of [['min', '10000'], ['base', '20000'], ['max', '20000']])
                    assert.equal(written[bound + '_' + direction + '_shaper_rate_kbps'], manual ? undefined : expected);
            }
        }
        const parse = new Function('return (' + helper(source, 'parsePositiveRate') + ');')();
        for (const input of ['10000junk', 'NaN', 'Infinity', '1e400', ' 1 ', '0x10']) assert.equal(parse(input), null, input);
        assert.equal(parse('1.5'), 1.5);
        assert.equal(parse('+1e3'), 1000);
    }
}
console.log('Full/Lite rate forms delegate exact values and direction semantics to the native-schema client');
