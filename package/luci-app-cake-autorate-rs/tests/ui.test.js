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
	createTextNode(value) {
		return { nodeType: 3, textContent: value };
	},
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
assert.deepEqual(header.text('<img src=x>'), { nodeType: 3, textContent: '<img src=x>' });
assert.equal(header.text(null).textContent, '');
assert.equal(header.text(0).textContent, '0');
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

{
	const link = header.docsLink('explicit-policy-route');
	assert.equal(link.attrs.href,
		'https://github.com/woffko/cake-autorate-rs-owrt/blob/main/USER_GUIDE.md#explicit-policy-route');
	assert.equal(link.attrs.rel, 'noopener noreferrer', 'documentation opens without referrer or opener');
	assert.throws(() => header.docsLink('javascript:alert(1)'), /unknown documentation section/,
		'only fixed user-guide sections can be linked');
	const guide = fs.readFileSync(path.join(__dirname, '../../../USER_GUIDE.md'), 'utf8');
	const anchors = new Set(guide.split('\n').filter(line => /^#+ /.test(line)).map(line =>
		line.replace(/^#+ /, '').trim().toLowerCase().replace(/[^\w\- ]/g, '').replace(/ /g, '-')));
	for (const section of source.match(/var DOCS_SECTIONS = \[([^\]]*)\]/)[1].match(/'[^']+'/g))
		assert(anchors.has(section.slice(1, -1)), 'USER_GUIDE.md must keep the linked section ' + section);
}
console.log('ui.js tests passed');
