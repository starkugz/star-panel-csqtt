'use strict';
'require view';
'require rpc';
'require uci';
'require ui';

// [star-panel] Вью профилей CSQTT (ubus-объект "csqtt").
// Компактный вертикальный список карточек вместо широкой таблицы. Три
// независимых понятия разведены и показаны фактическими данными:
//   * «включён»  — флаг enabled профиля (переключатель с подписью);
//   * «активный» — выбранный рабочий профиль (метка на карточке);
//   * «состояние»— фактическое состояние подключения (badge из status.json).
// Главное действие — «Подключить»; второстепенные — «Изменить»/«Экспорт»;
// «Удалить» визуально отделено и требует подтверждения. Технические поля
// (ID, workers, TURN/OBFS, CAPTCHA, заметка) — в раскрываемых «Подробностях».
// Редактор — UCI (acl uci csqtt write); password/vk_js_token не эхо-ятся.
// Импорт/экспорт csqtt:// — контракт link.rs; верхние действия сохранены.

const callProfiles = rpc.declare({ object: 'csqtt', method: 'profiles' });
const callStatus   = rpc.declare({ object: 'csqtt', method: 'status' });
const callAction   = rpc.declare({ object: 'csqtt', method: 'profile_action', params: ['action', 'id'] });
const callImport   = rpc.declare({ object: 'csqtt', method: 'profile_import', params: ['link', 'id', 'name', 'activate', 'commit'] });
const callExport   = rpc.declare({ object: 'csqtt', method: 'profile_export', params: ['id'] });
const callTestConf = rpc.declare({ object: 'csqtt', method: 'test_conf' });
const callHashesValidate = rpc.declare({ object: 'csqtt', method: 'hashes_validate', params: ['id', 'vk'] });

const BRAND = 'star-panel-csqtt';
const ID_RE = /^[A-Za-z0-9_]+$/;

// Нормализация VK-токена (auto_js). Синхронизировано с ядром
// (csqtt-openwrt/lib.rs normalize_vk_js_token): принимает «голый» токен или
// implicit-flow redirect-URL, URL-декодирует параметр и убирает пробелы по
// краям. Возвращает токен, '' для пустого поля или null, если распознать не
// удалось (произвольный текст не должен попадать в UCI как якобы токен).
// CSQTT_VK_TOKEN_BEGIN
function extractTokenParameter(v) {
	var access = null, token = null, parts = v.split(/[?#&]/), eq, key, val;
	for (var i = 0; i < parts.length; i++) {
		eq = parts[i].indexOf('=');
		if (eq <= 0)
			continue;
		key = parts[i].slice(0, eq);
		val = parts[i].slice(eq + 1);
		if (val === '')
			continue;
		if (key === 'access_token')
			access = val;
		else if (key === 'token')
			token = val;
	}
	return access !== null ? access : token;
}

function isPlausibleToken(t) {
	if (!t || /\s/.test(t))
		return false;
	if (/^vk1\./.test(t))
		return t.length > 8 && /^[A-Za-z0-9._=-]+$/.test(t.slice(4));
	return t.length >= 80 && /^[0-9A-Fa-f]+$/.test(t);
}

function percentDecode(v) {
	try { return decodeURIComponent(v); } catch (e) { return v; }
}

function normalizeVkToken(raw) {
	var v = String(raw == null ? '' : raw).trim();
	if (v === '')
		return '';
	var param = extractTokenParameter(v), t;
	if (param !== null) {
		t = percentDecode(param).trim();
		return isPlausibleToken(t) ? t : null;
	}
	var i = v.indexOf('vk1.');
	if (i >= 0) {
		t = v.slice(i).split(/[&#?\s]/)[0].trim();
		return isPlausibleToken(t) ? t : null;
	}
	return isPlausibleToken(v) ? v : null;
}
// CSQTT_VK_TOKEN_END

const CSS = [
	'.csqtt{width:100%;max-width:1080px;box-sizing:border-box;line-height:1.45;}',
	'.csqtt *,.csqtt *::before,.csqtt *::after{box-sizing:border-box;}',
	'.csqtt-brand{display:flex;flex-wrap:wrap;align-items:baseline;gap:.55em;margin:0 0 .9em;padding:0 0 .6em;border-bottom:1px solid rgba(127,127,127,.3);}',
	'.csqtt-brand-name{font-size:1.3em;font-weight:700;}',
	'.csqtt-brand-tag{font-size:.85em;opacity:.7;}',
	'.csqtt-actions{display:flex;flex-wrap:wrap;align-items:center;gap:.5em;margin:0 0 1em;}',
	'.csqtt button.btn,.csqtt a.btn,.csqtt .cbi-button{height:2.4em;min-height:2.4em;line-height:1;display:inline-flex;align-items:center;justify-content:center;gap:.35em;padding:.35em .9em;margin:0;border:1px solid rgba(127,127,127,.38);border-radius:8px;background:rgba(127,127,127,.07);color:inherit;font:inherit;font-size:.92em;cursor:pointer;text-decoration:none;white-space:nowrap;}',
	'.csqtt button.btn:hover,.csqtt a.btn:hover{border-color:#4a90e2;}',
	'.csqtt button.btn:focus-visible,.csqtt a.btn:focus-visible,.csqtt .cbi-button:focus-visible,.csqtt summary:focus-visible{outline:2px solid #4a90e2;outline-offset:2px;}',
	'.csqtt button.btn.primary{background:#2f7fd6;border-color:transparent;color:#fff;}',
	'.csqtt button.btn.negative{border-color:rgba(217,83,79,.55);color:#e08b88;}',
	'.csqtt button.btn[disabled]{opacity:.5;cursor:default;}',
	'.csqtt-list{display:flex;flex-direction:column;gap:.75em;}',
	'.csqtt-card{min-width:0;border:1px solid rgba(127,127,127,.28);border-radius:10px;padding:.75em 1em;background:rgba(127,127,127,.05);}',
	'.csqtt-card.is-active{border-color:rgba(74,144,226,.55);background:rgba(74,144,226,.08);}',
	'.csqtt-card-head{display:flex;flex-wrap:wrap;align-items:center;gap:.35em .7em;margin:0 0 .6em;}',
	'.csqtt-card-name{font-size:15px;font-weight:600;overflow-wrap:anywhere;min-width:0;}',
	'.csqtt-card-head .csqtt-badge{margin-left:auto;}',
	'.csqtt-card-grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(200px,1fr));gap:.6em 1.2em;}',
	'.csqtt-field{min-width:0;display:flex;flex-direction:column;gap:.1em;}',
	'.csqtt-field-label{font-size:12px;opacity:.65;}',
	'.csqtt-field-value{font-size:14px;line-height:1.4;overflow-wrap:anywhere;}',
	'.csqtt-mono{font-family:ui-monospace,Menlo,Consolas,monospace;font-size:13px;}',
	'.csqtt-active-tag{display:inline-flex;align-items:center;gap:.3em;color:inherit;font-weight:600;font-size:.85em;white-space:nowrap;}',
	'.csqtt-active-tag::before{content:"●";color:#4a90e2;font-size:.8em;}',
	'.csqtt-muted{opacity:.5;}',
	'.csqtt-badge{display:inline-block;border:1px solid rgba(127,127,127,.35);border-radius:1em;padding:.05em .6em;font-size:.82em;white-space:nowrap;background:rgba(127,127,127,.12);color:inherit;}',
	'.csqtt-badge.success{background:rgba(63,157,88,.18);border-color:rgba(63,157,88,.6);}',
	'.csqtt-badge.warning{background:rgba(224,164,74,.2);border-color:rgba(224,164,74,.65);}',
	'.csqtt-badge.danger{background:rgba(217,83,79,.18);border-color:rgba(217,83,79,.6);}',
	'.csqtt-switch{position:relative;display:inline-flex;align-items:center;gap:.45em;cursor:pointer;user-select:none;white-space:nowrap;font-size:.9em;min-height:2.2em;}',
	'.csqtt-switch input{position:absolute;inset:0;width:100%;height:100%;margin:0;opacity:0;cursor:pointer;}',
	'.csqtt-switch .track{width:2.4em;height:1.3em;border-radius:1em;background:rgba(127,127,127,.4);position:relative;flex:0 0 auto;transition:background .15s;}',
	'.csqtt-switch .track::after{content:"";position:absolute;top:.16em;left:.16em;width:.98em;height:.98em;border-radius:50%;background:#fff;transition:left .15s;}',
	'.csqtt-switch input:checked + .track{background:#3f9d58;}',
	'.csqtt-switch input:checked + .track::after{left:1.26em;}',
	'.csqtt-switch input:focus-visible + .track{outline:2px solid #4a90e2;outline-offset:2px;}',
	'.csqtt-switch input:disabled + .track{opacity:.5;}',
	'.csqtt-prio{display:inline-flex;align-items:center;gap:.35em;}',
	'.csqtt-prio .val{min-width:1.6em;text-align:center;font-variant-numeric:tabular-nums;font-size:14px;}',
	'.csqtt-prio button.btn{padding:.15em .55em;font-size:.85em;height:2.6em;min-height:2.6em;}',
	'.csqtt-card-actions{display:flex;flex-wrap:wrap;align-items:center;gap:.5em;margin-top:.75em;}',
	'.csqtt-card-actions .spacer{flex:1 1 auto;}',
	'.csqtt-card-actions .danger-sep{border-left:1px solid rgba(127,127,127,.3);padding-left:.5em;}',
	'.csqtt-details{margin-top:.6em;padding-top:.5em;border-top:1px solid rgba(127,127,127,.18);}',
	'.csqtt-details summary{cursor:pointer;opacity:.8;font-size:13px;}',
	'.csqtt-details[open] summary{margin-bottom:.5em;}',
	'.csqtt-details-grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(180px,1fr));gap:.5em 1.2em;}',
	'.csqtt-empty{border:1px dashed rgba(127,127,127,.4);border-radius:10px;padding:1.5em 1em;text-align:center;opacity:.75;}',
	'.csqtt-ellipsis{display:inline-block;vertical-align:middle;max-width:100%;overflow-wrap:anywhere;}',
	'.csqtt-hint{opacity:.72;font-size:13px;margin:.9em 0 0;max-width:60em;overflow-wrap:anywhere;}',
	'.csqtt-imp-sec{margin:.2em 0 1em;}',
	'.csqtt-imp-sec-title{font-size:.95em;font-weight:700;margin:.2em 0 .6em;padding-bottom:.3em;border-bottom:1px solid rgba(127,127,127,.3);}',
	'.csqtt-desc{font-size:.85em;opacity:.72;line-height:1.35;margin-top:.2em;max-width:38em;overflow-wrap:anywhere;}',
	'.csqtt-warn{font-size:.85em;color:#e06c6c;margin-top:.2em;overflow-wrap:anywhere;}',
	'.csqtt-imp-sec .cbi-value-title{width:16em;}',
	'.csqtt-imp-sec textarea{width:100%;}',
	'.csqtt-hash-tools{display:flex;flex-wrap:wrap;align-items:center;gap:.5em;margin-top:.4em;}',
	'.csqtt-hash-status{font-size:.85em;}',
	'.csqtt-hash-list{margin-top:.3em;font-family:monospace;font-size:.9em;line-height:1.5;}',
	'.csqtt-hash-ok{color:#79c98a;}',
	'.csqtt-hash-bad{color:#e06c6c;}',
	'.csqtt-hash-checking{opacity:.72;}',
	'@media (max-width:560px){',
	' .csqtt-card{padding:.7em .8em;}',
	' .csqtt button.btn,.csqtt a.btn,.csqtt .cbi-button{min-height:2.8em;}',
	' .csqtt-card-actions{width:100%;}',
	' .csqtt-card-actions .spacer{display:none;}',
	' .csqtt-card-actions .danger-sep{border-left:0;padding-left:0;width:100%;margin-top:.2em;}',
	' .csqtt-card-actions .danger-sep button.btn{width:100%;}',
	' .csqtt-imp-sec .cbi-value{display:block;}',
	' .csqtt-imp-sec .cbi-value-title{display:block;width:auto;padding:0 0 .2em;}',
	'}',
].join('');

function notify(msg, kind) {
	ui.addTimeLimitedNotification('csqtt', E('p', msg), 6000, kind);
}

function stateLabel(state) {
	switch (state) {
		case 'active': return _('Connected');
		case 'ready': return _('Ready');
		case 'connecting': return _('Connecting');
		case 'failed': return _('Failed');
		case 'standby': return _('Standby');
		case 'disabled': return _('Disabled');
		case 'cooldown': return _('Cooldown');
		case 'captcha_required': return _('CAPTCHA required');
		case 'auth_failed': return _('Auth failed');
		default: return state || '—';
	}
}

function stateBadge(state) {
	if (!state)
		return E('span', { class: 'csqtt-badge' }, '—');
	var kind = (state === 'active' || state === 'ready') ? 'success'
		: (state === 'captcha_required' || state === 'cooldown' || state === 'connecting') ? 'warning'
			: (state === 'failed' || state === 'auth_failed') ? 'danger' : '';
	return E('span', { class: 'csqtt-badge %s'.format(kind) }, stateLabel(state));
}

function humanSeconds(sec) {
	sec = parseInt(sec, 10) || 0;
	if (sec < 60)
		return '';
	var m = Math.floor(sec / 60), r = sec % 60, h = Math.floor(m / 60);
	m = m % 60;
	var parts = [];
	if (h > 0) parts.push('%d %s'.format(h, _('h')));
	if (m > 0) parts.push('%d %s'.format(m, _('min')));
	if (r > 0 && h === 0) parts.push('%d %s'.format(r, _('sec')));
	return parts.join(' ');
}

function rowBtn(label, cls, handler) {
	return E('button', {
		class: 'btn %s'.format(cls || ''),
		click: handler,
	}, label);
}

function addHint(field, hint) {
	if (hint == null)
		return;
	if (Object.prototype.toString.call(hint) === '[object Array]') {
		for (var i = 0; i < hint.length; i++)
			if (hint[i] != null)
				field.push(hint[i]);
	} else {
		field.push(hint);
	}
}

function fieldRow(id, label, input, hint) {
	var field = [input];
	addHint(field, hint);
	return E('div', { class: 'cbi-value' }, [
		E('label', { class: 'cbi-value-title', for: id }, label),
		E('div', { class: 'cbi-value-field' }, field),
	]);
}

function section(title, rows) {
	return E('div', { class: 'csqtt-imp-sec' }, [
		E('h4', { class: 'csqtt-imp-sec-title' }, title),
	].concat(rows));
}

function warning(text) {
	return E('div', { class: 'csqtt-warn' }, text);
}

function cardField(label, value) {
	return E('div', { class: 'csqtt-field' }, [
		E('span', { class: 'csqtt-field-label' }, label),
		E('span', { class: 'csqtt-field-value' }, value),
	]);
}

function txtInput(id, value, ph, type, readonly) {
	return E('input', {
		id: id, type: type || 'text', class: 'cbi-input-text',
		value: value != null ? value : '', placeholder: ph || '',
		readonly: readonly ? 'readonly' : null,
	});
}

function selInput(id, values, current) {
	return E('select', { id: id, class: 'cbi-select' }, values.map(function(v) {
		var val = (typeof v === 'object') ? v.v : v;
		var lbl = (typeof v === 'object') ? v.l : v;
		return E('option', { value: val, selected: (val === current) ? 'selected' : null }, lbl);
	}));
}

function chkInput(id, checked) {
	return E('input', { id: id, type: 'checkbox', class: 'cbi-checkbox', checked: checked ? 'checked' : null });
}

return view.extend({
	title: BRAND + ' — ' + _('Profiles'),

	profiles: [],
	states: {},
	activeProfile: '',
	selectionMode: '',
	_open: {},

	load: function() {
		return Promise.all([
			callProfiles().catch(function() { return null; }),
			callStatus().catch(function() { return null; }),
			uci.load('csqtt').catch(function() { return null; }),
		]);
	},

	update: function(res) {
		var pr = res[0] || {}, st = (res[1] && res[1].status) || {};
		this.profiles = pr.profiles || [];
		this.selectionMode = pr.selection_mode || '';
		this.activeProfile = pr.active_profile || st.active_profile || '';
		this.states = {};
		(st.profiles || []).forEach(L.bind(function(p) {
			this.states[p.id] = p.state;
		}, this));
		this.renderList();
	},

	reload: function() {
		return this.load().then(L.bind(this.update, this)).catch(function() {});
	},

	render: function(res) {
		var self = this;

		var nodes = E('div', { class: 'csqtt csqtt-profiles' }, [
			E('style', {}, CSS),
			E('div', { class: 'csqtt-brand' }, [
				E('span', { class: 'csqtt-brand-name' }, BRAND),
				E('span', { class: 'csqtt-brand-tag' }, _('Server profiles')),
			]),
			E('div', { class: 'csqtt-actions' }, [
				rowBtn(_('Add profile'), 'primary', L.bind(this.openEditor, this, null)),
				rowBtn(_('Import link'), '', L.bind(this.openImport, this)),
				rowBtn(_('Auto (by priority)'), '', L.bind(this.handleAuto, this)),
			]),
			E('div', { class: 'csqtt-listwrap' }, E('div', { id: 'csqtt-profiles-box' })),
			E('p', { class: 'csqtt-hint' }, _('Passwords and VK tokens are never read back from the router; leaving a secret field empty keeps the stored value.')),
		]);

		window.setTimeout(function() { self.update(res); }, 0);
		this.startRefresh();

		return nodes;
	},

	startRefresh: function() {
		var self = this;
		this._pollFn = function() {
			if (!document.getElementById('csqtt-profiles-box'))
				return;
			return self.reload();
		};
		L.Poll.add(this._pollFn, 5);
	},

	unload: function() {
		if (this._pollFn) {
			L.Poll.remove(this._pollFn);
			this._pollFn = null;
		}
	},

	handleSave: null,
	handleReset: null,
	handleSaveApply: null,

	isActive: function(p) {
		return (p.id === this.activeProfile) || (this.states[p.id] === 'active');
	},

	setBusy: function(btn, on) {
		if (btn && 'disabled' in btn)
			btn.disabled = !!on;
	},

	switch: function(id, enabled, checkbox) {
		var self = this, want = enabled ? 'enable' : 'disable';
		checkbox.disabled = true;
		callAction(want, id)
			.then(function(res) {
				if (res && res.ok) {
					notify(_('%s: %s').format(want, id), 'info');
					return self.reload();
				}
				checkbox.checked = !enabled;
				checkbox.disabled = false;
				notify((res && (res.error || res.output)) || _('action failed'), 'error');
			})
			.catch(function() {
				checkbox.checked = !enabled;
				checkbox.disabled = false;
				notify(_('Permission denied or rpcd error'), 'error');
			});
	},

	profileCard: function(p) {
		var isActive = this.isActive(p);
		var enabled = (p.enabled === '1' || p.enabled === 'true');
		var state = this.states[p.id];
		var name = p.name || p.id;

		var toggle = E('label', { class: 'csqtt-switch' }, [
			E('input', {
				type: 'checkbox',
				class: 'cbi-checkbox',
				checked: enabled ? 'checked' : null,
				'aria-label': _('Enabled'),
				change: L.bind(function(ev) {
					this.switch(p.id, ev.target.checked, ev.target);
				}, this),
			}),
			E('span', { class: 'track' }),
			E('span', { class: 'txt' }, enabled ? _('Enabled') : _('Disabled')),
		]);

		var priority = E('span', { class: 'csqtt-prio' }, [
			E('span', { class: 'val' }, String(p.priority || '0')),
			E('button', { class: 'btn', title: _('Up'), 'aria-label': _('Up'), click: L.bind(this.handleMove, this, p.id, -1) }, '↑'),
			E('button', { class: 'btn', title: _('Down'), 'aria-label': _('Down'), click: L.bind(this.handleMove, this, p.id, 1) }, '↓'),
		]);

		var details = E('details', {
			class: 'csqtt-details',
			open: this._open[p.id] ? 'open' : null,
			toggle: L.bind(function(ev) {
				this._open[p.id] = !!ev.target.open;
			}, this),
		}, [
			E('summary', {}, _('Details')),
			E('div', { class: 'csqtt-details-grid' }, [
				cardField(_('ID'), E('span', { class: 'csqtt-mono' }, p.id)),
				cardField(_('Workers'), p.workers != null ? String(p.workers) : '—'),
				cardField(_('TURN / obfs'), '%s / %s'.format(p.turn_transport || '—', p.obfs || '—')),
				cardField('CAPTCHA', p.captcha_mode || '—'),
				cardField(_('Note'), p.note || '—'),
			]),
		]);

		return E('div', { class: 'csqtt-card%s'.format(isActive ? ' is-active' : '') }, [
			E('div', { class: 'csqtt-card-head' }, [
				E('span', { class: 'csqtt-card-name', title: name }, name),
				isActive ? E('span', { class: 'csqtt-active-tag' }, _('Active')) : '',
				stateBadge(state),
			]),
			E('div', { class: 'csqtt-card-grid' }, [
				cardField(_('Server / Peer'), E('span', { class: 'csqtt-mono csqtt-ellipsis', title: p.peer || '' }, p.peer || '—')),
				cardField(_('Enabled'), toggle),
				cardField(_('Priority'), priority),
			]),
			E('div', { class: 'csqtt-card-actions' }, [
				rowBtn(_('Connect'), 'primary', L.bind(this.handleAction, this, 'use', p.id)),
				rowBtn(_('Edit'), '', L.bind(this.openEditor, this, p.id)),
				rowBtn(_('Export'), '', L.bind(this.handleExport, this, p.id)),
				E('span', { class: 'spacer' }),
				E('span', { class: 'danger-sep' }, rowBtn(_('Delete'), 'negative', L.bind(this.handleRemove, this, p.id, name))),
			]),
			details,
		]);
	},

	renderList: function() {
		var box = document.getElementById('csqtt-profiles-box');
		if (!box)
			return;

		if (!this.profiles.length) {
			L.dom.content(box, E('div', { class: 'csqtt-empty' }, _('No profiles yet. Add a profile or import a link.')));
			return;
		}

		var cards = this.profiles.map(L.bind(function(p) {
			return this.profileCard(p);
		}, this));

		L.dom.content(box, E('div', { class: 'csqtt-list' }, cards));
	},

	/* --- profile_action / service ------------------------------------------- */

	runAction: function(promise, okMsg, btn) {
		var self = this;
		this.setBusy(btn, true);
		return promise
			.then(L.bind(function(res) {
				if (res && res.ok) {
					if (okMsg)
						notify(okMsg, 'info');
					return this.reload();
				}
				notify((res && (res.error || res.output)) || _('action failed'), 'error');
			}, this))
			.catch(function() {
				notify(_('Permission denied or rpcd error'), 'error');
			})
			.then(function() { self.setBusy(btn, false); });
	},

	handleAction: function(action, id, ev) {
		this.runAction(callAction(action, id), _('%s: %s').format(action, id), ev && ev.currentTarget);
	},

	handleAuto: function(ev) {
		this.runAction(callAction('auto', ''), _('selection mode = priority (auto)'), ev && ev.currentTarget);
	},

	handleRemove: function(id, name, ev) {
		if (!confirm(_('Delete profile "%s"? The UCI section will be removed.').format(name || id)))
			return;
		this.runAction(callAction('remove', id), _('profile %s deleted').format(id), ev && ev.currentTarget);
	},

	/* --- сортировка priority (↑/↓ обмен с соседом) -------------------------- */

	handleMove: function(id, dir) {
		var list = this.profiles.slice().sort(function(a, b) {
			return (parseInt(a.priority, 10) || 0) - (parseInt(b.priority, 10) || 0);
		});
		var i = list.findIndex(function(p) { return p.id === id; });
		var j = i + dir;
		if (i < 0 || j < 0 || j >= list.length)
			return;
		var a = list[i], b = list[j],
		    pa = parseInt(a.priority, 10) || 0,
		    pb = parseInt(b.priority, 10) || 0;
		if (pa === pb) {
			notify(_('equal priorities — tie-break by config order; set explicit values in the editor'), 'warning');
			return;
		}
		try {
			uci.set('csqtt', a.id, 'priority', String(pb));
			uci.set('csqtt', b.id, 'priority', String(pa));
			uci.save().then(function(){return uci.apply();}).then(L.bind(function() {
				return this.reload();
			}, this)).catch(function() {
				notify(_('uci commit failed (write access required)'), 'error');
			});
		} catch (e) {
			notify(_('uci write error: %s').format(e), 'error');
		}
	},

	/* --- экспорт (ссылка с секретом — только по явному действию) ------------- */

	handleExport: function(id) {
		callExport(id).then(function(res) {
			if (!res || !res.ok) {
				notify((res && (res.error || res.output)) || _('export failed'), 'error');
				return;
			}
			var input = E('input', { type: 'text', class: 'cbi-input-text', readonly: 'readonly', value: res.link, style: 'width:100%' });
			ui.showModal(_('Export profile "%s"').format(id), [
				E('p', { class: 'small' }, E('b', _('The link contains the password — treat it as a secret.'))),
				input,
				E('div', { class: 'right' }, [
					rowBtn(_('Copy'), '', function() {
						input.select();
						input.setSelectionRange(0, input.value.length);
						try { document.execCommand('copy'); } catch (e) {}
					}),
					rowBtn(_('Close'), 'negative', ui.hideModal),
				]),
			]);
		}).catch(function() {
			notify(_('Permission denied or rpcd error'), 'error');
		});
	},

	/* --- импорт: preview → commit ------------------------------------------- */

	openImport: function() {
		var self = this,
		    link = E('textarea', {
			id: 'csqtt-imp-link', class: 'cbi-input-textarea', rows: '3',
			style: 'width:100%', placeholder: _('csqtt://connect?v=2&host=…&peer=…&password=… (or legacy csqtt://pass@host:port)'),
		}),
		    out = E('pre', { id: 'csqtt-imp-out', class: 'logview', style: 'display:none;min-height:6em;max-height:18em;overflow:auto' });

		function collect(commit) {
			return callImport(
				link.value.trim(),
				document.getElementById('csqtt-imp-id').value.trim(),
				document.getElementById('csqtt-imp-name').value.trim(),
				document.getElementById('csqtt-imp-act').checked,
				commit);
		}

		function show(res) {
			out.style.display = '';
			var head = res
				? (res.code != null ? (res.ok ? 'OK' : 'ERROR') + ' (code %s)'.format(res.code)
					: (res.ok ? 'OK' : 'ERROR'))
				: 'rpc error';
			out.textContent = '%s\n%s'.format(head, (res && (res.output || res.error)) || '');
		}

		ui.showModal(_('Import profile'), [
			E('p', { class: 'small' }, _('Supported: csqtt://connect v2 and legacy csqtt:// links (parser link.rs). VK hashes / vk.com/call/join links go to the “vk” field of the profile editor. For token-based auto_js profiles (a link without hashes) leave Activate off, set the VK JS token in the editor, then enable the profile. Preview does not touch the live config; commit is transactional with backup (M4a).')),
			link,
			E('div', { class: 'cbi-value' }, [
				E('label', { class: 'cbi-value-title' }, _('ID')),
				E('div', { class: 'cbi-value-field' }, txtInput('csqtt-imp-id', '', 'auto from host')),
				E('label', { class: 'cbi-value-title' }, _('Name')),
				E('div', { class: 'cbi-value-field' }, txtInput('csqtt-imp-name', '', 'optional')),
			]),
			E('div', { class: 'cbi-value' }, [
				E('label', { class: 'cbi-value-title' }, _('Activate')),
				E('div', { class: 'cbi-value-field' }, chkInput('csqtt-imp-act', false)),
			]),
			out,
			E('div', { class: 'right' }, [
				rowBtn(_('Preview'), '', function() {
					collect(false).then(show).catch(function() { show(null); });
				}),
				' ',
				rowBtn(_('Save (commit)'), 'primary', function() {
					collect(true).then(function(res) {
						show(res);
						if (res && res.ok) {
							link.value = '';
							notify(_('profile imported'), 'info');
							self.reload();
							ui.hideModal();
						}
					}).catch(function() { show(null); });
				}),
				' ',
				rowBtn(_('Close'), 'negative', ui.hideModal),
			]),
		]);
	},

	/* --- редактор профиля (UCI) ---------------------------------------------- */

	openEditor: function(id) {
		var self = this,
		    isNew = (id == null),
		    sec = isNew ? null : uci.get('csqtt', id),
		    gm = {},
		    get = function(k) {
			return (sec && sec[k] != null) ? String(sec[k]) : '';
		};
		try { gm = uci.get('csqtt', 'main') || {}; } catch (e) { gm = {}; }
		var gv = function(k, d) { return (gm[k] != null && gm[k] !== '') ? String(gm[k]) : (d || ''); };

		var f = {
			id: txtInput('csqtt-ed-id', isNew ? '' : id, 'profile1', 'text', !isNew),
			name: txtInput('csqtt-ed-name', get('name')),
			enabled: chkInput('csqtt-ed-enabled', isNew ? true : get('enabled') !== '0'),
			priority: txtInput('csqtt-ed-priority', get('priority') || '10'),
			peer: txtInput('csqtt-ed-peer', get('peer'), 'host:port'),
			password: txtInput('csqtt-ed-password', '', isNew ? _('required for an enabled profile') : _('— unchanged —'), 'password'),
			vk: E('textarea', { id: 'csqtt-ed-vk', class: 'cbi-input-textarea', rows: '2' }, get('vk')),
			workers: txtInput('csqtt-ed-workers', get('workers') || '18', '9..126'),
			obfs: selInput('csqtt-ed-obfs', [{ v: 'audio', l: 'Audio' }, { v: 'video', l: 'Video' }], get('obfs') || 'audio'),
			turn_transport: selInput('csqtt-ed-turn', [
				{ v: 'udp', l: 'UDP' }, { v: 'tcp_tls', l: 'TCP (TLS)' }], get('turn_transport') || 'udp'),
			captcha_mode: selInput('csqtt-ed-capmode', [
				{ v: 'auto', l: 'Auto' }, { v: 'wv', l: 'WebView' }, { v: 'rjs', l: 'Rust JS' }], get('captcha_mode') || 'auto'),
			fingerprint: selInput('csqtt-ed-fp', [
				{ v: 'chrome', l: 'Chrome' }, { v: 'firefox', l: 'Firefox' }], get('fingerprint') || 'chrome'),
			client_ids: txtInput('csqtt-ed-clientids', get('client_ids')),
			vk_auth_mode: selInput('csqtt-ed-vkauth', [
				{ v: 'vkcalls', l: 'VK Calls' }, { v: 'auto_js', l: 'Auto JS' }], get('vk_auth_mode') || 'vkcalls'),
			vk_hash_mode: selInput('csqtt-ed-vkhash', [
				{ v: 'manual', l: 'Manual' }, { v: 'auto_js', l: 'Auto JS' }], get('vk_hash_mode') || 'manual'),
			vk_js_token: txtInput('csqtt-ed-vktok', '', _('— unchanged —'), 'password'),
			device_id: txtInput('csqtt-ed-devid', get('device_id')),
			note: txtInput('csqtt-ed-note', get('note')),
			fail_threshold: txtInput('csqtt-ed-ft', get('fail_threshold'), gv('fail_threshold', '3')),
			success_threshold: txtInput('csqtt-ed-st', get('success_threshold'), gv('success_threshold', '2')),
			cooldown: txtInput('csqtt-ed-cd', get('cooldown'), gv('cooldown', '60')),
			captcha_policy: selInput('csqtt-ed-cppol', [
				{ v: '', l: _('— global —') },
				{ v: 'failover', l: _('Switch to another profile') },
				{ v: 'wait', l: _('Wait for CAPTCHA to be solved') }], get('captcha_policy') || ''),
		};

		var saveBtn = rowBtn(_('Save'), 'primary', save);
		var saving = false;

		function save() {
			if (saving)
				return;
			var nid = f.id.value.trim();
			if (!ID_RE.test(nid) || nid === 'main' || nid === 'routing') {
				notify(_('id must match [A-Za-z0-9_]+ and differ from main/routing'), 'error');
				return;
			}
			// VK-токен: пустое поле = «не менять»; нераспознанный ввод —
			// понятная ошибка до любых правок UCI.
			var tokenValue = normalizeVkToken(f.vk_js_token.value);
			if (tokenValue === null) {
				notify(_('VK token not recognized. Paste the token itself or the full OAuth redirect URL containing access_token.'), 'error');
				return;
			}
			try {
				if (isNew)
					uci.add('csqtt', 'server', nid);
				uci.set('csqtt', nid, 'name', f.name.value.trim() || nid);
				uci.set('csqtt', nid, 'enabled', f.enabled.checked ? '1' : '0');
				uci.set('csqtt', nid, 'priority', f.priority.value.trim() || '10');
				uci.set('csqtt', nid, 'peer', f.peer.value.trim());
				uci.set('csqtt', nid, 'vk', f.vk.value.trim());
				uci.set('csqtt', nid, 'workers', f.workers.value.trim() || '18');
				uci.set('csqtt', nid, 'obfs', f.obfs.value);
				uci.set('csqtt', nid, 'turn_transport', f.turn_transport.value);
				uci.set('csqtt', nid, 'captcha_mode', f.captcha_mode.value);
				uci.set('csqtt', nid, 'fingerprint', f.fingerprint.value);
				uci.set('csqtt', nid, 'vk_auth_mode', f.vk_auth_mode.value);
				uci.set('csqtt', nid, 'vk_hash_mode', f.vk_hash_mode.value);
				uci.set('csqtt', nid, 'note', f.note.value.trim());
				['password', 'vk_js_token', 'client_ids', 'device_id',
					'fail_threshold', 'success_threshold', 'cooldown', 'captcha_policy'].forEach(function(k) {
					var v;
					if (k === 'vk_js_token')
						v = tokenValue;
					else if (k === 'device_id')
						v = f[k].value.replace(/\s+/g, '');
					else
						v = f[k].value.trim();
					if (v !== '')
						uci.set('csqtt', nid, k, v);
				});
			} catch (e) {
				notify(_('uci write error: %s').format(e), 'error');
				return;
			}
			f.password.value = '';
			f.vk_js_token.value = '';
			saving = true;
			saveBtn.disabled = true;
			uci.save().then(function(){return uci.apply();}).then(function() {
				ui.hideModal();
				notify(_('saved — running test_conf…'), 'info');
				return callTestConf().then(function(rep) {
					if (rep && rep.ok)
						notify(_('test_conf: OK'), 'info');
					else
						notify(_('test_conf reported problems (code %s) — see Settings/doctor').format(rep ? rep.code : '?'), 'warning');
				}).catch(function() {
					notify(_('Diagnostics failed'), 'warning');
				});
			}).then(function() {
				return self.reload();
			}).catch(function() {
				saving = false;
				saveBtn.disabled = false;
				notify(_('uci commit failed (write access required)'), 'error');
			});
		}

		var vkStatus = E('div', { id: 'csqtt-ed-vk-status', class: 'csqtt-hash-status' });

		function renderHashResult(res) {
			L.dom.content(vkStatus, '');
			if (!res || !res.ok) {
				vkStatus.appendChild(E('span', { class: 'csqtt-hash-bad' },
					(res && res.error) ? res.error : _('Hash check failed')));
				return;
			}
			if ((res.total || 0) === 0) {
				vkStatus.appendChild(E('span', { class: 'csqtt-hint' }, _('No hashes to check')));
				return;
			}
			vkStatus.appendChild(E('span', { class: 'csqtt-hint' },
				_('Valid: %s · invalid: %s').format(res.valid, res.invalid)));
			var list = E('div', { class: 'csqtt-hash-list' });
			(res.hashes || []).forEach(function(h) {
				var ok = (h.status === 'valid');
				var text = '%s. %s — %s'.format(h.index + 1, h.masked, ok ? _('valid') : _('invalid'));
				if (!ok && h.code)
					text += ' (' + h.code + ')';
				list.appendChild(E('div', { class: ok ? 'csqtt-hash-ok' : 'csqtt-hash-bad' }, text));
			});
			vkStatus.appendChild(list);
		}

		function checkHashes() {
			var vk = f.vk.value.trim();
			if (vk === '') {
				notify(_('VK hashes / links field is empty'), 'warning');
				return;
			}
			L.dom.content(vkStatus, '');
			vkStatus.appendChild(E('span', { class: 'csqtt-hash-checking' }, _('Checking hashes…')));
			callHashesValidate(isNew ? '' : id, vk).then(function(res) {
				renderHashResult(res);
			}).catch(function(e) {
				L.dom.content(vkStatus, '');
				vkStatus.appendChild(E('span', { class: 'csqtt-hash-bad' },
					_('Hash check failed: %s').format(e)));
			});
		}

		var vkTools = E('div', { class: 'csqtt-hash-tools' }, [
			rowBtn(_('Check hashes'), '', checkHashes),
			vkStatus,
		]);

		var cooldownHuman = humanSeconds(gv('cooldown', '60'));
		var overrideHint = function(globalVal) {
			return _('Empty = use the global setting (%s).').format(globalVal);
		};

		ui.showModal(isNew ? _('Add profile') : _('Edit profile "%s"').format(id), [
			section(_('General'), [
				fieldRow('csqtt-ed-id', _('ID'), f.id, _('Internal UCI section name (letters, digits, underscore).')),
				fieldRow('csqtt-ed-name', _('Name'), f.name, _('Display name of the profile in LuCI.')),
				fieldRow('csqtt-ed-enabled', _('Profile enabled'), f.enabled, _('Disabled profiles do not participate in automatic selection.')),
				fieldRow('csqtt-ed-priority', _('Priority'), f.priority, _('Lower number means higher priority (10 is preferred over 20).')),
			]),
			section(_('Connection'), [
				fieldRow('csqtt-ed-peer', _('Server / Peer'), f.peer, _('Address of the CSQTT server (host:port).')),
				fieldRow('csqtt-ed-password', _('Password'), f.password,
					_('Connection secret. The stored password is never read back; an empty field keeps the existing value.')),
				fieldRow('csqtt-ed-vk', _('VK hashes / links'), f.vk,
					_('Comma-separated VK hashes or vk.com/call/join links (parsed by the core).')),
				vkTools,
			]),
			section(_('Performance'), [
				fieldRow('csqtt-ed-workers', _('Workers'), f.workers,
					[E('span', {}, _('Number of parallel CSQTT worker channels (9..126). The effective value is rounded to the nearest multiple of 9.')),
						warning(_('Do not set the maximum without measuring throughput, CPU, RAM, temperature and ping.'))]),
				fieldRow('csqtt-ed-turn', _('TURN transport'), f.turn_transport,
					_('Transport affects compatibility, latency and connection behaviour. Use a mode supported by the server (tcp is stored as tcp_tls).')),
				fieldRow('csqtt-ed-obfs', _('Obfuscation (OBFS)'), f.obfs,
					_('Obfuscation profile used by the connection (audio or video).')),
			]),
			section('CAPTCHA', [
				fieldRow('csqtt-ed-capmode', _('CAPTCHA mode'), f.captcha_mode,
					_('How CAPTCHA is solved: auto, wv (WebView) or rjs (Rust JS).')),
				fieldRow('csqtt-ed-cppol', _('CAPTCHA policy'), f.captcha_policy,
					overrideHint(gv('captcha_policy', 'failover'))),
			]),
			section(_('Identification / compatibility'), [
				fieldRow('csqtt-ed-fp', _('Fingerprint'), f.fingerprint,
					_('TLS fingerprint used by the client: chrome or firefox.')),
				fieldRow('csqtt-ed-clientids', _('Client IDs'), f.client_ids,
					_('VK client IDs (advanced; leave empty to use defaults).')),
				fieldRow('csqtt-ed-devid', _('Device ID'), f.device_id,
					warning(_('Device ID is bound to the existing access/password. Do not change or regenerate without unbinding it on the server.'))),
			]),
			section(_('Failover overrides'), [
				fieldRow('csqtt-ed-ft', _('Failure threshold (override)'), f.fail_threshold, overrideHint(gv('fail_threshold', '3'))),
				fieldRow('csqtt-ed-st', _('Success threshold (override)'), f.success_threshold, overrideHint(gv('success_threshold', '2'))),
				fieldRow('csqtt-ed-cd', _('Cooldown (override)'), f.cooldown,
					cooldownHuman
						? _('Empty = use the global setting (%s · %s).').format('%s %s'.format(gv('cooldown', '60'), _('sec')), cooldownHuman)
						: overrideHint('%s %s'.format(gv('cooldown', '60'), _('sec')))),
			]),
			section(_('Advanced'), [
				fieldRow('csqtt-ed-vkauth', _('VK auth mode'), f.vk_auth_mode,
					_('VK auth path: vkcalls (default) or auto_js.')),
				fieldRow('csqtt-ed-vkhash', _('VK hash mode'), f.vk_hash_mode,
					_('How VK hashes are obtained: manual (from the VK field) or auto_js. auto_js also needs VK auth mode auto_js and a VK JS token.')),
				fieldRow('csqtt-ed-vktok', _('VK JS token'), f.vk_js_token,
					_('Secret used by auto_js mode. Accepts the access token itself or the full OAuth redirect URL containing access_token. Never shown; an empty field keeps the stored value.')),
			]),
			section(_('Note'), [
				fieldRow('csqtt-ed-note', _('Note'), f.note, _('User note. Does not affect the connection.')),
			]),
			E('div', { class: 'right' }, [
				saveBtn,
				' ',
				rowBtn(_('Cancel'), 'negative', ui.hideModal),
			]),
		]);
	},
});
