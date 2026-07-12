'use strict';
'require fs';
'require poll';
'require uci';
'require ui';
'require cake-autorate-rs.ui as cakeUi';

var HISTORY_MAX_KIB = 128;
var HISTORY_INTERVALS = [ 1, 2, 5, 10, 15, 30, 60 ];
var GRAPH_POINT_SPACING_PX = 2;
var GRAPH_MAX_WIDTH_PX = 20000;
var graphScrollStates = {};

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
		var fields, timestamp, rtt, cpu, dl, ul;

		if (!line)
			return;

		fields = line.split(',');
		if (fields.length < 3)
			return;

		timestamp = Number(fields[0]);
		rtt = fields[1] === '' ? null : Number(fields[1]);
		cpu = fields[2] === '' ? null : Number(fields[2]);
		dl = fields.length < 4 || fields[3] === '' ? null : Number(fields[3]);
		ul = fields.length < 5 || fields[4] === '' ? null : Number(fields[4]);
		if (!isFinite(timestamp) || timestamp <= 0)
			return;

		points.push({
			timestamp: timestamp,
			rtt: rtt == null || !isFinite(rtt) ? null : rtt,
			cpu: cpu == null || !isFinite(cpu) ? null : cpu,
			dl: dl == null || !isFinite(dl) ? null : dl,
			ul: ul == null || !isFinite(ul) ? null : ul
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

function historyInterval(section) {
	var value = Number(section.graph_history_interval_s || 10);

	if (!isFinite(value) || value < 1 || value > 60)
		return 10;

	return Math.round(value);
}

function intervalLabel(seconds) {
	if (seconds === 60)
		return _('1 minute');

	return _('%d seconds').format(seconds);
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

function formatTime(timestamp, includeDate) {
	var date = new Date(timestamp * 1000);
	var time = date.toLocaleTimeString([], {
		hour: '2-digit',
		minute: '2-digit',
		second: '2-digit'
	});

	return includeDate ? date.toLocaleDateString() + ' ' + time : time;
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

function saveGraphConfig(section, control) {
	control.disabled = true;

	return uci.save().then(function() {
		return uci.apply(30);
	}).then(function() {
		return fs.exec('/etc/init.d/cake-autorate', [ 'restart' ]);
	}).then(function(result) {
		if (result.code !== 0)
			throw new Error(result.stderr || _('Unable to restart CAKE Autorate.'));

		window.location = window.location.href.split('#')[0];
	}).catch(function(err) {
		control.disabled = false;
		ui.addNotification(null,
			E('p', _('Unable to update graph settings: %s').format(err.message || err)),
			'error');
	});
}

function setHistoryEnabled(section, enabled, button) {
	uci.set('cake-autorate', section, 'graph_history_enabled', enabled ? '1' : '0');
	return saveGraphConfig(section, button);
}

function setHistoryInterval(section, interval, select, previous) {
	interval = Number(interval);
	if (!isFinite(interval) || interval < 1 || interval > 60) {
		select.value = String(previous);
		return Promise.resolve();
	}

	uci.set('cake-autorate', section, 'graph_history_interval_s', String(Math.round(interval)));
	return saveGraphConfig(section, select).then(function() {
		if (!select.disabled)
			select.value = String(previous);
	});
}

function drawLine(ctx, points, valueKey, xFor, yFor, color) {
	var drawing = false;

	ctx.beginPath();
	ctx.strokeStyle = color;
	ctx.lineWidth = 1.5;
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

function buildChartGeometry(rawPoints, interval, viewport) {
	var now = Date.now() / 1000;
	var points = rawPoints.filter(function(point) {
		return point.timestamp <= now + 60;
	}).sort(function(a, b) {
		return a.timestamp - b.timestamp;
	});
	var viewportWidth = Math.max(320, viewport.clientWidth || 720);
	var firstTimestamp = points.length ? points[0].timestamp : now;
	var lastTimestamp = points.length ? points[points.length - 1].timestamp : now;
	var span = Math.max(interval, lastTimestamp - firstTimestamp);
	var slots = Math.max(points.length - 1, Math.ceil(span / interval));
	var width = Math.min(GRAPH_MAX_WIDTH_PX,
		Math.max(viewportWidth, 96 + slots * GRAPH_POINT_SPACING_PX));
	var height = 220;
	var dpr = width > 4096 ? 1 : Math.min(window.devicePixelRatio || 1, 2);
	var left = 48, right = 48, top = 30, bottom = 34;
	var plotWidth = width - left - right;
	var plotHeight = height - top - bottom;
	var includeDate = span >= 24 * 60 * 60;

	return {
		points: points,
		width: width,
		height: height,
		dpr: dpr,
		left: left,
		right: right,
		top: top,
		bottom: bottom,
		plotWidth: plotWidth,
		plotHeight: plotHeight,
		firstTimestamp: firstTimestamp,
		lastTimestamp: lastTimestamp,
		includeDate: includeDate
	};
}

function prepareCanvas(canvas, geometry) {
	var ctx = canvas.getContext('2d');

	canvas.style.width = geometry.width + 'px';
	canvas.style.height = geometry.height + 'px';
	canvas.width = Math.round(geometry.width * geometry.dpr);
	canvas.height = Math.round(geometry.height * geometry.dpr);
	ctx.setTransform(geometry.dpr, 0, 0, geometry.dpr, 0, 0);
	ctx.clearRect(0, 0, geometry.width, geometry.height);
	ctx.font = '12px sans-serif';
	ctx.fillStyle = '#777';
	ctx.strokeStyle = 'rgba(127,127,127,0.28)';
	ctx.lineWidth = 1;

	return ctx;
}

function chartX(geometry, timestamp) {
	if (geometry.lastTimestamp <= geometry.firstTimestamp)
		return geometry.left + geometry.plotWidth;

	return geometry.left + Math.max(0, Math.min(1,
		(timestamp - geometry.firstTimestamp) /
		(geometry.lastTimestamp - geometry.firstTimestamp))) * geometry.plotWidth;
}

function drawChartGrid(ctx, geometry) {
	for (var grid = 0; grid <= 4; grid++) {
		var y = geometry.top + geometry.plotHeight * grid / 4;
		ctx.beginPath();
		ctx.moveTo(geometry.left, y);
		ctx.lineTo(geometry.width - geometry.right, y);
		ctx.stroke();
	}

	var tickCount = Math.max(2, Math.min(6, Math.floor(geometry.plotWidth / 140)));
	for (var tick = 0; tick <= tickCount; tick++) {
		var ratio = tick / tickCount;
		var x = geometry.left + geometry.plotWidth * ratio;
		var timestamp = geometry.firstTimestamp +
			(geometry.lastTimestamp - geometry.firstTimestamp) * ratio;

		ctx.beginPath();
		ctx.moveTo(x, geometry.top);
		ctx.lineTo(x, geometry.top + geometry.plotHeight);
		ctx.stroke();
		ctx.textAlign = tick === 0 ? 'left' : (tick === tickCount ? 'right' : 'center');
		ctx.fillText(formatTime(timestamp, geometry.includeDate), x, geometry.height - 8);
	}
	ctx.textAlign = 'left';
}

function drawLatencyChart(canvas, geometry) {
	var ctx = prepareCanvas(canvas, geometry);
	var rttMax = 10;

	geometry.points.forEach(function(point) {
		if (point.rtt != null && isFinite(point.rtt))
			rttMax = Math.max(rttMax, point.rtt);
	});
	rttMax = Math.ceil(rttMax / 10) * 10;
	drawChartGrid(ctx, geometry);

	function rttY(value) {
		return geometry.top + geometry.plotHeight -
			Math.max(0, Math.min(1, value / rttMax)) * geometry.plotHeight;
	}

	function cpuY(value) {
		return geometry.top + geometry.plotHeight -
			Math.max(0, Math.min(1, value / 100)) * geometry.plotHeight;
	}

	ctx.fillText(_('RTT %d ms').format(rttMax), 4, geometry.top + 4);
	ctx.fillText('0', 32, geometry.top + geometry.plotHeight + 4);
	ctx.textAlign = 'right';
	ctx.fillText('CPU 100%', geometry.width - 4, geometry.top + 4);
	ctx.fillText('0%', geometry.width - 4, geometry.top + geometry.plotHeight + 4);
	ctx.textAlign = 'left';

	if (!geometry.points.length) {
		ctx.textAlign = 'center';
		ctx.fillText(_('Waiting for history samples…'), geometry.width / 2, geometry.height / 2);
		return;
	}

	drawLine(ctx, geometry.points, 'rtt', function(timestamp) {
		return chartX(geometry, timestamp);
	}, rttY, '#22a06b');
	drawLine(ctx, geometry.points, 'cpu', function(timestamp) {
		return chartX(geometry, timestamp);
	}, cpuY, '#6c5ce7');
}

function niceRateCeiling(value) {
	if (!isFinite(value) || value <= 0)
		return 100;

	var magnitude = Math.pow(10, Math.floor(Math.log(value) / Math.LN10));
	var normalized = value / magnitude;
	var nice = normalized <= 1 ? 1 : (normalized <= 2 ? 2 : (normalized <= 5 ? 5 : 10));
	return nice * magnitude;
}

function formatTrafficRate(value) {
	if (value == null || !isFinite(value))
		return '-';

	return value >= 1000 ? (value / 1000).toFixed(2) + ' Mbps' : value.toFixed(0) + ' kbps';
}

function drawTrafficChart(canvas, geometry) {
	var ctx = prepareCanvas(canvas, geometry);
	var rateMax = 100;

	geometry.points.forEach(function(point) {
		if (point.dl != null && isFinite(point.dl))
			rateMax = Math.max(rateMax, point.dl);
		if (point.ul != null && isFinite(point.ul))
			rateMax = Math.max(rateMax, point.ul);
	});
	rateMax = niceRateCeiling(rateMax);
	drawChartGrid(ctx, geometry);

	function rateY(value) {
		return geometry.top + geometry.plotHeight -
			Math.max(0, Math.min(1, value / rateMax)) * geometry.plotHeight;
	}

	ctx.fillText(_('Traffic %s').format(formatTrafficRate(rateMax)), 4, geometry.top + 4);
	ctx.fillText('0', 32, geometry.top + geometry.plotHeight + 4);

	if (!geometry.points.length) {
		ctx.textAlign = 'center';
		ctx.fillText(_('Waiting for history samples…'), geometry.width / 2, geometry.height / 2);
		return;
	}

	drawLine(ctx, geometry.points, 'dl', function(timestamp) {
		return chartX(geometry, timestamp);
	}, rateY, '#2980b9');
	drawLine(ctx, geometry.points, 'ul', function(timestamp) {
		return chartX(geometry, timestamp);
	}, rateY, '#e67e22');
}

function nearestPoint(points, timestamp) {
	var low = 0;
	var high = points.length - 1;

	while (low < high) {
		var middle = Math.floor((low + high) / 2);
		if (points[middle].timestamp < timestamp)
			low = middle + 1;
		else
			high = middle;
	}

	if (low > 0 && Math.abs(points[low - 1].timestamp - timestamp) <=
		Math.abs(points[low].timestamp - timestamp))
		return points[low - 1];

	return points[low];
}

function bindHover(canvas, geometry, hoverInfo) {
	canvas.addEventListener('mousemove', function(ev) {
		if (!geometry.points.length)
			return;

		var rect = canvas.getBoundingClientRect();
		var logicalX = (ev.clientX - rect.left) * geometry.width / rect.width;
		if (logicalX < geometry.left || logicalX > geometry.width - geometry.right) {
			hoverInfo.style.visibility = 'hidden';
			return;
		}

		var ratio = (logicalX - geometry.left) / geometry.plotWidth;
		var timestamp = geometry.firstTimestamp +
			(geometry.lastTimestamp - geometry.firstTimestamp) * ratio;
		var point = nearestPoint(geometry.points, timestamp);

		hoverInfo.textContent = '%s · RTT %s · CPU %s · DL %s · UL %s'.format(
			new Date(point.timestamp * 1000).toLocaleString(),
			formatMetric(point.rtt, ' ms', 3),
			formatMetric(point.cpu, '%', 1),
			formatTrafficRate(point.dl),
			formatTrafficRate(point.ul));
		hoverInfo.style.visibility = 'visible';
	});

	canvas.addEventListener('mouseleave', function() {
		hoverInfo.style.visibility = 'hidden';
	});
}

function scrollState(section) {
	if (!graphScrollStates[section])
		graphScrollStates[section] = { left: 0, followLatest: true };

	return graphScrollStates[section];
}

function bindScroll(viewport, latestButton, section) {
	var state = scrollState(section);

	function updateButton() {
		latestButton.disabled = state.followLatest;
	}

	viewport.addEventListener('scroll', function() {
		var maxScroll = Math.max(0, viewport.scrollWidth - viewport.clientWidth);
		state.left = viewport.scrollLeft;
		state.followLatest = maxScroll - viewport.scrollLeft <= 8;
		updateButton();
	});

	latestButton.addEventListener('click', function() {
		state.followLatest = true;
		viewport.scrollLeft = Math.max(0, viewport.scrollWidth - viewport.clientWidth);
		state.left = viewport.scrollLeft;
		updateButton();
	});

	window.requestAnimationFrame(function() {
		var maxScroll = Math.max(0, viewport.scrollWidth - viewport.clientWidth);
		viewport.scrollLeft = state.followLatest ? maxScroll : Math.min(state.left, maxScroll);
		state.left = viewport.scrollLeft;
		updateButton();
	});
}

function renderIntervalSelect(sectionName, interval) {
	var select = E('select', {
		'class': 'cbi-input-select cake-graph-interval',
		'title': _('Graph sample interval')
	}, HISTORY_INTERVALS.map(function(value) {
		return E('option', { 'value': String(value) }, intervalLabel(value));
	}));

	select.value = String(interval);
	select.addEventListener('change', function() {
		return setHistoryInterval(sectionName, select.value, select, interval);
	});

	return E('label', { 'class': 'cake-graph-interval-label' }, [
		E('span', {}, _('Sample interval')),
		select
	]);
}

function renderCard(instance) {
	var section = instance.section;
	var status = instance.status || {};
	var sectionName = section['.name'];
	var enabled = historyEnabled(section);
	var interval = historyInterval(section);
	var button = E('button', {
		'class': enabled ? 'btn cbi-button cbi-button-negative' : 'btn cbi-button cbi-button-action'
	}, enabled ? _('Disable history') : _('Enable history'));
	var body;

	button.addEventListener('click', function() {
		return setHistoryEnabled(sectionName, !enabled, button);
	});

	if (enabled) {
		var latencyCanvas = E('canvas', {
			'class': 'cake-graph-canvas',
			'role': 'img',
			'aria-label': _('RTT and CPU history for instance %s').format(sectionName)
		});
		var trafficCanvas = E('canvas', {
			'class': 'cake-graph-canvas',
			'role': 'img',
			'aria-label': _('Download and upload traffic history for instance %s').format(sectionName)
		});
		var track = E('div', { 'class': 'cake-graph-track' }, [
			E('div', { 'class': 'cake-graph-chart-title' }, _('Latency and CPU')),
			latencyCanvas,
			E('div', { 'class': 'cake-graph-chart-title' }, _('Download and upload traffic')),
			trafficCanvas
		]);
		var viewport = E('div', { 'class': 'cake-graph-scroll' }, track);
		var hoverInfo = E('div', {
			'class': 'cake-graph-hover',
			'style': 'visibility:hidden'
		}, _('Move the pointer over a graph to inspect a sample.'));
		var latestButton = E('button', {
			'class': 'btn cbi-button cbi-button-neutral cake-graph-latest'
		}, _('Latest'));

		body = E('div', { 'class': 'cake-graph-body' }, [
			E('div', { 'class': 'cake-graph-legend' }, [
				E('span', { 'class': 'cake-graph-rtt' },
					_('RTT: %s').format(formatMetric(status.rtt_ms, ' ms', 2))),
				E('span', { 'class': 'cake-graph-cpu' },
					_('CPU: %s').format(formatMetric(status.cpu_total_percent, '%', 1))),
				E('span', { 'class': 'cake-graph-dl' },
					_('DL: %s').format(formatTrafficRate(status.dl_achieved_rate_kbps))),
				E('span', { 'class': 'cake-graph-ul' },
					_('UL: %s').format(formatTrafficRate(status.ul_achieved_rate_kbps))),
				E('span', { 'class': 'cake-graph-samples' },
					_('Stored: %d samples').format((instance.history || []).length)),
				latestButton
			]),
			hoverInfo,
			viewport
		]);
		window.requestAnimationFrame(function() {
			var geometry = buildChartGeometry(instance.history || [], interval, viewport);
			track.style.width = geometry.width + 'px';
			drawLatencyChart(latencyCanvas, geometry);
			drawTrafficChart(trafficCanvas, geometry);
			bindHover(latencyCanvas, geometry, hoverInfo);
			bindHover(trafficCanvas, geometry, hoverInfo);
			bindScroll(viewport, latestButton, sectionName);
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
			E('div', { 'class': 'cake-graph-actions' }, [
				renderIntervalSelect(sectionName, interval),
				button
			])
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
		cakeUi.ensureAppHeader();
		var sections = data[0];
		var content = renderInstances(data[1]);
		var root = E('div', {}, [
			E('style', {}, [
				'.cake-graphs-warning{margin-bottom:18px}',
				'.cake-graphs-grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(360px,1fr));gap:16px}',
				'.cake-graph-card{min-width:0;border:1px solid rgba(127,127,127,.3);border-radius:6px;padding:14px;background:rgba(127,127,127,.04)}',
				'.cake-graph-header{display:flex;justify-content:space-between;align-items:center;gap:12px;margin-bottom:12px}',
				'.cake-graph-header h3{margin:0 0 3px}',
				'.cake-graph-actions{display:flex;align-items:flex-end;gap:10px;flex-wrap:wrap;justify-content:flex-end}',
				'.cake-graph-interval-label{display:flex;flex-direction:column;gap:3px;font-size:12px}',
				'.cake-graph-interval{min-width:120px}',
				'.cake-graph-legend{display:flex;align-items:center;gap:18px;margin-bottom:5px;font-weight:600;flex-wrap:wrap}',
				'.cake-graph-rtt{color:#22a06b}.cake-graph-cpu{color:#6c5ce7}.cake-graph-dl{color:#2980b9}.cake-graph-ul{color:#e67e22}.cake-graph-samples{color:#777;font-weight:400}',
				'.cake-graph-latest{margin-left:auto}',
				'.cake-graph-hover{min-height:1.5em;margin:3px 0 6px;padding:4px 7px;border-radius:4px;background:rgba(127,127,127,.12);font-variant-numeric:tabular-nums}',
				'.cake-graph-scroll{display:block;width:100%;overflow-x:auto;overflow-y:hidden;scrollbar-gutter:stable}',
				'.cake-graph-track{display:block;max-width:none}',
				'.cake-graph-chart-title{position:sticky;left:0;z-index:1;display:inline-block;margin:5px 0 1px;padding:2px 6px;border-radius:3px;background:var(--background-color-high,#fff);font-weight:600}',
				'.cake-graph-canvas{display:block;max-width:none;height:220px}',
				'.cake-graph-disabled{min-height:80px;display:flex;align-items:center;color:#777}',
				'@media(max-width:600px){.cake-graphs-grid{grid-template-columns:1fr}.cake-graph-header{align-items:flex-start;flex-direction:column}.cake-graph-actions{width:100%;justify-content:space-between}.cake-graph-canvas{height:200px}.cake-graph-latest{margin-left:0}}'
			].join('')),
			E('div', { 'class': 'alert-message warning cake-graphs-warning' }, [
				E('strong', {}, _('Optional RAM history. ')),
				_('Enabling graphs stores RTT, CPU, download, and upload samples only in /var/run (RAM), never in flash. Each active instance can use up to %d KiB. Choose a per-instance interval from 1 second to 1 minute; shorter intervals fill the limit sooner. Oldest samples are discarded automatically. Both charts share one horizontal timeline and follow the newest data until you scroll back. Hover over either chart for exact values. Data is cleared when the service stops or the router reboots.')
					.format(HISTORY_MAX_KIB)
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
