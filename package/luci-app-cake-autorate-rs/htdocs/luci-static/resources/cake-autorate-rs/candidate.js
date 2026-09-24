'use strict';
'require rpc';
'require uci';
'require ui';
'require cake-autorate-rs.sha256 as cakeHash';

// The validator is an admission check, not permission to restart an operation
// owner. The remaining SQM/lifecycle gates retain their own authority.
var SCOPE = 'controller-sqm';
var MAX_BYTES = 256 * 1024;
var calls = {
 changes: rpc.declare({ object: 'uci', method: 'changes', params: [], reject: true }),
 schema: rpc.declare({ object: 'cake-autorate-config', method: 'schema', params: [], reject: true }),
 begin: rpc.declare({ object: 'cake-autorate-config', method: 'begin', params: [ 'request_id', 'length', 'sha256' ], reject: true }),
 append: rpc.declare({ object: 'cake-autorate-config', method: 'append', params: [ 'request_id', 'token', 'offset', 'data' ], reject: true }),
 finish: rpc.declare({ object: 'cake-autorate-config', method: 'finish', params: [ 'request_id', 'token' ], reject: true }),
 cancel: rpc.declare({ object: 'cake-autorate-config', method: 'cancel', params: [ 'request_id', 'token', 'sha256' ], reject: true })
};
var busy = false;
var rateRules = null;
var maps = new WeakMap();
var OWN_ERROR = Symbol('candidate-error');
var CACHE_FIELDS = [ 'values', 'creates', 'changes', 'deletes', 'reorder' ];

function error(message) { var value = new Error(message); value[OWN_ERROR] = true; return value; }
function clone(value) { return JSON.parse(JSON.stringify(value)); }
function hex(bytes) {
 var out = '';
 for (var i = 0; i < bytes.length; i++) out += bytes[i].toString(16).padStart(2, '0');
 return out;
}
function requestId() {
 var bytes = new Uint8Array(16);
 if (!globalThis.crypto || typeof globalThis.crypto.getRandomValues !== 'function')
  throw error(_('Secure random IDs are unavailable; configuration was not saved.'));
 globalThis.crypto.getRandomValues(bytes);
 return hex(bytes);
}
function rawCandidate(data) {
 var sections = (data.sections('cake-autorate', 'cake_autorate') || []).map(function(section) {
  var options = Object.create(null);
  Object.keys(section).sort().forEach(function(key) {
   if (key.charAt(0) !== '.' && section[key] !== undefined) options[key] = clone(section[key]);
  });
  return { name: section['.create'] || section['.name'], options: options };
 }).sort(function(a, b) { return String(a.name).localeCompare(String(b.name)); });
 var globalOptions = data.get('cake-autorate', 'globals') || {};
 var globals = Object.create(null);
 Object.keys(globalOptions).sort().forEach(function(key) {
  if (key.charAt(0) !== '.' && globalOptions[key] !== undefined) globals[key] = clone(globalOptions[key]);
 });
 var sqm = (data.sections('sqm', 'queue') || []).map(function(section) {
  var options = Object.create(null);
  Object.keys(section).sort().forEach(function(key) {
   if (key.charAt(0) !== '.' && section[key] !== undefined) options[key] = clone(section[key]);
  });
  return { name: section['.create'] || section['.name'], options: options };
 }).sort(function(a, b) { return String(a.name).localeCompare(String(b.name)); });
 return { sections: sections, globals: globals, sqm: sqm };
}
function selectedOptions(raw, fields) {
 var options = Object.create(null);
 fields.slice().sort().forEach(function(key) {
  if (!Object.prototype.hasOwnProperty.call(raw, key)) return;
  var value = raw[key];
  if (typeof value !== 'string' && !(Array.isArray(value) && value.every(function(item) { return typeof item === 'string'; })))
   throw error(_('A candidate field has an invalid UCI value type: ') + key);
  options[key] = value;
 });
 return options;
}
function schemaResult(reply) {
 var schema = reply && reply.result;
 if (!reply || reply.schema_version !== 1 || reply.ok !== true || !schema ||
     schema.protocol !== 'chunked-candidate-v1' || schema.validation_scope !== SCOPE ||
     schema.max_input_bytes !== MAX_BYTES || schema.chunk_bytes !== 1024 ||
     !Array.isArray(schema.fields) || schema.fields.length > 256 ||
     !Array.isArray(schema.global_fields) || schema.global_fields.length > 32 ||
     new Set(schema.fields).size !== schema.fields.length ||
     ![ 'enabled', 'min_dl_shaper_rate_kbps', 'base_dl_shaper_rate_kbps', 'max_dl_shaper_rate_kbps',
        'min_ul_shaper_rate_kbps', 'base_ul_shaper_rate_kbps', 'max_ul_shaper_rate_kbps',
        'bufferbloat_detection_window', 'alpha_delta_ewma' ].every(function(key) { return schema.fields.indexOf(key) >= 0; }) ||
     schema.global_fields.indexOf('graph_history_ram_budget_kib') < 0 ||
     !schema.fields.concat(schema.global_fields).every(function(key) { return typeof key === 'string' && /^[A-Za-z0-9_]{1,64}$/.test(key); }))
  throw error(_('Native candidate validation is unavailable or incompatible; configuration was not saved.'));
 var rates = schema.rate_rules;
 if (!rates || !Number.isFinite(rates.min) || rates.min <= 0 || !Number.isFinite(rates.max) || rates.max < rates.min ||
     rates.disabled_zero_tuple !== true || !rates.defaults || ![ 'dl', 'ul' ].every(function(direction) {
      var tuple = rates.defaults[direction];
      return Array.isArray(tuple) && tuple.length === 3 && tuple.every(function(value) {
       return Number.isFinite(value) && value >= rates.min && value <= rates.max;
      }) && tuple[0] <= tuple[1] && tuple[1] <= tuple[2];
     }))
  throw error(_('Native rate validation is unavailable or incompatible; configuration was not saved.'));
 rateRules = clone(rates);
 return schema;
}
function parseRate(value) {
 if (typeof value === 'number') return Number.isFinite(value) && value >= 0 ? value : null;
 if (typeof value !== 'string' || !/^[+-]?(?:[0-9]+(?:\.[0-9]*)?|\.[0-9]+)(?:[eE][+-]?[0-9]+)?$/.test(value)) return null;
 var number = Number(value);
 return Number.isFinite(number) && number >= 0 ? number : null;
}
function validateRateTuple(values, direction, enabled) {
 if ([ 'dl', 'ul' ].indexOf(direction) < 0 || !Array.isArray(values) || values.length !== 3)
  return _('Invalid rate tuple.');
 if (values.some(function(value) { return value != null && parseRate(value) == null; }))
  return _('Minimum, base and maximum rates must be finite non-negative numbers.');
 var numbers = values.map(function(value, index) {
  return value == null ? rateRules && rateRules.defaults[direction][index] : parseRate(value);
 });
 if (numbers.some(function(value) { return value == null; })) {
  // Missing defaults can only be resolved by the native schema. Saving still
  // fails closed if it is unavailable; never invent local fallback limits.
  if (!rateRules && values.some(function(value) { return value == null || value === ''; })) return true;
  return _('Minimum, base and maximum rates must be finite non-negative numbers.');
 }
 if (!enabled && numbers.every(function(value) { return value === 0; })) return true;
 if (rateRules && numbers.some(function(value) { return value < rateRules.min || value > rateRules.max; }))
  return _('Each rate must be between ') + rateRules.min + _(' and ') + rateRules.max + _(' kbit/s; only a disabled direction may use an all-zero tuple.');
 if (numbers[0] > numbers[1] || numbers[1] > numbers[2])
  return _('Rates must satisfy minimum ≤ base ≤ maximum.');
 return true;
}
function directionEnabled(direction, get) {
 function flag(name, fallback) {
  var value = get(name);
  if (value == null) return fallback;
  return [ '1', 'true', 'yes', 'on' ].indexOf(String(value).toLowerCase()) >= 0;
 }
 var mode = get('sqm_direction_mode');
 if (mode == null) mode = 'both';
 return flag('sqm_enabled', flag('enabled', false)) && flag('adjust_' + direction + '_shaper_rate', true) &&
  (mode === 'both' || (direction === 'dl' && mode === 'download_only') || (direction === 'ul' && mode === 'upload_only'));
}
function result(reply, id) {
 if (!reply || reply.schema_version !== 1 || reply.request_id !== id || reply.ok !== true ||
     !reply.result || typeof reply.result !== 'object')
  throw error(_('Native candidate transfer failed; configuration was not saved.'));
 return reply.result;
}
function cacheSnapshot(data, packages) {
 if (!data.state || CACHE_FIELDS.some(function(field) { return !data.state[field]; }))
  throw error(_('Unsupported LuCI cache layout; configuration was not saved.'));
 var snapshot = {};
 CACHE_FIELDS.forEach(function(field) {
  snapshot[field] = {};
  packages.forEach(function(pkg) {
   snapshot[field][pkg] = Object.prototype.hasOwnProperty.call(data.state[field], pkg)
    ? { present: true, value: clone(data.state[field][pkg]) } : { present: false };
  });
 });
 return snapshot;
}
function restoreCache(data, packages, before) {
 CACHE_FIELDS.forEach(function(field) {
  packages.forEach(function(pkg) {
   var item = before[field][pkg];
   if (item.present) data.state[field][pkg] = clone(item.value);
   else delete data.state[field][pkg];
  });
 });
}
function requireLocalScope(data, packages) {
 if (!data.state) throw error(_('Unsupported LuCI cache layout; configuration was not saved.'));
 [ 'creates', 'changes', 'deletes', 'reorder' ].forEach(function(field) {
  Object.keys(data.state[field] || {}).forEach(function(pkg) {
   var value = data.state[field][pkg];
   if (packages.indexOf(pkg) < 0 && (value === true || (value && Object.keys(value).length)))
    throw error(_('Unrelated local changes must be saved from their own page first; configuration was not saved.'));
  });
 });
}
async function requirePendingScope(packages) {
 try {
  var reply = await calls.changes();
  if (!reply || !reply.changes || typeof reply.changes !== 'object' || Array.isArray(reply.changes) ||
      !Object.keys(reply.changes).every(function(pkg) { return Array.isArray(reply.changes[pkg]); }))
   throw error(_('Unable to verify pending changes; configuration was not applied.'));
  if (Object.keys(reply.changes).some(function(pkg) { return reply.changes[pkg].length && packages.indexOf(pkg) < 0; }))
   throw error(_('Unrelated pending packages must be applied or reverted separately; configuration was not applied.'));
 } catch (cause) {
  if (cause && cause[OWN_ERROR]) throw cause;
  throw error(_('Unable to verify pending changes; configuration was not applied.'));
 }
}
function locked(operation) {
 if (busy) return Promise.reject(error(_('Another configuration validation is still running.')));
 busy = true;
 return Promise.resolve().then(operation).finally(function() { busy = false; });
}
async function validate(data, cancelled) {
 var original = rawCandidate(data);
 var originalText = JSON.stringify(original);
 var id = requestId(), token = '', digest = '', began = false, finished = false;
 var start = Date.now();
 function current() {
  if ((cancelled && cancelled()) || Date.now() - start > 90000)
   throw error(_('Configuration validation was cancelled or expired; configuration was not saved.'));
  if (JSON.stringify(rawCandidate(data)) !== originalText)
   throw error(_('The candidate changed while being validated; review it and save again.'));
 }
 try {
  var schema = schemaResult(await calls.schema());
  current();
  var candidate = { schema_version: 1, request_id: id,
   sections: original.sections.map(function(section) {
    return { name: section.name, options: selectedOptions(section.options, schema.fields) };
   }), globals: selectedOptions(original.globals, schema.global_fields),
   sqm: original.sqm.map(function(section) {
    return { name: section.name, options: selectedOptions(section.options, Object.keys(section.options)) };
   }) };
  var bytes = new TextEncoder().encode(JSON.stringify(candidate));
  if (bytes.length > MAX_BYTES) throw error(_('The configuration candidate exceeds the native transfer limit.'));
  digest = cakeHash.digest(bytes);
  began = true;
  var begin = result(await calls.begin(id, bytes.length, digest), id);
  if (!/^[0-9a-f]{32}$/.test(begin.token || '') || begin.chunk_bytes !== 1024)
   throw error(_('Native candidate transfer returned an invalid handle.'));
  token = begin.token;
  for (var offset = 0; offset < bytes.length; offset += 1024) {
   current();
   var part = bytes.subarray(offset, Math.min(offset + 1024, bytes.length));
   var ack = result(await calls.append(id, token, offset, hex(part)), id);
   if (ack.next_offset !== offset + part.length)
    throw error(_('Native candidate transfer did not acknowledge the exact byte range.'));
  }
  current();
  var terminal = result(await calls.finish(id, token), id);
  var validation = terminal.validation;
  if (!validation || validation.schema_version !== 1 || validation.validation_scope !== SCOPE ||
      validation.request_id !== id || validation.candidate_sha256 !== digest ||
      typeof validation.valid !== 'boolean')
   throw error(_('Native candidate validation returned an unbound receipt.'));
  finished = true;
  current();
  if (!validation.valid) {
   var detail = Array.isArray(validation.errors) ? validation.errors.map(function(item) {
    return String(item.instance || '') + ': ' + String(item.message || item.code || '');
   }).join('\n') : String(validation.code || 'candidate-invalid');
   throw error(_('Configuration rejected before saving: ') + detail.slice(0, 4096));
  }
  return { receipt: validation, check: current };
 } catch (cause) {
  // Never expose an RPC implementation's raw request/error body.
  if (cause && cause[OWN_ERROR] === true)
   throw cause;
  throw error(_('Native candidate validation failed; configuration was not saved.'));
 } finally {
  if (began && !finished) {
   try { await calls.cancel(id, token, digest); } catch (_) { /* bounded private state expires; no Apply follows */ }
  }
 }
}
function attach(map, ownedPackages) {
 if (maps.has(map)) return map;
 if (!map || typeof map.save !== 'function' || !map.data)
  throw error(_('Unsupported LuCI form API; configuration was not saved.'));
 var state = { cancelled: false }, save = map.save;
 maps.set(map, state);
 map.save = function(cb, silent) {
  var self = this;
  var started = false;
  return locked(function() {
   started = true;
   state.cancelled = false;
   var packages = ownedPackages || [ 'cake-autorate', 'sqm' ];
   var data = self.data;
   requireLocalScope(data, packages);
   var before = cacheSnapshot(data, packages), parsed = null, admission = null, saveStarted = false;
   var proxy = new Proxy(data, { get: function(target, key) {
    if (key === 'save') return function() {
     if (!admission) throw error(_('The candidate has not been validated; configuration was not saved.'));
     admission.check();
     requireLocalScope(data, packages);
     saveStarted = true;
     return data.save.apply(data, arguments);
    };
    var value = target[key];
    return typeof value === 'function' ? value.bind(target) : value;
   } });
   self.data = proxy;
   return Promise.resolve().then(function() { return save.call(self, function() {
    return Promise.resolve(typeof cb === 'function' ? cb() : null).then(function() {
     parsed = cacheSnapshot(self.data, packages);
     return validate(self.data, function() { return state.cancelled; });
    }).then(function(value) { admission = value; return value.receipt; });
   }, silent); }).catch(function(cause) {
    // Restore only the exact state produced by this parse. Concurrent changes
    // or an already attempted UCI save must never be reverted by this guard.
    if (!saveStarted && parsed &&
        JSON.stringify(cacheSnapshot(self.data, packages)) === JSON.stringify(parsed))
     restoreCache(self.data, packages, before);
    if (saveStarted && !(cause && cause[OWN_ERROR]))
     cause = error(_('Saving the validated candidate failed; refresh pending changes before retrying.'));
    throw cause;
   }).finally(function() { if (self.data === proxy) self.data = data; });
  }).catch(function(cause) {
   if (silent && (!started || !state.cancelled) && cause && cause[OWN_ERROR]) {
    var message = document.createElement('p');
    message.textContent = String(cause.message).slice(0, 4096);
    ui.addNotification(null, message, 'error');
   }
   throw cause;
  });
 };
 return map;
}
function protectModal(section) {
 var cancel = section.handleModalCancel;
 if (typeof cancel !== 'function') return;
 section.handleModalCancel = function(map, ev, isSaving) {
  if (!isSaving && maps.has(map)) maps.get(map).cancelled = true;
  return cancel.apply(this, arguments);
 };
}
return L.Class.extend({
 loadSchema: function() { return calls.schema().then(schemaResult).catch(function(cause) {
  rateRules = null;
  throw cause && cause[OWN_ERROR] ? cause : error(_('Native validation rules are unavailable.'));
 }); },
 parseRate: parseRate,
 validateRateTuple: validateRateTuple,
 directionEnabled: directionEnabled,
 attach: attach,
 protectModal: protectModal,
 validate: function() { return locked(function() { return validate(uci).then(function(admission) { admission.check(); return admission.receipt; }); }); },
 save: function(packages) { return locked(function() {
  packages = packages || [ 'cake-autorate', 'sqm' ];
  requireLocalScope(uci, packages);
  return validate(uci).then(function(admission) { admission.check(); requireLocalScope(uci, packages); return uci.save(); });
 }); },
 apply: function(action, packages) { return locked(async function() {
  packages = packages || [ 'cake-autorate', 'sqm' ];
  requireLocalScope(uci, packages);
  await requirePendingScope(packages);
  var admission = await validate(uci);
  await requirePendingScope(packages);
  admission.check();
  requireLocalScope(uci, packages);
  return action();
 }); },
 snapshot: rawCandidate
});
