const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const source = fs.readFileSync(path.join(
  __dirname,
  '../htdocs/luci-static/resources/cake-autorate-rs/ui.js'
), 'utf8');

let inserted = null;
let removed = null;
let flushed = 0;
const tabList = {};
const legacyItem = {
	parentNode: {
		removeChild(node) {
			removed = node;
		},
	},
};
const legacyAnchor = {
	getAttribute(name) {
		return name === 'href' ?
			'/cgi-bin/luci/admin/network/cake-autorate-rs/priorities' : null;
	},
	closest(selector) {
		return selector === 'li' ? legacyItem : tabList;
	},
};
const tabs = {
  parentNode: {
    insertBefore(node, reference) {
      inserted = { node, reference };
    },
  },
};
const document = {
	querySelectorAll(selector) {
		return selector === 'a[href]' ? [ legacyAnchor ] : [];
	},
  querySelector(selector) {
    return selector.includes('ul.tabs') ? tabs : null;
  },
  getElementById() {
    return null;
  },
};
const ui = {
	menu: {
		flushCache() {
			flushed++;
		},
	},
};
const window = {
  requestAnimationFrame(callback) {
    callback();
  },
  setTimeout() {
    throw new Error('header insertion unexpectedly retried');
  },
};
const L = {
  Class: {
    extend(methods) {
      function LuCIClass() {}
      LuCIClass.prototype = methods;
      return LuCIClass;
    },
  },
};
const E = (tag, attrs, children) => ({ tag, attrs, children });
const translate = text => text;

const HeaderClass = new Function('window', 'document', 'L', 'E', '_', 'ui', source)(
	window, document, L, E, translate, ui
);

assert.equal(typeof HeaderClass, 'function');
const header = new HeaderClass();
assert.equal(typeof header.ensureAppHeader, 'function');
header.ensureAppHeader();
assert.equal(inserted.reference, tabs);
assert.equal(inserted.node.attrs.id, 'cake-autorate-app-header');
assert.equal(inserted.node.children[0].children, 'CAKE Autorate SQM');
assert.equal(removed, legacyItem,
	'stale top-level priorities tab must be removed after an ordinary reload');
assert.equal(flushed, 1,
	'stale LuCI session menu must be invalidated for the next navigation');

assert.equal(source.includes('setTimeout('), false,
	'header insertion must be driven by DOM mutation, not a retry timer');

let delayedInserted = null;
let delayedTabs = null;
let mutationCallback = null;
let observed = null;
let disconnected = 0;
let pagehide = null;
const delayedDocument = {
	documentElement: {},
	body: {},
	querySelectorAll() {
		return [];
	},
	querySelector() {
		return delayedTabs;
	},
	getElementById() {
		return null;
	},
};
const delayedWindow = {
	requestAnimationFrame(callback) {
		callback();
	},
	MutationObserver: class {
		constructor(callback) {
			mutationCallback = callback;
		}
		observe(node, options) {
			observed = { node, options };
		}
		disconnect() {
			disconnected++;
		}
	},
	addEventListener(name, callback, options) {
		if (name === 'pagehide')
			pagehide = { callback, options };
	},
};
const DelayedHeaderClass = new Function('window', 'document', 'L', 'E', '_', 'ui', source)(
	delayedWindow, delayedDocument, L, E, translate, ui
);
const delayedHeader = new DelayedHeaderClass();
delayedHeader.ensureAppHeader();
assert.equal(typeof mutationCallback, 'function',
	'missing tabs must install one DOM observer');
assert.equal(observed.node, delayedDocument.documentElement);
assert.deepEqual(observed.options, { childList: true, subtree: true });
assert.equal(pagehide.options.once, true,
	'observer lifetime must be bounded by the current page');

delayedTabs = {
	parentNode: {
		insertBefore(node, reference) {
			delayedInserted = { node, reference };
		},
	},
};
mutationCallback();
assert.equal(delayedInserted.reference, delayedTabs,
	'DOM mutation must insert the header as soon as tabs exist');
assert.equal(disconnected, 1,
	'successful insertion must disconnect the observer immediately');

console.log('ui.js tests passed');
