'use strict';
'require fs';
'require poll';
'require uci';
'require ui';

var HISTORY_WINDOW_S = 3 * 60 * 60;
var HISTORY_INTERVAL_S = 10;
var HISTORY_MAX_KIB = 128;

function statusPath(section) {
	return '/var/run/cake-autorate/' + section + '/status.json';
}

function historyPath(section) {
	return '/var/run/cake-autorate/' + section + '/history.csv';
}

function readStatus(section) {
	return L.resolveDefault(fs.read_direct(statusPath(section)).then(JSON.parse), null);
}

function parseHistory(data) {
	var points = [];

	String(data || '').split(/\n/).forEach(function(line) {
		var fields, timestamp, rtt, cpu;

		if (!line)
			return;

		fields = line.split(',');
		if (fields.length < 3)
			return;

		timestamp = Number(fields[0]);
		rtt = fields[1] === '' ? null : Number(fields[1]);
		cpu = fields[2] === '' ? null : Number(fields[2]);
		if (!isFinite(timestamp) || timestamp <= 0)
			return;

		points.push({
			timestamp: timestamp,
			rtt: isFinite(rtt) ? rtt : null,
			cpu: isFinite(cpu) ? cpu : null
		});
	});

	return points;
}

function readHistory(section, enabled) {
	if (!enabled)
		return Promise.resolve([]);

	return L.resolveDefault(fs.read_direct(historyPath(section)).then(parseHistory), []);
}

function isEnabled(section) {
	return String(section.enabled || '0') === '1';
}

function historyEnabled(section) {
	return String(section.graph_history_enabled || '0') === '1';
}

function isActive(section, status) {
	return isEnabled(section) && status && status.state;
}

function formatMetric(value, suffix, precision) {
	if (value == null || value === '')
		return '-';

	value = Number(value);
	return isFinite(value) ? value.toFixed(precision) + suffix : '-';
}

function loadInstances(sections) {
	return Promise.all(sections.map(function(section) {
		var enabled = historyEnabled(section);

		return Promise.all([
			readStatus(section['.name']),
			readHistory(section['.name'], enabled)
		]).then(function(data) {
			return {
				section: section,
				status: data[0],
				history: data[1]
			};
		});
	}));
}

function setHistoryEnabled(section, enabled, button) {
	button.disabled = true;
	uci.set('cake-autorate', section, 'graph_history_enabled', enabled ? '1' : '0');

	return uci.save().then(function() {
		return uci.apply(30);
	}).then(function() {
		return fs.exec('/etc/init.d/cake-autorate', [ 'restart' ]);
	}).then(function(result) {
		if (result.code !== 0)
			throw new Error(result.stderr || _('Unable to restart CAKE Autorate.'));

		window.location = window.location.href.split('#')[0];
	}).catch(function(err) {
		button.disabled = false;
		ui.addNotification(null,
			E('p', _('Unable to change graph history: %s').format(err.message || err)),
			'error');
	});
}

function drawLine(ctx, points, valueKey, xFor, yFor, color) {
	var drawing = false;

	ctx.beginPath();
	ctx.strokeStyle = color;
	ctx.lineWidth = 1.7;
	for (var i = 0; i < points.length; i++) {
		var value = points[i][valueKey];
		if (value == null || !isFinite(value)) {
			drawing = false;
			continue;
		}

		if (drawing)
			ctx.lineTo(xFor(points[i].timestamp), yFor(value));
		else
			ctx.moveTo(xFor(points[i].timestamp), yFor(value));
		drawing = true;
	}
	ctx.stroke();
}

function drawChart(canvas, rawPoints) {
	var now = Date.now() / 1000;
	var from = now - HISTORY_WINDOW_S;
	var points = rawPoints.filter(function(point) {
		return point.timestamp >= from && point.timestamp <= now + 60;
	});
	var dpr = window.devicePixelRatio || 1;
	var width = Math.max(320, canvas.clientWidth || 720);
	var height = 230;
	var left = 48, right = 48, top = 30, bottom = 30;
	var plotWidth = width - left - right;
	var plotHeight = height - top - bottom;
	var ctx = canvas.getContext('2d');
	var rttMax = 10;

	canvas.width = Math.round(width * dpr);
	canvas.height = Math.round(height * dpr);
	ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
	ctx.clearRect(0, 0, width, height);
	ctx.font = '12px sans-serif';
	ctx.fillStyle = '#777';
	ctx.strokeStyle = 'rgba(127,127,127,0.28)';
	ctx.lineWidth = 1;

	points.forEach(function(point) {
		if (point.rtt != null && isFinite(point.rtt))
			rttMax = Math.max(rttMax, point.rtt);
	});
	rttMax = Math.ceil(rttMax / 10) * 10;

	function xFor(timestamp) {
		return left + Math.max(0, Math.min(1, (timestamp - from) / HISTORY_WINDOW_S)) * plotWidth;
	}

	function rttY(value) {
		return top + plotHeight - Math.max(0, Math.min(1, value / rttMax)) * plotHeight;
	}

	function cpuY(value) {
		return top + plotHeight - Math.max(0, Math.min(1, value / 100)) * plotHeight;
	}

	for (var grid = 0; grid <= 4; grid++) {
		var y = top + plotHeight * grid / 4;
		ctx.beginPath();
		ctx.moveTo(left, y);
		ctx.lineTo(width - right, y);
		ctx.stroke();
	}

	ctx.fillText(_('RTT %d ms').format(rttMax), 4, top + 4);
	ctx.fillText('0', 32, top + plotHeight + 4);
	ctx.textAlign = 'right';
	ctx.fillText('CPU 100%', width - 4, top + 4);
	ctx.fillText('0%', width - 4, top + plotHeight + 4);
	ctx.textAlign = 'left';
	ctx.fillText(_('-3 h'), left, height - 7);
	ctx.textAlign = 'right';
	ctx.fillText(_('now'), width - right, height - 7);
	ctx.textAlign = 'left';

	if (!points.length) {
		ctx.textAlign = 'center';
		ctx.fillText(_('Waiting for history samples…'), width / 2, height / 2);
		return;
	}

	drawLine(ctx, points, 'rtt', xFor, rttY, '#22a06b');
	drawLine(ctx, points, 'cpu', xFor, cpuY, '#6c5ce7');
}

function renderCard(instance) {
	var section = instance.section;
	var status = instance.status || {};
	var sectionName = section['.name'];
	var enabled = historyEnabled(section);
	var button = E('button', {
		'class': enabled ? 'btn cbi-button cbi-button-negative' : 'btn cbi-button cbi-button-action'
	}, enabled ? _('Disable history') : _('Enable history'));
	var body;

	button.addEventListener('click', function() {
		return setHistoryEnabled(sectionName, !enabled, button);
	});

	if (enabled) {
		var canvas = E('canvas', {
			'class': 'cake-graph-canvas',
			'role': 'img',
			'aria-label': _('RTT and CPU history for instance %s').format(sectionName)
		});
		body = E('div', { 'class': 'cake-graph-body' }, [
			E('div', { 'class': 'cake-graph-legend' }, [
				E('span', { 'class': 'cake-graph-rtt' },
					_('RTT: %s').format(formatMetric(status.rtt_ms, ' ms', 2))),
				E('span', { 'class': 'cake-graph-cpu' },
					_('CPU: %s').format(formatMetric(status.cpu_total_percent, '%', 1)))
			]),
			canvas
		]);
		window.requestAnimationFrame(function() {
			drawChart(canvas, instance.history || []);
		});
	} else {
		body = E('p', { 'class': 'cake-graph-disabled' },
			_('History is disabled for this instance. Live status continues to work without it.'));
	}

	return E('div', { 'class': 'cake-graph-card' }, [
		E('div', { 'class': 'cake-graph-header' }, [
			E('div', {}, [
				E('h3', {}, sectionName),
				E('small', {}, _('State: %s').format(String(status.state || '-').toUpperCase()))
			]),
			button
		]),
		body
	]);
}

function renderInstances(instances) {
	var active = instances.filter(function(instance) {
		return isActive(instance.section, instance.status);
	});

	if (!active.length)
		return E('div', { 'class': 'alert-message notice' }, _('No active instances.'));

	return E('div', { 'class': 'cake-graphs-grid' }, active.map(renderCard));
}

return L.view.extend({
	load: function() {
		return uci.load('cake-autorate').then(function() {
			var sections = uci.sections('cake-autorate', 'cake_autorate');
			return loadInstances(sections).then(function(instances) {
				return [ sections, instances ];
			});
		});
	},

	render: function(data) {
		var sections = data[0];
		var content = renderInstances(data[1]);
		var root = E('div', {}, [
			E('style', {}, [
				'.cake-graphs-warning{margin-bottom:18px}',
				'.cake-graphs-grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(360px,1fr));gap:16px}',
				'.cake-graph-card{border:1px solid rgba(127,127,127,.3);border-radius:6px;padding:14px;background:rgba(127,127,127,.04)}',
				'.cake-graph-header{display:flex;justify-content:space-between;align-items:center;gap:12px;margin-bottom:12px}',
				'.cake-graph-header h3{margin:0 0 3px}',
				'.cake-graph-legend{display:flex;gap:18px;margin-bottom:5px;font-weight:600}',
				'.cake-graph-rtt{color:#22a06b}.cake-graph-cpu{color:#6c5ce7}',
				'.cake-graph-canvas{display:block;width:100%;height:230px}',
				'.cake-graph-disabled{min-height:80px;display:flex;align-items:center;color:#777}',
				'@media(max-width:600px){.cake-graphs-grid{grid-template-columns:1fr}.cake-graph-header{align-items:flex-start}.cake-graph-canvas{height:210px}}'
			].join('')),
			E('div', { 'class': 'alert-message warning cake-graphs-warning' }, [
				E('strong', {}, _('Optional RAM history. ')),
				_('Enabling graphs stores samples only in /var/run (RAM), never in flash. Each active instance can use up to %d KiB. Data is cleared when the service stops or the router reboots. Samples are taken every %d seconds and the graph displays the last 3 hours.')
					.format(HISTORY_MAX_KIB, HISTORY_INTERVAL_S)
			]),
			content
		]);

		poll.add(function() {
			return loadInstances(sections).then(function(instances) {
				var nextContent = renderInstances(instances);
				if (content.parentNode) {
					content.parentNode.replaceChild(nextContent, content);
					content = nextContent;
				}
			});
		}, 5);

		return root;
	},

	handleSaveApply: null,
	handleSave: null,
	handleReset: null
});
