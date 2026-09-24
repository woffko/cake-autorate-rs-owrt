'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

for (const variant of ['', '-lite']) {
    const source = fs.readFileSync(path.join(__dirname, '../../luci-app-cake-autorate-rs' + variant,
        'htdocs/luci-static/resources/view/cake-autorate-rs/settings.js'), 'utf8');
    const options = {};
    function option(_section, _tab, key, _title, type, initial) {
        const result = { default: initial, datatype: type, dependencies: [],
            depends(key, value) { this.dependencies.push([key, value]); } };
        options[key] = result;
        return result;
    }
    const flag = (s, tab, key, title, initial) => option(s, tab, key, title, null, initial);
    // Execute the actual form declarations, with no LuCI instance or network.
    const start = source.lastIndexOf('\n', source.indexOf("'external_ip_check_enabled', _('Look up external IPv4 address')"));
    const endKey = variant ? "'ul_if', _('Upload device')" : "'rx_bytes_path', _('RX bytes path')";
    const end = source.lastIndexOf('\n', source.indexOf(endKey, start));
    assert(start >= 0 && end > start);
    new Function('flag', 'value', 'addFlag', 'addValue', '_', 'section', 's',
        'var o;\n' + source.slice(start, end))(flag, option, flag, option, x => x, {}, {});
    assert.equal(options.external_ip_check_enabled.default, '0');
    assert.match(options.external_ip_check_enabled.description, /sees your public address/);
    assert.equal(options.external_ip_check_url.default, 'https://api.ipify.org');
    assert.equal(options.external_ip_check_interval_s.default, '3600');
    assert.equal(options.external_ip_check_interval_s.datatype, 'and(uinteger,min(60),max(604800))');
    for (const key of ['external_ip_check_url', 'external_ip_check_interval_s'])
        assert.deepEqual(options[key].dependencies, [['external_ip_check_enabled', '1']]);
}
console.log('Full/Lite external IP settings are opt-in with visible privacy notice and bounded cadence');
