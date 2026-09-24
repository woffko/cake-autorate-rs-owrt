'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const source = fs.readFileSync(path.join(__dirname, '../htdocs/luci-static/resources/view/cake-autorate-rs/status.js'), 'utf8');
const prefix = source.slice(0, source.indexOf('return L.view.extend'));
const E = (tag, attrs, children) => ({ tag, attrs, children: Array.isArray(children) ? children : [children] });
const document = { createTextNode: data => ({ nodeType: 3, data }) };
const helpers = new Function('fs', 'poll', 'uci', 'L', 'E', '_', 'document',
	prefix + '\nreturn { stateCell };')({}, {}, {}, {}, E, value => value, document);
const collect = node => node.nodeType === 3 ? node.data : node.children.map(collect).join('\n');
const payload = '<img src=x onerror="bad()">';
const degraded = helpers.stateCell({ config: { enabled: '1' }, status: {
	state: 'RUNNING', runtime_control_held: true, runtime_control_degraded: true, runtime_control_error: payload
} });
assert.match(collect(degraded), /runtime owner could not be verified/);
assert.ok(degraded.children.some(node => node.attrs.role === 'alert'));
assert.equal(degraded.children[2].children[0].nodeType, 3);
assert.equal(degraded.children[2].children[0].data, payload);
const held = helpers.stateCell({ config: { enabled: '1' }, status: { state: 'RUNNING', runtime_control_held: true } });
assert.match(collect(held), /held by an active operation/);
assert.doesNotMatch(collect(held), /could not be verified/);
const normal = helpers.stateCell({ config: { enabled: '1' }, status: { state: 'RUNNING' } });
assert.equal(normal.tag, 'span');
assert.equal(collect(normal), 'RUNNING');
const external = helpers.stateCell({ config: { enabled: '1' }, status: {
	state: 'WAITING_EXTERNAL_SQM', sqm_runtime_managed: false, sqm_runtime_healthy: false,
	sqm_runtime_reason: payload
} });
assert.equal(external.attrs.title, payload);
assert.match(external.attrs.style, /d94141/);
console.log('Lite runtime owner status tests passed');
