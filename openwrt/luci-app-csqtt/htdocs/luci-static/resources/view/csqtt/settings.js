'use strict';
'require view';
'require rpc';
'require uci';
'require ui';

// [star-panel] Глобальные настройки CSQTT (секция main).
// Категории: Основные, Переключение и восстановление, Проверка соединения,
// CAPTCHA, Туннель и сеть, Журналирование, Диагностика. На широком экране —
// вертикальное меню слева, на узком — select над формой. Все панели создаются
// один раз и лишь скрываются, поэтому переходы между категориями сохраняют
// несохранённые значения. Подписи — над полями; связанные поля объединяются в
// две колонки при достаточной ширине. Зависимые поля включаются по режиму.
// Панель сохранения не перекрывает форму и отражает состояния: несохранённые
// изменения / сохранение / сохранено / ошибка. UCI option names, единицы
// хранения и backend-семантика (uci.save+uci.apply, test_conf) не менялись.

const callTestConf = rpc.declare({ object: 'csqtt', method: 'test_conf' });

const BRAND = 'star-panel-csqtt';

const CSS = [
	'.csqtt{width:100%;max-width:1080px;box-sizing:border-box;line-height:1.45;}',
	'.csqtt *,.csqtt *::before,.csqtt *::after{box-sizing:border-box;}',
	'.csqtt-brand{display:flex;flex-wrap:wrap;align-items:baseline;gap:.55em;margin:0 0 .9em;padding:0 0 .6em;border-bottom:1px solid rgba(127,127,127,.3);}',
	'.csqtt-brand-name{font-size:1.3em;font-weight:700;}',
	'.csqtt-brand-tag{font-size:.85em;opacity:.7;}',
	'.csqtt-cats{display:flex;gap:16px;align-items:flex-start;}',
	'.csqtt-catlist{display:flex;flex-direction:column;gap:4px;flex:0 0 14em;position:sticky;top:8px;}',
	'.csqtt-catselect{display:none;}',
	'.csqtt-panels{flex:1 1 auto;min-width:0;}',
	'.csqtt button.btn,.csqtt .cbi-button{height:2.4em;min-height:2.4em;line-height:1;display:inline-flex;align-items:center;justify-content:center;gap:.35em;padding:.35em .9em;margin:0;border:1px solid rgba(127,127,127,.38);border-radius:8px;background:rgba(127,127,127,.07);color:inherit;font:inherit;font-size:.92em;cursor:pointer;text-decoration:none;white-space:nowrap;}',
	'.csqtt button.btn:hover{border-color:#4a90e2;}',
	'.csqtt button.btn:focus-visible,.csqtt .cbi-button:focus-visible{outline:2px solid #4a90e2;outline-offset:2px;}',
	'.csqtt button.btn.primary{background:#2f7fd6;border-color:transparent;color:#fff;}',
	'.csqtt button.btn[disabled]{opacity:.5;cursor:default;}',
	'.csqtt button.cat{justify-content:flex-start;width:100%;text-align:left;border-color:transparent;background:transparent;}',
	'.csqtt button.cat.active{background:rgba(74,144,226,.16);border-color:rgba(74,144,226,.5);font-weight:600;}',
	'.csqtt-panel{min-width:0;}',
	'.csqtt-panel-head{margin:0 0 14px;padding:0 0 10px;border-bottom:1px solid rgba(127,127,127,.3);}',
	'.csqtt-panel-title{font-size:1.05em;font-weight:700;margin:0 0 4px;}',
	'.csqtt-panel-desc{margin:0;font-size:13px;opacity:.75;overflow-wrap:anywhere;}',
	'.csqtt-fields{display:grid;grid-template-columns:repeat(auto-fit,minmax(260px,1fr));gap:14px 20px;align-items:start;}',
	'.csqtt-row{min-width:0;display:flex;flex-direction:column;gap:4px;}',
	'.csqtt-row-wide{grid-column:1/-1;}',
	'.csqtt-row-label{font-size:13px;font-weight:600;opacity:.9;overflow-wrap:anywhere;}',
	'.csqtt-row-control{min-width:0;display:flex;flex-direction:column;gap:4px;}',
	'.csqtt input.cbi-input-text,.csqtt select.cbi-select{min-height:2.4em;height:2.4em;padding:.3em .55em;border:1px solid rgba(127,127,127,.38);border-radius:8px;background:rgba(127,127,127,.05);color:inherit;font:inherit;font-size:14px;width:100%;max-width:100%;}',
	'.csqtt input[type=checkbox].cbi-checkbox{width:1.15em;height:1.15em;}',
	'.csqtt input:focus-visible,.csqtt select:focus-visible{outline:2px solid #4a90e2;outline-offset:1px;}',
	'.csqtt-inputline{display:flex;align-items:center;flex-wrap:wrap;gap:8px;}',
	'.csqtt-inputline input.cbi-input-text{width:auto;max-width:10em;}',
	'.csqtt-unit{font-size:13px;opacity:.8;white-space:nowrap;}',
	'.csqtt-human{display:block;font-size:12px;opacity:.7;}',
	'.csqtt-desc{font-size:12.5px;opacity:.72;line-height:1.4;overflow-wrap:anywhere;}',
	'.csqtt-warn{font-size:12.5px;color:#e06c6c;overflow-wrap:anywhere;}',
	'.csqtt-invalid input,.csqtt-invalid select{border-color:#e06c6c;}',
	'.csqtt-actions{display:flex;flex-wrap:wrap;gap:8px;margin:4px 0;}',
	'.csqtt-badge{display:inline-block;border:1px solid rgba(127,127,127,.35);border-radius:1em;padding:.05em .6em;font-size:.82em;white-space:nowrap;background:rgba(127,127,127,.12);color:inherit;}',
	'.csqtt-badge.success{background:rgba(63,157,88,.18);border-color:rgba(63,157,88,.6);}',
	'.csqtt-badge.warning{background:rgba(224,164,74,.2);border-color:rgba(224,164,74,.65);}',
	'.csqtt-badge.danger{background:rgba(217,83,79,.18);border-color:rgba(217,83,79,.6);}',
	'.csqtt-switch{position:relative;display:inline-flex;align-items:center;gap:.45em;cursor:pointer;user-select:none;white-space:nowrap;font-size:14px;min-height:2.2em;}',
	'.csqtt-switch input{position:absolute;inset:0;width:100%;height:100%;margin:0;opacity:0;cursor:pointer;}',
	'.csqtt-switch .track{width:2.4em;height:1.3em;border-radius:1em;background:rgba(127,127,127,.4);position:relative;flex:0 0 auto;transition:background .15s;}',
	'.csqtt-switch .track::after{content:"";position:absolute;top:.16em;left:.16em;width:.98em;height:.98em;border-radius:50%;background:#fff;transition:left .15s;}',
	'.csqtt-switch input:checked + .track{background:#3f9d58;}',
	'.csqtt-switch input:checked + .track::after{left:1.26em;}',
	'.csqtt-switch input:focus-visible + .track{outline:2px solid #4a90e2;outline-offset:2px;}',
	'.csqtt-switch input:disabled + .track{opacity:.5;}',
	'.csqtt-savebar{display:flex;flex-wrap:wrap;align-items:center;gap:12px;margin:16px 0 0;padding:12px;border:1px solid rgba(127,127,127,.3);border-radius:10px;background:rgba(127,127,127,.05);}',
	'.csqtt-save-state{font-size:13px;margin-left:auto;color:inherit;border:1px solid rgba(127,127,127,.4);background:rgba(127,127,127,.1);border-radius:1em;padding:.1em .7em;}',
	'.csqtt-save-state.dirty{border-color:rgba(224,164,74,.7);background:rgba(224,164,74,.16);}',
	'.csqtt-save-state.saving{border-color:rgba(74,144,226,.6);background:rgba(74,144,226,.14);}',
	'.csqtt-save-state.saved{border-color:rgba(63,157,88,.65);background:rgba(63,157,88,.15);}',
	'.csqtt-save-state.error{border-color:rgba(217,83,79,.7);background:rgba(217,83,79,.16);}',
	'.csqtt details summary{cursor:pointer;opacity:.8;font-size:13px;}',
	'.csqtt details[open] summary{margin-bottom:.4em;}',
	'.csqtt-note{margin:.4em 0 0;}',
	'.csqtt-note pre{max-width:100%;overflow-x:auto;border:1px solid rgba(127,127,127,.3);border-radius:8px;padding:.5em .7em;}',
	'@media (max-width:760px){',
	' .csqtt-cats{display:block;}',
	' .csqtt-catlist{display:none;}',
	' .csqtt-catselect{display:block;margin:0 0 12px;}',
	' .csqtt-fields{grid-template-columns:1fr;}',
	' .csqtt-save-state{margin-left:0;}',
	'}',
].join('');

function notify(msg, kind) {
	ui.addTimeLimitedNotification('csqtt', E('p', msg), 6000, kind);
}

function val(id) {
	var el = document.getElementById(id);
	return el ? el.value.trim() : '';
}

function chk(id) {
	var el = document.getElementById(id);
	return (el && el.checked) ? '1' : '0';
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

function humanKib(kib) {
	kib = parseInt(kib, 10) || 0;
	var mib = Math.round((kib / 1024) * 100) / 100;
	return '≈ %s MiB'.format(mib);
}

function desc(text) {
	return E('div', { class: 'csqtt-desc' }, text);
}

function warning(text) {
	return E('div', { class: 'csqtt-warn' }, text);
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

function field(id, label, control, hint, wide) {
	var control_kids = [control];
	addHint(control_kids, hint);
	return E('div', { class: 'csqtt-row' + (wide ? ' csqtt-row-wide' : '') }, [
		E('label', { class: 'csqtt-row-label', for: id }, label),
		E('div', { class: 'csqtt-row-control' }, control_kids),
	]);
}

function sel(id, values, current) {
	return E('select', { id: id, class: 'cbi-select' }, values.map(function(v) {
		var vv = (typeof v === 'object') ? v.v : v;
		var lbl = (typeof v === 'object') ? v.l : (v || _('— none —'));
		return E('option', { value: vv, selected: (vv === current) ? 'selected' : null }, lbl);
	}));
}

function boolSwitch(id, current) {
	var on = (current === '1');
	var txt = E('span', { class: 'txt' }, on ? _('Enabled') : _('Disabled'));
	var input = E('input', {
		id: id, type: 'checkbox', class: 'cbi-checkbox',
		checked: on ? 'checked' : null,
		change: function(ev) {
			L.dom.content(txt, ev.target.checked ? _('Enabled') : _('Disabled'));
		},
	});
	return E('label', { class: 'csqtt-switch' }, [
		input,
		E('span', { class: 'track' }),
		txt,
	]);
}

function text(id, current, ph, wide) {
	return E('input', { id: id, type: 'text', class: 'cbi-input-text', value: current, placeholder: ph || '' });
}

function num(id, current, unit, humanFn) {
	var input = E('input', { id: id, type: 'number', class: 'cbi-input-text', value: current });
	var human = E('small', { class: 'csqtt-human' }, humanFn ? humanFn(current) : '');
	if (humanFn)
		input.addEventListener('input', function() {
			L.dom.content(human, humanFn(input.value));
		});
	return E('div', {}, [
		E('span', { class: 'csqtt-inputline' }, [input, unit ? E('span', { class: 'csqtt-unit' }, unit) : '']),
		human,
	]);
}

return view.extend({
	title: BRAND + ' — ' + _('Settings'),

	main: null,
	routingMode: 'auto',
	activeCat: 'general',
	_catBtns: {},
	_panels: {},
	_saveBtn: null,
	_saving: false,
	_diagRunning: false,

	load: function() {
		return uci.load('csqtt').catch(function() { return null; });
	},

	showCat: function(id) {
		this.activeCat = id;
		Object.keys(this._panels).forEach(L.bind(function(k) {
			this._panels[k].style.display = (k === id) ? '' : 'none';
		}, this));
		Object.keys(this._catBtns).forEach(L.bind(function(k) {
			this._catBtns[k].className = 'btn cat' + (k === id ? ' active' : '');
		}, this));
		var selEl = document.getElementById('csqtt-catselect');
		if (selEl && 'value' in selEl)
			selEl.value = id;
	},

	// Включает/выключает зависимые поля по выбранному режиму, не трогая значения.
	syncDeps: function() {
		var selEl = document.getElementById('csqtt-set-selection');
		var ap = document.getElementById('csqtt-set-active');
		var note = document.getElementById('csqtt-set-active-note');
		var manual = !selEl || selEl.value === 'manual';
		if (ap)
			ap.disabled = !manual;
		if (note)
			note.style.display = manual ? 'none' : '';

		var hm = document.getElementById('csqtt-set-hm');
		var ht = document.getElementById('csqtt-set-ht');
		var dataMode = !hm || hm.value === 'data' || hm.value === 'both';
		if (ht)
			ht.disabled = !dataMode;

		var fo = document.getElementById('csqtt-set-failover');
		var fb = document.getElementById('csqtt-set-failback');
		if (fb)
			fb.disabled = !(fo && fo.checked);
	},

	setSaveState: function(state, msg) {
		this._saveState = state;
		var el = document.getElementById('csqtt-dirty');
		if (!el)
			return;
		var def = {
			dirty: _('There are unsaved changes'),
			saving: _('Saving…'),
			saved: _('All changes saved'),
			error: _('Save error'),
		};
		el.className = 'csqtt-save-state ' + state;
		L.dom.content(el, msg != null ? msg : (def[state] || ''));
	},

	markDirty: function(on) {
		if (on)
			this.setSaveState('dirty');
		else
			this.setSaveState('saved');
	},

	// Есть ли фактические отличия от загруженного конфига. Без этого кнопка
	// «Сохранить» без изменений вызывала uci.apply, а rpcd возвращает NO_DATA
	// (code 5) при отсутствии изменений — пользователь видел ложную ошибку.
	hasChanges: function(data) {
		var m = this.main || {};
		return Object.keys(data).some(function(k) {
			var cur = (m[k] != null) ? String(m[k]) : '';
			return String(data[k]) !== cur;
		});
	},

	mount: function() {
		this.syncDeps();
	},

	render: function() {
		var m = {}, self = this;
		try {
			m = uci.get('csqtt', 'main') || {};
			self.routingMode = (uci.get('csqtt', 'routing', 'mode') || 'auto');
		} catch (e) {
			m = {};
		}
		this.main = m;
		var g = function(k, d) { return (m[k] != null) ? String(m[k]) : (d || ''); };
		var profileOpts = [];
		try {
			profileOpts = uci.sections('csqtt', 'server').map(function(s) {
				var id = s['.name'];
				var name = s.name || id;
				return { v: id, l: (name && name !== id) ? '%s (%s)'.format(name, id) : id };
			});
		} catch (e) {}

		function catButton(id, label) {
			var b = E('button', { class: 'btn cat' + (id === self.activeCat ? ' active' : ''), click: L.bind(self.showCat, self, id) }, label);
			self._catBtns[id] = b;
			return b;
		}
		function panelEl(id, title, descText, body) {
			var p = E('div', { class: 'csqtt-panel', 'data-panel': id, style: (id === self.activeCat ? '' : 'display:none') }, [
				E('div', { class: 'csqtt-panel-head' }, [
					E('h3', { class: 'csqtt-panel-title' }, title),
					E('p', { class: 'csqtt-panel-desc' }, descText),
				]),
			].concat(body));
			self._panels[id] = p;
			return p;
		}
		function fields(rows) {
			return E('div', { class: 'csqtt-fields' }, rows);
		}

		var cats = [
			['general', _('General')],
			['failover', _('Failover and recovery')],
			['health', _('Connection health check')],
			['captcha', 'CAPTCHA'],
			['tunnel', _('Tunnel and network')],
			['log', _('Logging')],
			['diagnostics', _('Diagnostics')],
		];

		var catList = E('div', { class: 'csqtt-catlist' }, cats.map(function(c) { return catButton(c[0], c[1]); }));
		var catSelect = E('select', { id: 'csqtt-catselect', class: 'cbi-select csqtt-catselect',
			change: function() { self.showCat(this.value); } },
			cats.map(function(c) { return E('option', { value: c[0], selected: (c[0] === self.activeCat) ? 'selected' : null }, c[1]); }));

		var pGeneral = panelEl('general', _('General'), _('Turn the service on and choose how the working profile is selected.'), [
			fields([
				field('csqtt-set-enabled', _('CSQTT enabled'), boolSwitch('csqtt-set-enabled', g('enabled', '0')),
					desc(_('Start the tunnel automatically with the service.'))),
				field('csqtt-set-selection', _('Profile selection mode'),
					sel('csqtt-set-selection', [{ v: 'priority', l: _('Priority') }, { v: 'manual', l: _('Manual') }], g('selection_mode', 'priority')),
					desc(_('Priority: CSQTT automatically picks the best available enabled profile. Manual: only the selected active profile is used.'))),
				field('csqtt-set-active', _('Active profile'),
					sel('csqtt-set-active', [{ v: '', l: _('— none —') }].concat(profileOpts), g('active_profile')),
					[desc(_('Profile used in manual mode.')),
						E('span', { id: 'csqtt-set-active-note', class: 'csqtt-desc' }, _('Used only in manual selection mode.'))]),
			]),
		]);

		var pFailover = panelEl('failover', _('Failover and recovery'), _('How CSQTT switches between profiles and recovers after failures.'), [
			fields([
				field('csqtt-set-failover', _('Automatic failover'), boolSwitch('csqtt-set-failover', g('failover', '1')),
					desc(_('On problems with the current profile CSQTT can switch to the next available profile. Works in priority selection mode.'))),
				field('csqtt-set-failback', _('Automatic failback'), boolSwitch('csqtt-set-failback', g('failback', '0')),
					desc(_('After a higher-priority profile recovers, CSQTT can automatically return to it.'))),
				field('csqtt-set-hint', _('Health check interval'), num('csqtt-set-hint', g('health_interval', '5'), _('sec'), humanSeconds),
					desc(_('How often CSQTT checks the connection state.'))),
				field('csqtt-set-ft', _('Failure threshold'), num('csqtt-set-ft', g('fail_threshold', '3'), _('times'), null),
					desc(_('How many consecutive failed checks are required before a profile is considered unhealthy.'))),
				field('csqtt-set-st', _('Success threshold'), num('csqtt-set-st', g('success_threshold', '2'), _('times'), null),
					desc(_('How many successful checks are required to confirm the profile works stably again.'))),
				field('csqtt-set-cd', _('Cooldown after failure'), num('csqtt-set-cd', g('cooldown', '60'), _('sec'), humanSeconds),
					desc(_('After a failure the profile is not used for this time before the next attempt.'))),
				field('csqtt-set-rd', _('Reconnect delay'), num('csqtt-set-rd', g('reconnect_delay', '5'), _('sec'), humanSeconds),
					desc(_('Pause between repeated connection attempts.'))),
				field('csqtt-set-fst', _('Failback stable time'), num('csqtt-set-fst', g('failback_stable_time', '60'), _('sec'), humanSeconds),
					desc(_('How long a higher-priority profile must stay stable before automatic failback.'))),
			]),
		]);

		var pHealth = panelEl('health', _('Connection health check'), _('How the connection state is checked.'), [
			fields([
				field('csqtt-set-hm', _('Health check mode'),
					sel('csqtt-set-hm', [
						{ v: 'transport', l: _('Transport') },
						{ v: 'data', l: _('Data transfer') },
						{ v: 'both', l: _('Transport + data') },
					], g('health_mode', 'both')),
					desc(_('Transport: checks connection, workers and control-plane state. Data: evaluates actual traffic signs. Transport + data: both checks (recommended production mode).'))),
				field('csqtt-set-ht', _('Additional probe target'),
					text('csqtt-set-ht', g('health_target'), _('IP, host or address (optional)')),
					[desc(_('If empty, internal connection-state signals are used. The target is only an extra check.')),
						warning(_('A successful probe alone does not make a profile ACTIVE.'))], true),
			]),
		]);

		var pCaptcha = panelEl('captcha', 'CAPTCHA', _('What happens when a profile needs CAPTCHA.'), [
			fields([
				field('csqtt-set-cppol', _('CAPTCHA policy'),
					sel('csqtt-set-cppol', [
						{ v: 'failover', l: _('Switch to another profile') },
						{ v: 'wait', l: _('Wait for CAPTCHA to be solved') },
					], g('captcha_policy', 'failover')),
					desc(_('What to do when a profile requires CAPTCHA solving.'))),
			]),
		]);

		var pTunnel = panelEl('tunnel', _('Tunnel and network'), _('Tunnel interface parameters. CSQTT stays interface-only.'), [
			fields([
				field('csqtt-set-tunaddr', _('TUN address'),
					text('csqtt-set-tunaddr', g('tun_address'), _('IP or CIDR (optional)')),
					desc(_('Address of the csqtt0 interface. Empty means use the address from the server.'))),
				field('csqtt-set-mtu', _('TUN MTU'),
					num('csqtt-set-mtu', g('tun_mtu', '1280'), _('bytes'), null),
					[desc(_('Maximum IP packet size of the csqtt0 interface. A larger MTU is not always faster.')),
						warning(_('Change only when diagnosing MTU/fragmentation problems.'))]),
				field('csqtt-set-dns', _('Tunnel DNS'),
					text('csqtt-set-dns', g('dns'), _('e.g. 77.88.8.8,77.88.8.1')),
					desc(_('DNS for the tunnel connection. CSQTT does not change the system DNS of OpenWrt or dnsmasq.')), true),
			]),
		]);

		var pLog = panelEl('log', _('Logging'), _('Log detail and size.'), [
			fields([
				field('csqtt-set-ll', _('Log level'),
					sel('csqtt-set-ll', [
						{ v: 'error', l: _('Error — errors only') },
						{ v: 'warn', l: _('Warning — errors and warnings') },
						{ v: 'info', l: _('Info — normal operation') },
						{ v: 'debug', l: _('Debug — detailed diagnostics') },
					], g('log_level', 'info')),
					[desc(_('Higher levels write more detail to the log.')),
						warning(_('Debug creates more log entries and may increase load.'))]),
				field('csqtt-set-lf', _('Log file'),
					text('csqtt-set-lf', g('log_file', '/var/log/csqtt.log'), ''),
					desc(_('Path to the local CSQTT log file.')), true),
				field('csqtt-set-ls', _('Maximum log size'), num('csqtt-set-ls', g('log_size_kb', '512'), 'KiB', humanKib),
					desc(_('When the size is reached the existing log rotation is used.'))),
			]),
		]);

		var diagBtn = E('button', { class: 'btn', click: L.bind(this.runDiagnostics, this) }, _('Run configuration check'));
		var pDiag = panelEl('diagnostics', _('Diagnostics'), _('Read-only checks and the isolation model.'), [
			E('div', { class: 'csqtt-fields' }, [
				E('div', { class: 'csqtt-row' }, [
					E('span', { class: 'csqtt-row-label' }, 'routing.mode'),
					E('div', { class: 'csqtt-row-control' }, [
						E('span', { class: 'csqtt-badge' }, self.routingMode),
						desc(_('auto = interface-only (M3X): the daemon never installs routes/rules/tables and never changes WAN default, system DNS or firewall/NAT. There is no “capture whole LAN” mode by design.')),
					]),
				]),
			]),
			E('details', { class: 'csqtt-details' }, [
				E('summary', {}, _('Isolation note (csqtt0)')),
				E('p', { class: 'csqtt-desc' }, _('csqtt0 is an isolated egress for a user-configured proxy only. Traffic enters the tunnel solely for processes explicitly bound to the interface (SO_BINDTODEVICE). Control-plane CSQTT/VK/TURN stays on WAN.')),
				E('pre', { class: 'logview' },
					'# Mihomo / mihomo proxy example:\n' +
					'proxies:\n' +
					'  - name: CSQTT\n' +
					'    type: direct\n' +
					'    interface-name: csqtt0\n' +
					'    udp: true\n'),
			]),
			E('div', { class: 'csqtt-actions' }, [diagBtn]),
			E('div', { id: 'csqtt-set-report' }),
		]);

		this._panels = { general: pGeneral, failover: pFailover, health: pHealth, captcha: pCaptcha, tunnel: pTunnel, log: pLog, diagnostics: pDiag };

		var saveBtn = E('button', { class: 'btn primary', click: L.bind(this.saveSettings, this) }, _('Save & apply'));
		this._saveBtn = saveBtn;

		var nodes = E('div', { class: 'csqtt csqtt-settings' }, [
			E('style', {}, CSS),
			E('div', { class: 'csqtt-brand' }, [
				E('span', { class: 'csqtt-brand-name' }, BRAND),
				E('span', { class: 'csqtt-brand-tag' }, _('Global settings')),
			]),
			catSelect,
			E('div', { class: 'csqtt-cats' }, [
				catList,
				E('div', { class: 'csqtt-panels' }, [pGeneral, pFailover, pHealth, pCaptcha, pTunnel, pLog, pDiag]),
			]),
			E('div', { class: 'csqtt-savebar' }, [
				saveBtn,
				E('span', { id: 'csqtt-dirty', class: 'csqtt-save-state saved' }, _('All changes saved')),
			]),
		]);

		// Любое изменение поля: «есть несохранённые изменения» + актуализация
		// зависимых полей. Значения при этом не сбрасываются.
		nodes.addEventListener('input', function() { self.markDirty(true); });
		nodes.addEventListener('change', function() { self.markDirty(true); self.syncDeps(); });

		// Первичное применение зависимых полей после вставки в DOM (mount не
		// гарантирован во всех версиях LuCI). Значения полей не изменяются.
		window.setTimeout(function() { self.syncDeps(); }, 0);

		return nodes;
	},

	collect: function() {
		return {
			enabled: chk('csqtt-set-enabled'),
			selection_mode: val('csqtt-set-selection'),
			active_profile: val('csqtt-set-active'),
			failover: chk('csqtt-set-failover'),
			failback: chk('csqtt-set-failback'),
			health_interval: val('csqtt-set-hint') || '5',
			fail_threshold: val('csqtt-set-ft') || '3',
			success_threshold: val('csqtt-set-st') || '2',
			cooldown: val('csqtt-set-cd') || '60',
			reconnect_delay: val('csqtt-set-rd') || '5',
			failback_stable_time: val('csqtt-set-fst') || '60',
			health_mode: val('csqtt-set-hm'),
			health_target: val('csqtt-set-ht'),
			captcha_policy: val('csqtt-set-cppol'),
			tun_address: val('csqtt-set-tunaddr'),
			tun_mtu: val('csqtt-set-mtu') || '1280',
			dns: val('csqtt-set-dns'),
			log_level: val('csqtt-set-ll'),
			log_file: val('csqtt-set-lf') || '/var/log/csqtt.log',
			log_size_kb: val('csqtt-set-ls') || '512',
		};
	},

	// Клиентская проверка в дополнение к backend test_conf; ошибки — у поля.
	validate: function(data) {
		var errors = [];
		var mtu = parseInt(data.tun_mtu, 10);
		if (!(mtu >= 576 && mtu <= 65535)) {
			errors.push([_('TUN MTU'), _('Value must be between %s and %s.').format(576, 65535)]);
			this.flagInvalid('csqtt-set-mtu', true);
		} else {
			this.flagInvalid('csqtt-set-mtu', false);
		}
		var ls = parseInt(data.log_size_kb, 10);
		if (!(ls >= 16 && ls <= 65536)) {
			errors.push([_('Maximum log size'), _('Value must be between %s and %s.').format(16, 65536)]);
			this.flagInvalid('csqtt-set-ls', true);
		} else {
			this.flagInvalid('csqtt-set-ls', false);
		}
		return errors;
	},

	flagInvalid: function(id, on) {
		var el = document.getElementById(id);
		var field = el && el.closest ? el.closest('.csqtt-row-control') : null;
		if (field)
			field.className = on ? 'csqtt-row-control csqtt-invalid' : 'csqtt-row-control';
	},

	showReport: function(rep) {
		var box = document.getElementById('csqtt-set-report');
		if (!box)
			return;
		if (!rep) {
			L.dom.content(box, E('div', { class: 'csqtt-desc csqtt-warn' }, _('Diagnostics failed')));
			return;
		}
		var checks = (rep.report && rep.report.checks) || [];
		L.dom.content(box, E('div', {}, [
			E('h4', {}, rep.ok ? _('Diagnostics complete') : _('Diagnostics found issues')),
		].concat(checks.map(function(c) {
			var kind = c.status === 'ok' ? 'success'
				: (c.status === 'warn' ? 'warning'
					: (c.status === 'fail' ? 'danger' : ''));
			return E('div', {}, [
				E('span', { class: 'csqtt-badge %s'.format(kind) }, (c.status || '?').toUpperCase()),
				' ', E('b', c.name), c.message ? E('small', ' — ' + c.message) : '',
			]);
		}))));
	},

	runDiagnostics: function(ev) {
		var self = this, btn = (ev && ev.currentTarget) || null;
		if (this._diagRunning)
			return;
		this._diagRunning = true;
		if (btn)
			btn.disabled = true;
		callTestConf().then(function(rep) {
			self.showReport(rep);
		}).catch(function() {
			self.showReport(null);
		}).then(function() {
			self._diagRunning = false;
			if (btn)
				btn.disabled = false;
		});
	},

	saveSettings: function(ev) {
		var self = this, btn = (ev && ev.currentTarget) || this._saveBtn;
		if (this._saving)
			return;
		var data = this.collect();
		var errors = this.validate(data);
		if (errors.length) {
			notify(_('Fix the highlighted fields: %s').format(errors.map(function(e) { return e[0]; }).join(', ')), 'error');
			this.showCat('tunnel');
			this.setSaveState('error', _('Save error'));
			return;
		}
		// Нет фактических изменений — не дёргаем uci.apply (см. hasChanges).
		if (!this.hasChanges(data)) {
			this.setSaveState('saved');
			return;
		}
		try {
			Object.keys(data).forEach(function(k) {
				uci.set('csqtt', 'main', k, data[k]);
			});
		} catch (e) {
			notify(_('uci write error: %s').format(e), 'error');
			this.setSaveState('error', _('Save error'));
			return;
		}
		this._saving = true;
		if (btn)
			btn.disabled = true;
		this.setSaveState('saving');
		uci.save().then(function() { return uci.apply(); }).then(function() {
			self.main = Object.assign({}, self.main, data);
			self.setSaveState('saved');
			notify(_('settings saved'), 'info');
			return callTestConf().then(function(rep) {
				self.showReport(rep);
			}).catch(function() {
				self.showReport(null);
			});
		}).catch(function() {
			self.setSaveState('error', _('Save error'));
			notify(_('uci commit failed (write access required) or rpcd error'), 'error');
		}).then(function() {
			self._saving = false;
			if (btn)
				btn.disabled = false;
			self.syncDeps();
		});
	},

	handleSave: null,
	handleReset: null,
	handleSaveApply: null,
});
