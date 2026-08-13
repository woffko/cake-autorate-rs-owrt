'use strict';
'require ui';

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
	ensureAppHeader: ensureAppHeader
});
