'use strict';

const assert = require('assert');
const fs = require('fs');
const path = require('path');

const aclPath = path.join(
	__dirname,
	'..',
	'root',
	'usr',
	'share',
	'rpcd',
	'acl.d',
	'luci-app-cake-autorate-rs.json',
);
const document = JSON.parse(fs.readFileSync(aclPath, 'utf8'));
const group = document['luci-app-cake-autorate-rs'];

assert(group, 'CAKE Autorate ACL group is missing');
assert.deepStrictEqual(
	group.read.uci,
	[ 'cake-autorate', 'sqm', 'mwan3', 'network' ],
	'Settings must be able to read logical/device mappings from UCI network',
);
assert.deepStrictEqual(
	group.write.uci,
	[ 'cake-autorate', 'sqm' ],
	'The app must not receive UCI write access to network or mwan3',
);

console.log('ACL tests passed');
