const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const source = fs.readFileSync(path.join(
  __dirname,
  '../htdocs/luci-static/resources/cake-autorate-rs/ui.js'
), 'utf8');

let inserted = null;
const tabs = {
  parentNode: {
    insertBefore(node, reference) {
      inserted = { node, reference };
    },
  },
};
const document = {
  querySelector(selector) {
    return selector.includes('ul.tabs') ? tabs : null;
  },
  getElementById() {
    return null;
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

const HeaderClass = new Function('window', 'document', 'L', 'E', '_', source)(
  window, document, L, E, translate
);

assert.equal(typeof HeaderClass, 'function');
const header = new HeaderClass();
assert.equal(typeof header.ensureAppHeader, 'function');
header.ensureAppHeader();
assert.equal(inserted.reference, tabs);
assert.equal(inserted.node.attrs.id, 'cake-autorate-app-header');
assert.equal(inserted.node.children[0].children, 'CAKE Autorate SQM');

console.log('ui.js tests passed');
