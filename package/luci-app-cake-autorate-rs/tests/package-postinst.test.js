'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const makefile = fs.readFileSync(path.join(__dirname, '..', 'Makefile'), 'utf8');

assert.match(makefile, /rm -f \/tmp\/luci-indexcache \/tmp\/luci-indexcache\.\*/);
assert.match(makefile, /rm -rf \/tmp\/luci-modulecache \/tmp\/luci-modulecache\.\*/);
assert.match(makefile, /\/etc\/init\.d\/rpcd reload >\/dev\/null 2>&1 \|\| true/);
assert.doesNotMatch(makefile, /rm -f \/tmp\/luci-indexcache\.\*;/,
	'the unsuffixed LuCI index cache must not survive package replacement');

console.log('package post-install tests passed');
