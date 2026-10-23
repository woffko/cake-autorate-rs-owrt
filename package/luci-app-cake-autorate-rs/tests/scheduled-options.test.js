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

// ---- the form declarations, executed against a recording stand-in for LuCI --------

function declare(block) {
    const options = {};
    const record = (key, kind, choices) => {
        const option = { key, kind, choices, dependencies: [],
            depends(first, second) {
                this.dependencies.push(typeof first === 'object' ? first : { [first]: second });
            } };
        options[key] = option;
        return option;
    };
    const value = (_section, _tab, key) => record(key, 'Value');
    const flag = (_section, _tab, key) => record(key, 'Flag');
    const listValue = (_section, _tab, key, _title, values) =>
        record(key, 'ListValue', values.map(entry => Array.isArray(entry) ? entry[0] : entry));
    new Function('section', 'value', 'flag', 'listValue', 'describe', '_', 'var o;\n' + block)(
        {}, value, flag, listValue, () => {}, text => text);
    return options;
}

const SCHEDULED = [
    'scheduled_autotune_interval_hours', 'scheduled_autotune_idle_window_s',
    'scheduled_autotune_window_start_hour', 'scheduled_autotune_window_end_hour',
    'scheduled_autotune_max_traffic_mb_day', 'scheduled_autotune_max_traffic_mb_month',
    'scheduled_autotune_auto_apply',
];

const capacity = declare(between("o = listValue(section, 'rates', 'capacity_learning_policy'",
    "o = value(section, 'rates', 'adaptive_ceiling_dl_cap_kbps'")).capacity_learning_policy;
const scheduled = declare(between("o = value(section, 'testing', 'scheduled_autotune_interval_hours'", '\n}\n'));

// "Runtime capacity learning" is the field that turns scheduled calibration on.
assert(capacity.choices.includes('scheduled_active'));
assert.deepEqual(capacity.dependencies, [{ autotune_profile: 'variable_link' }]);

// The schedule's own options are shown exactly while it is chosen. They used to
// depend on runtime_learning_mode, which only its write handler stores and which
// is no field of the form: LuCI takes a dependency's value from a field, so they
// were never shown.
assert.deepEqual(Object.keys(scheduled).sort(), [...SCHEDULED].sort());
for (const key of SCHEDULED)
    assert.deepEqual(scheduled[key].dependencies, [{ capacity_learning_policy: 'scheduled_active' }], key);

// ---- every dependency of the form names a field of the form ------------------------

// A field is declared through a taboption or option call, or through a helper
// called as helper(section, 'tab', 'key', ...).
const declared = new Set([
    ...[...source.matchAll(/\.taboption\(\s*'[a-z_]+'\s*,\s*form\.[A-Za-z]+\s*,\s*'([a-z0-9_]+)'/g)].map(m => m[1]),
    ...[...source.matchAll(/\.option\(\s*form\.[A-Za-z]+\s*,\s*'([a-z0-9_]+)'/g)].map(m => m[1]),
    ...[...source.matchAll(/\b[A-Za-z]+\(\s*section\s*,\s*'[a-z_]+'\s*,\s*'([a-z0-9_]+)'/g)].map(m => m[1]),
]);
const dependencyKeys = new Set([
    ...[...source.matchAll(/\.depends\(\s*'([a-z0-9_]+)'/g)].map(m => m[1]),
    ...[...source.matchAll(/\.depends\(\s*\{([^}]*)\}/g)]
        .flatMap(m => [...m[1].matchAll(/([a-z0-9_]+)\s*:/g)].map(k => k[1])),
]);
assert(declared.size > 100 && dependencyKeys.size > 10);
assert.deepEqual([...dependencyKeys].filter(key => !declared.has(key)), []);

console.log('Scheduled calibration options: shown with scheduled active calibration; every dependency names a field passed');
