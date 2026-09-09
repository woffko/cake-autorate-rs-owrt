'use strict';
'require ui';
'require fs';

// LuCI E() treats string children as HTML. Backend/configuration text must
// cross this boundary as a Node, including when nested in a mixed child array.
function text(value) {
	return document.createTextNode(value == null ? '' : String(value));
}

// Opt in only for plain-text display components, not trusted rich help/layout.
// Constructed nodes keep their identity; strings in mixed arrays stay text.
function textElement(tag, attrs, children) {
	if (arguments.length === 2 && (typeof attrs !== 'object' || attrs instanceof Node || Array.isArray(attrs))) {
		children = attrs;
		attrs = {};
	}
	var childrenFlat = [];
	function child(value) {
		if (value == null || typeof value === 'boolean')
			return;
		if (Array.isArray(value))
			value.forEach(child);
		else
			childrenFlat.push(value instanceof Node ? value : text(value));
	}
	child(children);
	return E(tag, attrs || {}, childrenFlat);
}

function readNativeResult(args) {
	// Only completed read-only result documents can exceed rpcd's 256 KiB
	// stdout limit. Never route Start/Cancel/Apply mutation receipts here.
	if (!Array.isArray(args) || args.length !== 3 || args[0] !== '--calibrationctl' ||
	    [ 'rating-result', 'speedtest-result', 'autotune-result' ].indexOf(args[1]) < 0 ||
	    !/^[0-9a-f]{32}$/.test(args[2]))
		return Promise.reject(new Error(_('Invalid native result request.')));
	return fs.exec_direct('/usr/sbin/cake-autorated', args.slice(), 'text').then(function(output) {
		if (typeof output !== 'string' || !output.length || new Blob([ output ]).size > 512 * 1024)
			throw new Error(_('The native result is empty or exceeds its size limit.'));
		var result;
		try { result = JSON.parse(output); }
		catch (error) { throw new Error(_('The native result is incomplete or malformed.')); }
		if (!result || typeof result !== 'object' || Array.isArray(result))
			throw new Error(_('The native result has an invalid document type.'));
		// Callers still enforce operation, worker, route and manifest identity.
		// CGI has no child exit status; do not manufacture an exec code here.
		return result;
	});
}

function invalidateLegacyPrioritiesMenu() {
	var anchors = document.querySelectorAll('a[href]');
	var found = false;

	for (var i = 0; i < anchors.length; i++) {
		var href = anchors[i].getAttribute('href') || '';
		var menu = anchors[i].closest('.cbi-tabmenu, ul.tabs');

		if (!menu || !/\/cake-autorate-rs\/priorities(?:[?#]|$)/.test(href))
			continue;

		found = true;
		var item = anchors[i].closest('li');
		if (item && item.parentNode)
			item.parentNode.removeChild(item);
	}

	if (found && typeof ui !== 'undefined' && ui.menu &&
	    typeof ui.menu.flushCache === 'function')
		ui.menu.flushCache();
}

var appHeaderObserver = null;

function stopAppHeaderObserver() {
	if (!appHeaderObserver)
		return;

	appHeaderObserver.disconnect();
	appHeaderObserver = null;
}

function insertAppHeader() {
	invalidateLegacyPrioritiesMenu();
	var tabs = document.querySelector('.cbi-tabmenu, ul.tabs');

	if (document.getElementById('cake-autorate-app-header'))
		return true;
	if (!tabs || !tabs.parentNode)
		return false;

	tabs.parentNode.insertBefore(E('div', {
		'id': 'cake-autorate-app-header',
		'style': 'margin:0 0 16px'
	}, [
		E('h2', { 'style': 'margin:0 0 4px' }, _('CAKE Autorate SQM')),
		E('p', { 'style': 'margin:0;color:var(--text-color-medium,#666)' },
			_('Adaptive bandwidth control and SQM management for low latency under load.'))
	]), tabs);
	return true;
}

function ensureAppHeader() {
	window.requestAnimationFrame(function() {
		if (insertAppHeader()) {
			stopAppHeaderObserver();
			return;
		}
		if (appHeaderObserver || typeof window.MutationObserver !== 'function')
			return;

		appHeaderObserver = new window.MutationObserver(function() {
			if (insertAppHeader())
				stopAppHeaderObserver();
		});
		appHeaderObserver.observe(document.documentElement || document.body, {
			'childList': true,
			'subtree': true
		});
		window.addEventListener('pagehide', stopAppHeaderObserver, { 'once': true });
	});
}

return L.Class.extend({
	text: text,
	textElement: textElement,
	readNativeResult: readNativeResult,
	ensureAppHeader: ensureAppHeader
});
