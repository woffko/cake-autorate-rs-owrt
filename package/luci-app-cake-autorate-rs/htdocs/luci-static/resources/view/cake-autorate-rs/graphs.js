'use strict';
'require fs';
'require poll';
'require uci';
'require ui';
'require cake-autorate-rs.ui as cakeUi';

var HISTORY_INTERVALS = [ 1, 2, 5, 10, 15, 30, 60 ];
var HISTORY_BUDGETS_KIB = [ 256, 512, 1024, 2048, 4096, 8192, 16384, 32768, 65536, 102400 ];
var HISTORY_PAGE_SAMPLES = 10000;
var GRAPH_POINT_SPACING_PX = 2;
var GRAPH_MAX_WIDTH_PX = 20000;
var GRAPH_EVENT_CLUSTER_DISTANCE_PX = 12;
var GRAPH_EVENT_LABEL_LANES = 3;
var GRAPH_EVENT_LABEL_GAP_PX = 5;
var graphScrollStates = {};
var graphPageOffsets = {};
var graphFloorStates = {};
var graphRefreshNow = null;

function statusPath(section) {
	return '/var/run/cake-autorate/' + section + '/status.json';
}

function historyPath(section) {
	return '/var/run/cake-autorate/' + section + '/history.csv';
}

function readStatus(section) {
	return L.resolveDefault(fs.read(statusPath(section)).then(JSON.parse), null);
}

function parseHistory(data) {
	var points = [];

	String(data || '').split(/\n/).forEach(function(line) {
		var fields, timestamp, rtt, cpu, dl, ul, transport, effective, dlFloor, ulFloor,
			uplinkState, routeIdentity, grade, gradeState, gradeIncrease, ratingPhase,
			ratingDlSamples, ratingUlSamples;

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
		transport = fields.length < 6 || fields[5] === '' ? null : Number(fields[5]);
		effective = fields.length < 7 || fields[6] === '' ? null : Number(fields[6]);
		dlFloor = fields.length < 8 || fields[7] === '' ? null : Number(fields[7]);
		ulFloor = fields.length < 9 || fields[8] === '' ? null : Number(fields[8]);
		uplinkState = fields.length < 10 ? '' : fields[9];
		routeIdentity = fields.length < 11 ? '' : fields[10];
		grade = fields.length < 12 ? '' : fields[11];
		gradeState = fields.length < 13 ? '' : fields[12];
		gradeIncrease = fields.length < 14 || fields[13] === '' ? null : Number(fields[13]);
		ratingPhase = fields.length < 15 ? '' : fields[14];
		ratingDlSamples = fields.length < 16 || fields[15] === '' ? null : Number(fields[15]);
		ratingUlSamples = fields.length < 17 || fields[16] === '' ? null : Number(fields[16]);
		if (!isFinite(timestamp) || timestamp <= 0)
			return;

		points.push({
			timestamp: timestamp,
			rtt: rtt == null || !isFinite(rtt) ? null : rtt,
			cpu: cpu == null || !isFinite(cpu) ? null : cpu,
			dl: dl == null || !isFinite(dl) ? null : dl,
			ul: ul == null || !isFinite(ul) ? null : ul,
			transport: transport == null || !isFinite(transport) ? null : transport,
			effective: effective == null || !isFinite(effective) ? null : effective,
			dlFloor: dlFloor == null || !isFinite(dlFloor) ? null : dlFloor,
			ulFloor: ulFloor == null || !isFinite(ulFloor) ? null : ulFloor,
			uplinkState: uplinkState,
			routeIdentity: routeIdentity,
			grade: grade,
			gradeState: gradeState,
			gradeIncrease: gradeIncrease == null || !isFinite(gradeIncrease) ? null : gradeIncrease,
			ratingPhase: ratingPhase,
			ratingDlSamples: ratingDlSamples == null || !isFinite(ratingDlSamples) ? null : ratingDlSamples,
			ratingUlSamples: ratingUlSamples == null || !isFinite(ratingUlSamples) ? null : ratingUlSamples
		});
	});

	return points;
}

function readHistory(section, enabled, offset, total) {
	if (!enabled)
		return Promise.resolve([]);

	offset = Math.max(0, Number(offset || 0));
	return fs.exec('/usr/libexec/cake-autorate-rs/graph-history', [
		section, 'read', String(offset), String(HISTORY_PAGE_SAMPLES)
	]).then(function(result) {
		if (result.code !== 0)
			throw new Error(result.stderr || 'history helper failed');
		var points = parseHistory(result.stdout || '');
		var available = Math.max(0, Number(total || points.length) - offset);
		return points.slice(0, Math.min(HISTORY_PAGE_SAMPLES, available || points.length));
	}).catch(function() {
		return L.resolveDefault(fs.read_direct(historyPath(section)).then(function(data) {
			var points = parseHistory(data);
			var end = Math.max(0, points.length - offset);
			return points.slice(Math.max(0, end - HISTORY_PAGE_SAMPLES), end);
		}), []);
	});
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
		var sectionName = section['.name'];

		return readStatus(sectionName).then(function(status) {
			var total = Number(status && status.graph_history_stored_samples || 0);
			var offset = Math.max(0, Number(graphPageOffsets[sectionName] || 0));
			if (total > 0)
				offset = Math.min(offset, Math.max(0, total - 1));
			graphPageOffsets[sectionName] = offset;
			return readHistory(sectionName, enabled, offset, total).then(function(history) {
			return {
				section: section,
				status: status,
				history: history,
				historyOffset: offset,
				historyTotal: total || history.length
			};
			});
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

function formatMemoryKib(kib) {
	if (kib == null || kib === '')
		return '-';
	kib = Number(kib);
	if (!isFinite(kib) || kib < 0)
		return '-';
	if (kib >= 1024 * 1024)
		return (kib / 1024 / 1024).toFixed(2) + ' GiB';
	if (kib >= 1024)
		return (kib / 1024).toFixed(kib >= 10 * 1024 ? 0 : 1) + ' MiB';
	return Math.round(kib) + ' KiB';
}

function budgetLabel(kib) {
	if (kib === 'auto')
		return _('Automatic (recommended)');
	return formatMemoryKib(Number(kib));
}

function setHistoryBudget(value, select, previous) {
	select.disabled = true;
	return fs.exec('/usr/libexec/cake-autorate-rs/graph-history', [
		'globals', 'set-budget', String(value)
	]).then(function(result) {
		if (result.code !== 0)
			throw new Error(result.stderr || _('Unable to save the RAM budget.'));
		window.location = window.location.href.split('#')[0];
	}).catch(function(err) {
		select.disabled = false;
		select.value = String(previous);
		ui.addNotification(null,
			E('p', _('Unable to update graph RAM budget: %s').format(err.message || err)),
			'error');
	});
}

function renderMemoryBudget(instances, globalSection) {
	var status = instances.map(function(instance) { return instance.status; }).filter(Boolean)[0] || {};
	var configured = String(globalSection && globalSection.graph_history_ram_budget_kib || 'auto');
	var safeMax = Number(status.graph_history_safe_max_kib || 1024);
	var select = E('select', {
		'class': 'cbi-input-select cake-graph-budget-select',
		'title': _('Total RAM budget shared by all graph histories')
	}, [ E('option', { 'value': 'auto' }, budgetLabel('auto')) ].concat(
		HISTORY_BUDGETS_KIB.map(function(value) {
			return E('option', {
				'value': String(value),
				'disabled': value > safeMax && String(value) !== configured ? '' : null
			}, budgetLabel(value));
		})
	));

	select.value = configured;
	select.addEventListener('change', function() {
		return setHistoryBudget(select.value, select, configured);
	});

	return E('div', { 'class': 'cake-graph-memory-panel' }, [
		E('label', { 'class': 'cake-graph-budget-label' }, [
			E('span', {}, _('History RAM budget')),
			select
		]),
		E('div', { 'class': 'cake-graph-memory-stats' }, [
			E('span', {}, _('Available: %s').format(formatMemoryKib(status.graph_history_mem_available_kib))),
			E('span', {}, _('Safe maximum: %s').format(formatMemoryKib(safeMax))),
			E('span', {}, _('Effective: %s').format(formatMemoryKib(status.graph_history_effective_total_kib))),
			E('span', {}, _('Used: %s').format(formatMemoryKib(status.graph_history_used_total_kib))),
			E('span', {}, _('Instances: %d').format(Number(status.graph_history_instances || instances.length || 0))),
			status.graph_history_paused_low_memory ?
				E('strong', { 'class': 'cake-graph-memory-paused' }, _('History paused: low available RAM')) : ''
		])
	]);
}

function lineConnected(previous, current, interval) {
	if (!previous)
		return false;
	if (current.timestamp - previous.timestamp > Math.max(5, interval * 3.5))
		return false;
	if (previous.routeIdentity && current.routeIdentity &&
		previous.routeIdentity !== current.routeIdentity)
		return false;
	if (previous.uplinkState && current.uplinkState &&
		previous.uplinkState !== current.uplinkState)
		return false;
	return true;
}

function drawLine(ctx, geometry, valueKey, yFor, color, dashed) {
	var drawing = false;
	var previous = null;

	ctx.beginPath();
	ctx.strokeStyle = color;
	ctx.lineWidth = 1.5;
	ctx.setLineDash(dashed ? [ 6, 4 ] : []);
	for (var i = 0; i < geometry.points.length; i++) {
		var point = geometry.points[i];
		var value = point[valueKey];
		if (value == null || !isFinite(value)) {
			drawing = false;
			previous = point;
			continue;
		}

		if (drawing && lineConnected(previous, point, geometry.interval))
			ctx.lineTo(chartX(geometry, point.timestamp), yFor(value));
		else
			ctx.moveTo(chartX(geometry, point.timestamp), yFor(value));
		drawing = true;
		previous = point;
	}
	ctx.stroke();
	ctx.setLineDash([]);
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
	var height = 244;
	var dpr = width > 4096 ? 1 : Math.min(window.devicePixelRatio || 1, 2);
	var left = 48, right = 48, top = 54, bottom = 34;
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
		includeDate: includeDate,
		interval: interval
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

function gradeColor(grade) {
	if (grade === 'A+' || grade === 'A')
		return '#16a085';
	if (grade === 'B')
		return '#8eae2f';
	if (grade === 'C')
		return '#d08b20';
	return '#d34b4b';
}

function isQualityGrade(grade) {
	return grade === 'A+' || grade === 'A' || grade === 'B' || grade === 'C' ||
		grade === 'D' || grade === 'F';
}

function collectChartEvents(geometry) {
	var events = [];
	var previous = null;
	var previousGrade = '';
	var previousGradeState = '';
	var previousPhase = '';

	geometry.points.forEach(function(point) {
		if (previous) {
			var stateChanged = point.uplinkState && point.uplinkState !== previous.uplinkState;
			var identityChanged = point.routeIdentity && previous.routeIdentity &&
				point.routeIdentity !== previous.routeIdentity;
			if (stateChanged || identityChanged) {
				events.push({
					timestamp: point.timestamp,
					kind: identityChanged ? 'route' : 'uplink',
					label: identityChanged ? _('route change') : point.uplinkState,
					shortLabel: identityChanged ? _('ROUTE') :
						(point.uplinkState === 'LEARNING' ? _('LEARN') : point.uplinkState),
					color: point.uplinkState === 'OFFLINE' ? '#c0392b' :
						(point.uplinkState === 'LEARNING' ? '#d4a017' : '#2471a3'),
					dash: [ 4, 3 ]
				});
			}
		}

		if (isQualityGrade(point.grade)) {
			var gradeChanged = point.grade !== previousGrade ||
				(point.gradeState === 'final' && previousGradeState !== 'final');
			if (gradeChanged) {
				events.push({
					timestamp: point.timestamp,
					kind: 'grade',
					label: _('grade %s').format(point.grade),
					shortLabel: point.grade,
					color: gradeColor(point.grade),
					dash: [ 2, 3 ]
				});
			}
			previousGrade = point.grade;
			previousGradeState = point.gradeState;
		}

		var phase = point.ratingPhase || '';
		if (phase && phase !== previousPhase && phase !== 'IDLE') {
			var dlSamples = point.ratingDlSamples == null ? 0 : point.ratingDlSamples;
			var ulSamples = point.ratingUlSamples == null ? 0 : point.ratingUlSamples;
			var progress = dlSamples || ulSamples ?
				'%s %s/%s'.format(phase, dlSamples, ulSamples) : phase;
			events.push({
				timestamp: point.timestamp,
				kind: 'rating',
				label: progress,
				shortLabel: phase,
				color: phase === 'DL' ? '#2980b9' :
					(phase === 'UL' ? '#e67e22' : '#8e44ad'),
				dash: [ 3, 4 ]
			});
		}
		if (phase)
			previousPhase = phase;
		previous = point;
	});

	return events.sort(function(a, b) { return a.timestamp - b.timestamp; });
}

function clusterChartEvents(geometry, events) {
	var clusters = [];

	events.forEach(function(event) {
		var x = chartX(geometry, event.timestamp);
		var cluster = clusters.length ? clusters[clusters.length - 1] : null;

		/*
		 * Use the previous event, not the first event in the cluster, as the
		 * density boundary.  Otherwise a sequence at x=0,10,20 is split into
		 * two labels even though every adjacent pair belongs to one visual
		 * burst.  The two resulting labels then collide above the chart.
		 */
		if (!cluster || x - cluster.lastX > GRAPH_EVENT_CLUSTER_DISTANCE_PX) {
			clusters.push({
				timestamp: event.timestamp,
				x: x,
				firstX: x,
				lastX: x,
				events: [ event ]
			});
			return;
		}

		cluster.events.push(event);
		cluster.lastX = x;
		cluster.timestamp = cluster.events.reduce(function(sum, candidate) {
			return sum + candidate.timestamp;
		}, 0) / cluster.events.length;
		cluster.x = chartX(geometry, cluster.timestamp);
	});

	return clusters.map(function(cluster) {
		var labels = [];
		var shortLabels = [];

		cluster.events.forEach(function(event) {
			if (labels[labels.length - 1] !== event.label)
				labels.push(event.label);
			if (shortLabels[shortLabels.length - 1] !== (event.shortLabel || event.label))
				shortLabels.push(event.shortLabel || event.label);
		});

		cluster.label = labels.join(' → ');
		cluster.shortLabel = shortLabels.length > 2 ?
			'%s…%s'.format(shortLabels[0], shortLabels[shortLabels.length - 1]) :
			shortLabels.join('→');
		cluster.color = cluster.events.some(function(event) {
			return event.kind === 'uplink' && event.label === 'OFFLINE';
		}) ? '#c0392b' : cluster.events[cluster.events.length - 1].color;
		cluster.dash = cluster.events[cluster.events.length - 1].dash;
		return cluster;
	});
}

function chartEventClusters(geometry) {
	if (!geometry.eventClusters)
		geometry.eventClusters = clusterChartEvents(geometry, collectChartEvents(geometry));

	return geometry.eventClusters;
}

function eventLabelPlacement(ctx, geometry, cluster, label) {
	var textWidth = ctx.measureText(label).width;
	var width = textWidth + 7;
	var leftEdge = geometry.left + 3;
	var rightEdge = geometry.width - geometry.right;
	var align = cluster.x + 3 + width <= rightEdge ? 'left' : 'right';
	var textX = align === 'left' ? cluster.x + 3 : cluster.x - 3;
	if (align === 'left')
		textX = Math.max(leftEdge, Math.min(textX, rightEdge - textWidth - 3));
	else
		textX = Math.min(rightEdge - 3, Math.max(textX, leftEdge + textWidth));
	var start = align === 'left' ? textX : textX - textWidth;

	return {
		label: label,
		width: width,
		align: align,
		textX: textX,
		start: Math.max(geometry.left, start - 3),
		end: Math.min(rightEdge, start + textWidth + 3)
	};
}

function layoutEventLabels(ctx, geometry, clusters) {
	var laneEnds = [];
	for (var laneIndex = 0; laneIndex < GRAPH_EVENT_LABEL_LANES; laneIndex++)
		laneEnds.push(geometry.left - GRAPH_EVENT_LABEL_GAP_PX);

	return clusters.map(function(cluster) {
		var placement = eventLabelPlacement(ctx, geometry, cluster, cluster.label);
		var lane = laneEnds.findIndex(function(end) {
			return placement.start >= end + GRAPH_EVENT_LABEL_GAP_PX;
		});

		if (lane < 0 || placement.width > 144) {
			placement = eventLabelPlacement(ctx, geometry, cluster, cluster.shortLabel);
			lane = laneEnds.findIndex(function(end) {
				return placement.start >= end + GRAPH_EVENT_LABEL_GAP_PX;
			});
		}

		if (lane >= 0)
			laneEnds[lane] = placement.end;

		return {
			cluster: cluster,
			x: cluster.x,
			lane: lane,
			label: lane >= 0 ? placement.label : '',
			textX: placement.textX,
			textAlign: placement.align
		};
	});
}

function drawChartEvents(ctx, geometry, showLabels) {
	var layouts = layoutEventLabels(ctx, geometry, chartEventClusters(geometry));

	layouts.forEach(function(layout) {
		ctx.save();
		ctx.strokeStyle = layout.cluster.color;
		ctx.setLineDash(layout.cluster.dash || [ 3, 3 ]);
		ctx.beginPath();
		ctx.moveTo(layout.x, geometry.top);
		ctx.lineTo(layout.x, geometry.top + geometry.plotHeight);
		ctx.stroke();
		if (showLabels !== false && layout.label) {
			ctx.setLineDash([]);
			ctx.fillStyle = layout.cluster.color;
			ctx.textAlign = layout.textAlign;
			ctx.fillText(layout.label, layout.textX, 14 + layout.lane * 17);
		}
		ctx.restore();
	});
}

function drawLatencyChart(canvas, geometry) {
	var ctx = prepareCanvas(canvas, geometry);
	var rttMax = 10;

	geometry.points.forEach(function(point) {
		if (point.rtt != null && isFinite(point.rtt))
			rttMax = Math.max(rttMax, point.rtt);
		if (point.transport != null && isFinite(point.transport))
			rttMax = Math.max(rttMax, point.transport);
		if (point.effective != null && isFinite(point.effective))
			rttMax = Math.max(rttMax, point.effective);
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

	if (!geometry.points.length) {
		ctx.textAlign = 'center';
		ctx.fillText(_('Waiting for history samples…'), geometry.width / 2, geometry.height / 2);
		return rttMax;
	}

	drawLine(ctx, geometry, 'rtt', rttY, '#22a06b');
	drawLine(ctx, geometry, 'transport', rttY, '#d35400');
	drawLine(ctx, geometry, 'effective', rttY, '#c0398f');
	drawLine(ctx, geometry, 'cpu', cpuY, '#6c5ce7');
	drawChartEvents(ctx, geometry);
	return rttMax;
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

function drawTrafficChart(canvas, geometry, showFloors) {
	var ctx = prepareCanvas(canvas, geometry);
	var rateMax = 100;

	geometry.points.forEach(function(point) {
		if (point.dl != null && isFinite(point.dl))
			rateMax = Math.max(rateMax, point.dl);
		if (point.ul != null && isFinite(point.ul))
			rateMax = Math.max(rateMax, point.ul);
		if (showFloors && point.dlFloor != null && isFinite(point.dlFloor))
			rateMax = Math.max(rateMax, point.dlFloor);
		if (showFloors && point.ulFloor != null && isFinite(point.ulFloor))
			rateMax = Math.max(rateMax, point.ulFloor);
	});
	rateMax = niceRateCeiling(rateMax);
	drawChartGrid(ctx, geometry);

	function rateY(value) {
		return geometry.top + geometry.plotHeight -
			Math.max(0, Math.min(1, value / rateMax)) * geometry.plotHeight;
	}

	if (!geometry.points.length) {
		ctx.textAlign = 'center';
		ctx.fillText(_('Waiting for history samples…'), geometry.width / 2, geometry.height / 2);
		return rateMax;
	}

	drawLine(ctx, geometry, 'dl', rateY, '#2980b9');
	drawLine(ctx, geometry, 'ul', rateY, '#e67e22');
	if (showFloors) {
		drawLine(ctx, geometry, 'dlFloor', rateY, '#74a9cf', true);
		drawLine(ctx, geometry, 'ulFloor', rateY, '#f6b26b', true);
	}
	drawChartEvents(ctx, geometry, false);
	return rateMax;
}

function fixedAxis(labels) {
	var nodes = {
		leftTop: E('span', { 'class': 'cake-graph-axis-left cake-graph-axis-top' }, labels.leftTop || ''),
		leftBottom: E('span', { 'class': 'cake-graph-axis-left cake-graph-axis-bottom' }, labels.leftBottom || '0'),
		rightTop: E('span', { 'class': 'cake-graph-axis-right cake-graph-axis-top' }, labels.rightTop || ''),
		rightBottom: E('span', { 'class': 'cake-graph-axis-right cake-graph-axis-bottom' }, labels.rightBottom || '')
	};
	nodes.root = E('div', { 'class': 'cake-graph-fixed-axis', 'aria-hidden': 'true' }, [
		nodes.leftTop, nodes.leftBottom, nodes.rightTop, nodes.rightBottom
	]);
	return nodes;
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

function nearestEventCluster(geometry, logicalX) {
	var clusters = chartEventClusters(geometry);
	var nearest = null;
	var distance = Infinity;

	clusters.forEach(function(cluster) {
		var candidate = Math.abs(cluster.x - logicalX);
		if (candidate < distance) {
			distance = candidate;
			nearest = cluster;
		}
	});

	return distance <= GRAPH_EVENT_CLUSTER_DISTANCE_PX / 2 ? nearest : null;
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
		var eventCluster = nearestEventCluster(geometry, logicalX);

		hoverInfo.textContent = '%s · %s · route %s · rating phase %s (DL %s / UL %s) · grade %s (%s) · RTT %s · transport Δ %s · effective Δ %s · CPU %s · DL %s · UL %s · floors %s/%s'.format(
			new Date(point.timestamp * 1000).toLocaleString(),
			point.uplinkState || '-',
			point.routeIdentity || '-',
			point.ratingPhase || 'IDLE',
			point.ratingDlSamples == null ? '-' : point.ratingDlSamples,
			point.ratingUlSamples == null ? '-' : point.ratingUlSamples,
			point.grade || '-',
			formatMetric(point.gradeIncrease, ' ms', 1),
			formatMetric(point.rtt, ' ms', 3),
			formatMetric(point.transport, ' ms', 3),
			formatMetric(point.effective, ' ms', 3),
			formatMetric(point.cpu, '%', 1),
			formatTrafficRate(point.dl),
			formatTrafficRate(point.ul),
			formatTrafficRate(point.dlFloor),
			formatTrafficRate(point.ulFloor));
		if (eventCluster)
			hoverInfo.textContent += ' · %s: %s'.format(_('events'), eventCluster.label);
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

function scrollMaximum(viewport) {
	/* offsetWidth includes the stable scrollbar gutter which limits the actual
	 * scrollLeft range in Chromium. clientWidth alone leaves follow mode about
	 * one gutter width short of the newest sample. */
	return Math.max(0, viewport.scrollWidth -
		Math.max(viewport.clientWidth || 0, viewport.offsetWidth || 0));
}

function bindScroll(viewport, latestButton, section) {
	var state = scrollState(section);

	function updateButton() {
		latestButton.disabled = state.followLatest;
	}

	viewport.addEventListener('scroll', function() {
		var maxScroll = scrollMaximum(viewport);
		state.left = viewport.scrollLeft;
		state.followLatest = maxScroll - viewport.scrollLeft <= 8;
		updateButton();
	});

	latestButton.addEventListener('click', function() {
		state.followLatest = true;
		viewport.scrollLeft = scrollMaximum(viewport);
		state.left = viewport.scrollLeft;
		updateButton();
	});

	window.requestAnimationFrame(function() {
		var maxScroll = scrollMaximum(viewport);
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

function changeHistoryPage(sectionName, offset) {
	graphPageOffsets[sectionName] = Math.max(0, Math.round(offset));
	graphScrollStates[sectionName] = { followLatest: true, left: 0 };
	return graphRefreshNow ? graphRefreshNow() : Promise.resolve();
}

function renderCard(instance) {
	var section = instance.section;
	var status = instance.status || {};
	var sectionName = section['.name'];
	var enabled = historyEnabled(section);
	var interval = historyInterval(section);
	var totalSamples = Number(instance.historyTotal || (instance.history || []).length);
	var pageOffset = Number(instance.historyOffset || 0);
	var button = E('button', {
		'class': enabled ? 'btn cbi-button cbi-button-negative' : 'btn cbi-button cbi-button-action'
	}, enabled ? _('Disable history') : _('Enable history'));
	var body;

	button.addEventListener('click', function() {
		return setHistoryEnabled(sectionName, !enabled, button);
	});

	if (enabled) {
		var showFloors = !!graphFloorStates[sectionName];
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
		var latencyAxis = fixedAxis({ leftBottom: '0', rightTop: 'CPU 100%', rightBottom: '0%' });
		var trafficAxis = fixedAxis({ leftBottom: '0' });
		latencyAxis.root.className += ' cake-graph-latency-axis';
		trafficAxis.root.className += ' cake-graph-traffic-axis';
		var track = E('div', { 'class': 'cake-graph-track' }, [
			E('div', { 'class': 'cake-graph-chart-title' }, _('Latency and CPU')),
			latencyCanvas,
			E('div', { 'class': 'cake-graph-chart-title' }, _('Download and upload traffic')),
			trafficCanvas
		]);
		var viewport = E('div', { 'class': 'cake-graph-scroll' }, track);
		var chartFrame = E('div', { 'class': 'cake-graph-frame' }, [
			viewport,
			latencyAxis.root,
			trafficAxis.root
		]);
		var floorToggle = E('input', { 'type': 'checkbox' });
		floorToggle.checked = showFloors;
		var hoverInfo = E('div', {
			'class': 'cake-graph-hover',
			'style': 'visibility:hidden'
		}, _('Move the pointer over a graph to inspect a sample.'));
		var latestButton = E('button', {
			'class': 'btn cbi-button cbi-button-neutral cake-graph-latest'
		}, _('Latest'));
		var olderButton = E('button', {
			'class': 'btn cbi-button cbi-button-neutral cake-graph-page',
			'disabled': totalSamples > pageOffset + (instance.history || []).length ? null : ''
		}, _('Older'));
		var newerButton = E('button', {
			'class': 'btn cbi-button cbi-button-neutral cake-graph-page',
			'disabled': pageOffset > 0 ? null : ''
		}, _('Newer'));
		olderButton.addEventListener('click', function() {
			return changeHistoryPage(sectionName, pageOffset + (instance.history || []).length);
		});
		newerButton.addEventListener('click', function() {
			return changeHistoryPage(sectionName, Math.max(0, pageOffset - HISTORY_PAGE_SAMPLES));
		});
		var usedBytes = Math.max(1, Number(status.graph_history_used_instance_kib || 0) * 1024);
		var bytesPerSample = totalSamples > 0 ? Math.max(1, usedBytes / totalSamples) : 120;
		var estimatedHours = Number(status.graph_history_instance_budget_kib || 0) * 1024 /
			bytesPerSample * interval / 3600;

		body = E('div', { 'class': 'cake-graph-body' }, [
			E('div', { 'class': 'cake-graph-legend' }, [
				E('span', { 'class': 'cake-graph-rtt' },
					_('RTT: %s').format(formatMetric(status.rtt_ms, ' ms', 2))),
				E('span', { 'class': 'cake-graph-transport' },
					_('Transport Δ: %s').format(formatMetric(status.transport_delta_ms, ' ms', 2))),
				E('span', { 'class': 'cake-graph-effective' },
					_('Effective Δ: %s').format(formatMetric(status.effective_latency_delta_ms, ' ms', 2))),
				E('span', { 'class': 'cake-graph-cpu' },
					_('CPU: %s').format(formatMetric(status.cpu_total_percent, '%', 1))),
				E('span', { 'class': 'cake-graph-dl' },
					_('DL: %s').format(formatTrafficRate(status.dl_achieved_rate_kbps))),
				E('span', { 'class': 'cake-graph-ul' },
					_('UL: %s').format(formatTrafficRate(status.ul_achieved_rate_kbps))),
				E('label', { 'class': 'cake-graph-floors' }, [
					floorToggle,
					E('span', {}, _('Show safety floors'))
				]),
				E('span', { 'class': 'cake-graph-samples' },
					_('Showing: %d / %d · offset %d').format(
						(instance.history || []).length, totalSamples, pageOffset)),
				E('span', { 'class': 'cake-graph-samples' },
					_('RAM: %s / %s · about %s h').format(
						formatMemoryKib(status.graph_history_used_instance_kib),
						formatMemoryKib(status.graph_history_instance_budget_kib),
						isFinite(estimatedHours) ? estimatedHours.toFixed(1) : '-')),
				olderButton,
				newerButton,
				latestButton
			]),
			hoverInfo,
			chartFrame
		]);
		window.requestAnimationFrame(function() {
			var geometry = buildChartGeometry(instance.history || [], interval, viewport);
			track.style.width = geometry.width + 'px';
			latencyAxis.leftTop.textContent = _('RTT %d ms').format(drawLatencyChart(latencyCanvas, geometry));
			function redrawTraffic() {
				trafficAxis.leftTop.textContent = _('Traffic %s').format(
					formatTrafficRate(drawTrafficChart(trafficCanvas, geometry, showFloors)));
			}
			redrawTraffic();
			floorToggle.addEventListener('change', function() {
				showFloors = floorToggle.checked;
				graphFloorStates[sectionName] = showFloors;
				redrawTraffic();
			});
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
				E('small', {}, _('Uplink: %s · Controller: %s').format(
					String(status.uplink_state || '-').toUpperCase(),
					String(status.state || '-').toUpperCase()))
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
			var globalSection = uci.sections('cake-autorate', 'globals')[0] || {};
			return loadInstances(sections).then(function(instances) {
				return [ sections, instances, globalSection ];
			});
		});
	},

	render: function(data) {
		cakeUi.ensureAppHeader();
		var sections = data[0];
		var globalSection = data[2] || {};
		var content = renderInstances(data[1]);
		var memoryPanel = renderMemoryBudget(data[1], globalSection);
		var root = E('div', {}, [
			E('style', {}, [
				'.cake-graphs-warning{margin-bottom:18px}',
				'.cake-graphs-grid{display:grid;grid-template-columns:minmax(0,1fr);gap:16px}',
				'.cake-graph-memory-panel{display:flex;align-items:flex-end;justify-content:space-between;gap:16px;flex-wrap:wrap;margin:0 0 18px;padding:12px 14px;border:1px solid rgba(127,127,127,.3);border-radius:6px;background:rgba(127,127,127,.05)}',
				'.cake-graph-budget-label{display:flex;flex-direction:column;gap:4px;font-weight:600}.cake-graph-budget-select{min-width:220px}',
				'.cake-graph-memory-stats{display:flex;gap:14px;flex-wrap:wrap;color:#777}.cake-graph-memory-paused{color:#d34b4b}',
				'.cake-graph-card{min-width:0;border:1px solid rgba(127,127,127,.3);border-radius:6px;padding:14px;background:rgba(127,127,127,.04)}',
				'.cake-graph-header{display:flex;justify-content:space-between;align-items:center;gap:12px;margin-bottom:12px}',
				'.cake-graph-header h3{margin:0 0 3px}',
				'.cake-graph-actions{display:flex;align-items:flex-end;gap:10px;flex-wrap:wrap;justify-content:flex-end}',
				'.cake-graph-interval-label{display:flex;flex-direction:column;gap:3px;font-size:12px}',
				'.cake-graph-interval{min-width:120px}',
				'.cake-graph-legend{display:flex;align-items:center;gap:18px;margin-bottom:5px;font-weight:600;flex-wrap:wrap}',
				'.cake-graph-rtt{color:#22a06b}.cake-graph-transport{color:#d35400}.cake-graph-effective{color:#c0398f}.cake-graph-cpu{color:#6c5ce7}.cake-graph-dl{color:#2980b9}.cake-graph-ul{color:#e67e22}.cake-graph-samples{color:#777;font-weight:400}',
				'.cake-graph-floors{display:inline-flex;align-items:center;gap:5px;color:#777;font-weight:400;cursor:pointer}',
				'.cake-graph-latest{margin-left:auto}',
				'.cake-graph-hover{min-height:1.5em;margin:3px 0 6px;padding:4px 7px;border-radius:4px;background:rgba(127,127,127,.12);font-variant-numeric:tabular-nums}',
				'.cake-graph-frame{position:relative;min-width:0}',
				'.cake-graph-scroll{display:block;width:100%;overflow-x:auto;overflow-y:hidden;scrollbar-gutter:stable}',
				'.cake-graph-track{display:block;max-width:none}',
				'.cake-graph-chart-title{position:sticky;left:0;z-index:1;display:inline-block;margin:5px 0 1px;padding:2px 6px;border-radius:3px;background:var(--background-color-high,#fff);font-weight:600}',
				'.cake-graph-fixed-axis{position:absolute;left:0;right:0;z-index:2;height:0;pointer-events:none;font-size:12px;color:#777}',
				'.cake-graph-latency-axis{top:0}.cake-graph-traffic-axis{top:271px}',
				'.cake-graph-fixed-axis span{position:absolute;padding:1px 3px;border-radius:2px;background:var(--background-color-high,rgba(255,255,255,.82));white-space:nowrap}',
				'.cake-graph-axis-left{left:1px}.cake-graph-axis-right{right:1px;text-align:right}',
				'.cake-graph-axis-top{top:51px}.cake-graph-axis-bottom{top:205px}',
				'.cake-graph-canvas{display:block;max-width:none;height:244px}',
				'.cake-graph-disabled{min-height:80px;display:flex;align-items:center;color:#777}',
				'@media(max-width:600px){.cake-graphs-grid{grid-template-columns:minmax(0,1fr)}.cake-graph-card{padding:10px}.cake-graph-header{align-items:flex-start;flex-direction:column}.cake-graph-actions{width:100%;justify-content:space-between}.cake-graph-latest{margin-left:0}.cake-graph-memory-panel{align-items:stretch;flex-direction:column}.cake-graph-budget-select{width:100%}.cake-graph-legend{gap:10px}.cake-graph-fixed-axis{font-size:11px}}'
			].join('')),
			E('div', { 'class': 'alert-message warning cake-graphs-warning' }, [
				E('strong', {}, _('Optional RAM history. ')),
				_('Enabling graphs stores RTT, transport/effective latency, CPU, download/upload, and safety-floor samples only in /var/run (RAM), never in flash. The selected total budget is shared by all enabled instances and is reduced automatically under memory pressure. Large histories are fetched in bounded pages so the browser never loads the entire RAM buffer. Oldest samples are discarded automatically. Both charts share one horizontal timeline and follow the newest data until you scroll back. Hover over either chart for exact values. Data is cleared when the service stops or the router reboots.')
			]),
			memoryPanel,
			content
		]);

		graphRefreshNow = function() {
			return loadInstances(sections).then(function(instances) {
				var nextContent = renderInstances(instances);
				var nextMemoryPanel = renderMemoryBudget(instances, globalSection);
				if (content.parentNode) {
					content.parentNode.replaceChild(nextContent, content);
					content = nextContent;
				}
				if (memoryPanel.parentNode) {
					memoryPanel.parentNode.replaceChild(nextMemoryPanel, memoryPanel);
					memoryPanel = nextMemoryPanel;
				}
			});
		};
		poll.add(graphRefreshNow, 5);

		return root;
	},

	handleSaveApply: null,
	handleSave: null,
	handleReset: null
});
