'use strict';

function ensureAppHeader() {
	window.requestAnimationFrame(function insertHeader() {
		var tabs = document.querySelector('.cbi-tabmenu');

		if (!tabs || !tabs.parentNode) {
			window.setTimeout(insertHeader, 50);
			return;
		}

		if (document.getElementById('cake-autorate-app-header'))
			return;

		tabs.parentNode.insertBefore(E('div', {
			'id': 'cake-autorate-app-header',
			'style': 'margin:0 0 16px'
		}, [
			E('h2', { 'style': 'margin:0 0 4px' }, _('CAKE Autorate SQM')),
			E('p', { 'style': 'margin:0;color:var(--text-color-medium,#666)' },
				_('Adaptive bandwidth control and SQM management for low latency under load.'))
		]), tabs);
	});
}

return {
	ensureAppHeader: ensureAppHeader
};
