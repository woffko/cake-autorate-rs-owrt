'use strict';
'require rpc';
'require ui';

var callStatus = rpc.declare({ object: 'cake-autorate-config', method: 'mq_status', params: [ 'script' ], reject: true });
var callProbe = rpc.declare({ object: 'cake-autorate-config', method: 'mq_probe', params: [ 'script' ], reject: true });
var probing = false;
function flag(value) {
 if (value == null) return null;
 var text = String(value).trim().toLowerCase();
 if ([ '1', 'true', 'yes', 'on' ].indexOf(text) >= 0) return true;
 if ([ '0', 'false', 'no', 'off' ].indexOf(text) >= 0) return false;
 return undefined;
}
function mode(data, id) {
 var raw = data.get('cake-autorate', id, 'sqm_qdisc') || 'cake';
 if ([ 'cake', 'cake_mq', 'cake-mq' ].indexOf(raw) < 0) return '__unsupported__';
 var explicit = flag(data.get('cake-autorate', id, 'sqm_use_mq'));
 if (explicit === undefined) return '__unsupported__';
 if (explicit != null) return explicit ? 'cake_mq' : 'cake';
 if (raw !== 'cake') return 'cake_mq';
 var target = data.get('cake-autorate', id, 'sqm_section') || 'cake_' + id;
 var retained = flag(data.get('sqm', target, 'use_mq'));
 return retained === undefined ? '__unsupported__' : retained ? 'cake_mq' : 'cake';
}
function choices(value, supported) {
 var values = { cake: _('CAKE (single queue)') };
 if (supported || value === 'cake_mq')
  values.cake_mq = supported ? _('CAKE multi-queue (verified)') : _('Saved multi-queue mode — verify support before enabling');
 if (value === '__unsupported__') values.__unsupported__ = _('Unsupported saved mode — choose a supported CAKE mode');
 return values;
}
async function check(script, active) {
 if (typeof script !== 'string' || script.length > 128 || script.indexOf('..') >= 0 ||
     !/^[A-Za-z0-9_-][A-Za-z0-9_.-]*\.qos$/.test(script))
  throw new Error('mq-script-invalid');
 var reply = await (active ? callProbe(script) : callStatus(script));
 if (!reply || reply.schema_version !== 1 || reply.ok !== true || !reply.result ||
     reply.result.script !== script || typeof reply.result.supported !== 'boolean')
  throw new Error('mq-receipt-invalid');
 return reply.result.supported;
}
function bind(qdisc, scriptOption, button) {
 var states = Object.create(null);
 function script(id) {
  var value = scriptOption.formvalue(id);
  return value != null ? value : qdisc.map.data.get('cake-autorate', id, 'sqm_script') || 'piece_of_cake.qos';
 }
 function update(state, supported) {
  var select = state.widget.node.querySelector('select');
  var value = state.widget.getValue();
  var values = choices(value, supported);
  state.widget.choices = values;
  while (select.firstChild) select.removeChild(select.firstChild);
  Object.keys(values).forEach(function(key) {
   select.appendChild(E('option', {
    value: key, selected: key === value ? '' : null,
    disabled: (key === '__unsupported__' || (key === 'cake_mq' && !supported)) ? '' : null
   }, [ document.createTextNode(values[key]) ]));
  });
  state.widget.setValue(value);
 }
 async function refresh(id, active, forcedScript) {
  var state = states[id];
  if (!state) return null;
  var selected = forcedScript != null ? forcedScript : script(id);
  var generation = ++state.generation;
  update(state, false);
  var supported = await check(selected, active);
  if (states[id] !== state || generation !== state.generation || script(id) !== selected)
   return null;
  update(state, supported);
  return supported;
 }
 qdisc.default = 'cake';
 qdisc.rmempty = false;
 qdisc.forcewrite = true;
 qdisc.description = _('Only CAKE modes are supported. Legacy cake-mq/cake_mq settings are preserved as multi-queue intent and saved as cake plus use_mq. Verify support to offer a new multi-queue selection. The check uses one temporary down interface, sends no traffic, and does not change existing queues.');
 qdisc.cfgvalue = function(id) { return mode(this.map.data, id); };
 qdisc.validate = function(id, value) {
  return value === 'cake' || value === 'cake_mq' ? true : _('Choose CAKE or a verified multi-queue mode.');
 };
 qdisc.write = function(id, value) {
  if (value !== 'cake' && value !== 'cake_mq') throw new Error(_('Unsupported CAKE mode.'));
  this.map.data.set('cake-autorate', id, 'sqm_qdisc', 'cake');
  this.map.data.set('cake-autorate', id, 'sqm_use_mq', value === 'cake_mq' ? '1' : '0');
 };
 qdisc.renderWidget = async function(id, index, cfgvalue) {
  var selected = script(id);
  var supported = await check(selected, false).catch(function() { return false; });
  // A changed script never inherits the previous script's proof.
  if (script(id) !== selected) supported = false;
  var value = cfgvalue != null ? cfgvalue : this.cfgvalue(id);
  var widget = new ui.Select(value, choices(value, supported), {
   id: this.cbid(id), sort: false, widget: 'select', optional: false,
   validate: this.getValidator(id),
   disabled: this.readonly != null ? this.readonly : this.map.readonly
  });
  var node = widget.render();
  var state = { widget: widget, generation: 0 };
  states[id] = state;
  update(state, supported);
  return node;
 };
 var changed = scriptOption.onchange;
 scriptOption.onchange = function(ev, id, value) {
  if (typeof changed === 'function') changed.call(this, ev, id, value);
  return refresh(id, false, value).catch(function() { return null; });
 };
 button.inputtitle = _('Verify multi-queue support');
 button.inputstyle = 'action';
 button.rmempty = true;
 button.write = button.remove = function() {};
 button.description = _('Checks the selected SQM script, kernel and tc without generating test traffic. It does not enable multi-queue or save settings.');
 button.onclick = async function(ev, id) {
  if (probing) return;
  probing = true;
  var control = ev.currentTarget;
  control.disabled = true;
  try {
   var supported = await refresh(id, true);
   if (supported === null || !states[id] || !states[id].widget.node.isConnected) return;
   ui.addNotification(null, E('p', {}, document.createTextNode(supported
    ? _('Multi-queue support verified. You can select it now; settings have not been saved.')
    : _('Multi-queue support was not confirmed. Existing queues and settings were not changed.'))), supported ? 'info' : 'warning');
  } catch (cause) {
   if (states[id] && states[id].widget.node.isConnected)
    ui.addNotification(null, E('p', {}, document.createTextNode(
     _('Multi-queue verification failed. It remains unverified; retry the check to recover any unfinished temporary-interface cleanup.'))), 'error');
  } finally {
   probing = false;
   control.disabled = false;
  }
 };
 return qdisc;
}
return L.Class.extend({ bind: bind });
