'use strict';

const fs = require('fs');
const path = require('path');

if (!String.prototype.format) {
	String.prototype.format = function(...args) {
		let index = 0;
		return this.replace(/%[sd]/g, token => {
			const value = args[index++];
			return token === '%d' ? Number(value).toString() : String(value);
		});
	};
}

const sourcePath = path.join(__dirname, '..', 'htdocs', 'luci-static', 'resources',
	'view', 'cake-autorate-rs', 'graphs.js');
const source = fs.readFileSync(sourcePath, 'utf8');
const marker = 'return L.view.extend';
const prefix = source.slice(0, source.indexOf(marker));
const windowStub = {
	devicePixelRatio: 1,
	requestAnimationFrame: callback => callback(),
};
const loadHelpers = new Function('fs', 'poll', 'uci', 'ui', 'L', 'E', '_', 'window',
	`${prefix}\nreturn { parseHistory, historyInterval, buildChartGeometry, nearestPoint, ` +
		'bindHover, bindScroll, scrollState, scrollMaximum, formatMemoryKib, lineConnected, niceRateCeiling };');
const helpers = loadHelpers({}, {}, {}, {}, {}, () => {}, value => value, windowStub);

function assert(condition, message) {
	if (!condition)
		throw new Error(message);
}

assert(source.includes('.cake-graph-fixed-axis{position:absolute;left:0;right:0'),
	'Y-axis labels must stay fixed while the data timeline scrolls');
assert(source.includes('.cake-graph-chart-title{position:sticky;left:0'),
	'chart titles must stay fixed while the data timeline scrolls');
assert(source.includes('viewport.offsetWidth'),
	'follow-latest must account for the stable scrollbar gutter');
assert(source.includes('.cake-graphs-grid{display:grid;grid-template-columns:minmax(0,1fr)'),
	'different WAN cards must be stacked in one column');
assert(source.includes('HISTORY_PAGE_SAMPLES = 10000'),
	'large histories must be fetched in bounded pages');
assert(source.includes('graph_history_ram_budget_kib'),
	'Graphs must expose the global RAM budget');
assert(source.includes('Show safety floors') && source.includes('if (showFloors && point.dlFloor'),
	'safety floors must be optional and excluded from the default traffic scale');
assert(helpers.formatMemoryKib(1024) === '1.0 MiB', 'memory formatter failed');
assert(helpers.formatMemoryKib(null) === '-', 'missing memory must not look like zero use');

const now = Math.floor(Date.now() / 1000);
const points = helpers.parseHistory([
	`${now - 2},10.125,1.5,1000.0,500.0,20.5,22.0,600.0,300.0,ACTIVE,mwan3|wan|pppoe-wan|198.51.100.1|0x100|1,A+,final,1.25,DL,20,7`,
	`${now - 1},,2.5,2000.0,750.0`,
	`${now},12.500,3.5,3000.0,1000.0`,
].join('\n'));
assert(points.length === 3, 'five-column history parsing failed');
assert(points[1].rtt === null && points[1].dl === 2000, 'nullable RTT/rate parsing failed');
assert(points[0].transport === 20.5 && points[0].effective === 22.0,
	'nine-column transport latency parsing failed');
assert(points[0].dlFloor === 600 && points[0].ulFloor === 300,
	'nine-column throughput floor parsing failed');
assert(points[0].uplinkState === 'ACTIVE' && points[0].routeIdentity.includes('0x100'),
	'Multi-WAN state and route identity parsing failed');
assert(points[0].grade === 'A+' && points[0].gradeState === 'final' && points[0].gradeIncrease === 1.25,
	'quality-grade event parsing failed');
assert(points[0].ratingPhase === 'DL' && points[0].ratingDlSamples === 20 && points[0].ratingUlSamples === 7,
	'rating progress event parsing failed');

const legacy = helpers.parseHistory(`${now},9.5,4.0`);
assert(legacy.length === 1 && legacy[0].dl === null && legacy[0].ul === null,
	'legacy three-column compatibility failed');
assert(helpers.historyInterval({ graph_history_interval_s: '1' }) === 1, '1 s interval failed');
assert(helpers.historyInterval({ graph_history_interval_s: '60' }) === 60, '60 s interval failed');
assert(helpers.historyInterval({ graph_history_interval_s: '61' }) === 10,
	'out-of-range interval fallback failed');

const longHistory = [];
for (let index = 0; index < 2000; index++) {
	longHistory.push({
		timestamp: now - 1999 + index,
		rtt: 10 + index / 1000,
		cpu: index % 100,
		dl: index * 10,
		ul: index * 5,
	});
}
const geometry = helpers.buildChartGeometry(longHistory, 1, { clientWidth: 900 });
assert(geometry.width > 900, 'long history should require horizontal scrolling');
assert(helpers.nearestPoint(points, now - 1.1) === points[1], 'nearest hover sample failed');
assert(helpers.lineConnected(
	{ timestamp: 1, routeIdentity: 'route-a', uplinkState: 'ACTIVE' },
	{ timestamp: 2, routeIdentity: 'route-a', uplinkState: 'ACTIVE' }, 1),
	'adjacent samples on one route should connect');
assert(!helpers.lineConnected(
	{ timestamp: 1, routeIdentity: 'route-a', uplinkState: 'ACTIVE' },
	{ timestamp: 2, routeIdentity: 'route-b', uplinkState: 'ACTIVE' }, 1),
	'lines must break on route changes');
assert(!helpers.lineConnected(
	{ timestamp: 1, routeIdentity: 'route-a', uplinkState: 'ACTIVE' },
	{ timestamp: 20, routeIdentity: 'route-a', uplinkState: 'ACTIVE' }, 1),
	'lines must break across missing sample gaps');

function eventTarget(properties) {
	const listeners = {};
	return Object.assign({
		addEventListener(type, callback) {
			listeners[type] = callback;
		},
		emit(type, event = {}) {
			listeners[type](event);
		},
	}, properties);
}

const hoverInfo = { style: {}, textContent: '' };
const canvas = eventTarget({
	getBoundingClientRect: () => ({ left: 0, width: geometry.width }),
});
helpers.bindHover(canvas, geometry, hoverInfo);
canvas.emit('mousemove', { clientX: geometry.width - geometry.right });
for (const label of ['rating phase', 'grade', 'RTT', 'transport', 'effective', 'CPU', 'DL', 'UL', 'floors'])
	assert(hoverInfo.textContent.includes(label), `hover output is missing ${label}`);
assert(hoverInfo.style.visibility === 'visible', 'hover output should be visible');
canvas.emit('mouseleave');
assert(hoverInfo.style.visibility === 'hidden', 'hover output should hide on leave');

const latest = eventTarget({ disabled: true });
const viewport = eventTarget({ clientWidth: 900, scrollWidth: geometry.width, scrollLeft: 0 });
helpers.bindScroll(viewport, latest, 'test-instance');
assert(viewport.scrollLeft === geometry.width - 900, 'initial latest-follow failed');
viewport.scrollLeft = 0;
viewport.emit('scroll');
assert(latest.disabled === false, 'Latest should enable after manual scroll-back');

const replacementLatest = eventTarget({ disabled: true });
const replacementViewport = eventTarget({
	clientWidth: 900,
	scrollWidth: geometry.width + 100,
	scrollLeft: 0,
});
helpers.bindScroll(replacementViewport, replacementLatest, 'test-instance');
assert(replacementViewport.scrollLeft === 0, 'poll replacement should preserve manual scroll position');
replacementLatest.emit('click');
assert(replacementViewport.scrollLeft === replacementViewport.scrollWidth - 900,
	'Latest should restore follow mode');

const gutterViewport = {
	clientWidth: 900,
	offsetWidth: 915,
	scrollWidth: geometry.width,
};
assert(helpers.scrollMaximum(gutterViewport) === geometry.width - 915,
	'stable scrollbar gutter must not leave Latest short of the actual edge');

console.log('graphs.js tests passed');
