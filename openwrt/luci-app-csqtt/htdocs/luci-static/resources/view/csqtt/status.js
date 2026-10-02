'use strict';
'require view';
'require rpc';
'require uci';
'require ui';

// [star-panel] Вью статуса CSQTT (ubus-объект "csqtt").
// Информация выстроена по важности: общий статус соединения → активный профиль
// и время работы → группа кнопок службы → компактные карточки метрик →
// отдельный блок технических сведений → заметная последняя ошибка → таблица
// профилей. Цвет всегда дополнен текстом. RPC/polling/схема не менялись.

const callStatus  = rpc.declare({ object: 'csqtt', method: 'status' });
const callService = rpc.declare({ object: 'csqtt', method: 'service', params: ['action'] });

const BRAND = 'star-panel-csqtt';

const CSS = [
	'.csqtt{width:100%;max-width:1080px;box-sizing:border-box;line-height:1.45;}',
	'.csqtt *,.csqtt *::before,.csqtt *::after{box-sizing:border-box;}',
	'.csqtt-brand{display:flex;flex-wrap:wrap;align-items:baseline;gap:.55em;margin:0 0 .9em;padding:0 0 .6em;border-bottom:1px solid rgba(127,127,127,.3);}',
	'.csqtt-brand-name{font-size:1.3em;font-weight:700;}',
	'.csqtt-brand-tag{font-size:.85em;opacity:.7;}',
	'.csqtt-hero{display:flex;flex-wrap:wrap;align-items:center;gap:.8em 1.4em;padding:.9em 1em;border:1px solid rgba(127,127,127,.32);border-radius:12px;background:rgba(127,127,127,.06);margin:0 0 1em;}',
	'.csqtt-hero-state{display:flex;flex-direction:column;gap:.15em;min-width:11em;}',
	'.csqtt-hero-label{font-size:.75em;text-transform:uppercase;letter-spacing:.05em;opacity:.6;}',
	'.csqtt-hero-value{font-size:1.5em;font-weight:700;line-height:1.1;}',
	'.csqtt-hero-meta{display:flex;flex-wrap:wrap;gap:.4em 1.4em;font-size:.9em;}',
	'.csqtt-hero-meta .k{opacity:.62;}',
	'.csqtt-hero-meta .v{font-weight:600;}',
	'.csqtt-hero-actions{display:flex;flex-wrap:wrap;gap:.5em;margin-left:auto;}',
	'.csqtt-indicators{display:flex;flex-wrap:wrap;align-items:center;gap:.4em 1.4em;font-size:.9em;margin:-.3em 0 1em;}',
	'.csqtt-indicators .k{opacity:.62;margin-right:.35em;}',
	'.csqtt-grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(170px,1fr));gap:10px;margin:0 0 1em;align-items:stretch;}',
	'.csqtt-card{border:1px solid rgba(127,127,127,.3);border-radius:10px;padding:.7em .85em;min-height:4.6em;display:flex;flex-direction:column;gap:.2em;background:rgba(127,127,127,.05);}',
	'.csqtt-card-title{font-size:.74em;text-transform:uppercase;letter-spacing:.04em;opacity:.6;margin:0 0 .3em;min-height:1.1em;}',
	'.csqtt-main{font-size:1.02em;font-weight:600;line-height:1.35;overflow-wrap:anywhere;}',
	'.csqtt-sub{font-size:.84em;line-height:1.4;opacity:.78;overflow-wrap:anywhere;}',
	'.csqtt-sec-title{font-size:.98em;font-weight:700;margin:.4em 0 .6em;padding-bottom:.3em;border-bottom:1px solid rgba(127,127,127,.3);}',
	'.csqtt button.btn,.csqtt a.btn,.csqtt .cbi-button{height:2.35em;min-height:2.35em;line-height:1;display:inline-flex;align-items:center;justify-content:center;gap:.35em;padding:.35em .9em;margin:0;border:1px solid rgba(127,127,127,.38);border-radius:8px;background:rgba(127,127,127,.07);color:inherit;font:inherit;font-size:.92em;cursor:pointer;text-decoration:none;white-space:nowrap;}',
	'.csqtt button.btn:hover,.csqtt a.btn:hover{border-color:#4a90e2;}',
	'.csqtt button.btn.primary{background:#2f7fd6;border-color:transparent;color:#fff;}',
	'.csqtt button.btn[disabled]{opacity:.45;cursor:default;}',
	'.csqtt-badge{display:inline-block;border:1px solid rgba(127,127,127,.35);border-radius:1em;padding:.08em .7em;font-size:.85em;white-space:nowrap;}',
	'.csqtt-badge.success{color:#79c98a;border-color:rgba(121,201,138,.5);}',
	'.csqtt-badge.warning{color:#e0a44a;border-color:rgba(224,164,74,.5);}',
	'.csqtt-badge.danger{color:#e06c6c;border-color:rgba(224,108,108,.5);}',
	'.csqtt-error{font-size:.86em;line-height:1.4;overflow-wrap:anywhere;max-height:7em;overflow:auto;color:#e06c6c;}',
	'.csqtt-error-box{border:1px solid rgba(224,108,108,.5);background:rgba(224,108,108,.08);border-radius:10px;padding:.6em .8em;margin:0 0 1em;}',
	'.csqtt-ok-line{font-size:.86em;opacity:.7;margin:0 0 1em;}',
	'.csqtt-tech{display:grid;grid-template-columns:repeat(auto-fit,minmax(220px,1fr));gap:10px;margin:0 0 1em;}',
	'.csqtt-table{width:100%;border-collapse:separate;border-spacing:0;}',
	'.csqtt-table th{font-size:.76em;text-transform:uppercase;letter-spacing:.03em;opacity:.62;font-weight:600;text-align:left;padding:.5em .6em;border-bottom:1px solid rgba(127,127,127,.3);white-space:nowrap;}',
	'.csqtt-table td{padding:.5em .6em;border-bottom:1px solid rgba(127,127,127,.14);vertical-align:middle;}',
	'.csqtt-table tr.csqtt-row-active td{background:rgba(74,144,226,.10);}',
	'.csqtt-id{font-family:ui-monospace,Menlo,Consolas,monospace;font-size:.82em;opacity:.65;}',
	'.csqtt-name{font-weight:600;}',
	'.csqtt-tablewrap{width:100%;overflow-x:auto;}',
	'.csqtt-empty{opacity:.65;font-size:.9em;}',
	'@media (max-width:560px){.csqtt-grid{grid-template-columns:1fr 1fr;}.csqtt-hero-actions{width:100%;margin-left:0;}.csqtt-tech{grid-template-columns:1fr;}}',
	'@media (max-width:380px){.csqtt-grid{grid-template-columns:1fr;}}',
].join('');

function fmtUptime(secs) {
	secs = secs || 0;
	var d = Math.floor(secs / 86400),
	    h = Math.floor((secs % 86400) / 3600),
	    m = Math.floor((secs % 3600) / 60),
	    s = secs % 60;
	if (d > 0)
		return '%d %s %d %s'.format(d, _('d'), h, _('h'));
	if (h > 0)
		return '%d %s %d %s'.format(h, _('h'), m, _('min'));
	return '%d %s %d %s'.format(m, _('min'), s, _('sec'));
}

function stateLabel(state) {
	switch (state) {
		case 'active': return _('Active');
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

function fmtBytes(n) {
	n = n || 0;
	var u = ['B', 'KiB', 'MiB', 'GiB', 'TiB'], i = 0;
	while (n >= 1024 && i < u.length - 1) { n /= 1024; i++; }
	return (i === 0 ? '%d %s'.format(n, u[i]) : '%.1f %s'.format(n, u[i]));
}

function badge(text, kind) {
	return E('span', { class: 'csqtt-badge %s'.format(kind || '') }, text);
}

function mainLine(content) {
	return E('div', { class: 'csqtt-main' }, content);
}

function subLine(content) {
	return E('div', { class: 'csqtt-sub' }, content);
}

function card(title, id) {
	return E('div', { class: 'csqtt-card' }, [
		E('div', { class: 'csqtt-card-title' }, title),
		E('div', { id: id }, E('em', _('loading…'))),
	]);
}

function setNode(id, nodes) {
	var n = document.getElementById(id);
	if (n)
		L.dom.content(n, nodes);
}

function splitDns(dns) {
	if (!dns)
		return { dns: '', transport: '' };
	var parts = String(dns).split(':');
	var addr = parts.shift() || '';
	var transport = parts.length ? parts[parts.length - 1] : '';
	return { dns: addr.split(',').join(', '), transport: transport };
}

function notify(msg, kind) {
	ui.addTimeLimitedNotification('csqtt', E('p', msg), 5000, kind);
}

return view.extend({
	title: BRAND,

	failover: false,
	peers: {},
	names: {},
	_svc: {},

	load: function() {
		return uci.load('csqtt').catch(function() { return null; });
	},

	handleService: function(action) {
		callService(action)
			.then(L.bind(function(res) {
				if (!res || !res.ok)
					notify(_('Service action %s failed').format(action), 'error');
				this.refresh();
			}, this))
			.catch(function() {
				notify(_('Permission denied or rpcd error'), 'error');
			});
	},

	refresh: function() {
		var timeout = new Promise(function(_, reject) {
			window.setTimeout(function() { reject(new Error('csqtt status timeout')); }, 12000);
		});
		return Promise.race([callStatus(), timeout])
			.then(L.bind(this.update, this))
			.catch(L.bind(this.setUnavailable, this));
	},

	setUnavailable: function() {
		setNode('csqtt-state', badge(_('RPC error'), 'danger'));
		['csqtt-indicators', 'csqtt-uptime', 'csqtt-active', 'csqtt-tunnel', 'csqtt-traffic',
			'csqtt-workers', 'csqtt-routing', 'csqtt-captcha', 'csqtt-error']
			.forEach(function(id) { setNode(id, subLine('—')); });
	},

	startRefresh: function() {
		var self = this;
		this._pollFn = function() {
			if (!document.getElementById('csqtt-state'))
				return;
			return self.refresh();
		};
		L.Poll.add(this._pollFn, 5);
		window.setTimeout(this._pollFn, 0);
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

	setServiceState: function(running) {
		var s = this._svc || {};
		if (s.start) s.start.disabled = !!running;
		if (s.stop) s.stop.disabled = !running;
		if (s.restart) s.restart.disabled = !running;
	},

	update: function(res) {
		if (res == null || typeof res !== 'object' || (res.running == null && res.code == null)) {
			this.setUnavailable();
			return;
		}
		var st = (res.status && typeof res.status === 'object') ? res.status : {},
		    running = !!res.running,
		    connected = !!res.connected,
		    lastError = st.last_error;

		var stKind, stText;
		if (!running) { stKind = 'danger'; stText = _('Not running'); }
		else if (connected) { stKind = 'success'; stText = _('Connected'); }
		else if (lastError) { stKind = 'danger'; stText = _('Error'); }
		else { stKind = 'warning'; stText = _('Connecting'); }
		setNode('csqtt-state', badge(stText, stKind));
		this.setServiceState(running);

		// Три независимых признака: служба запущена, соединение установлено,
		// трафик реально прошёл (счётчики > 0). «Active»/workers сами по себе
		// не доказывают передачу данных.
		var trafficSeen = ((st.rx_bytes || 0) + (st.tx_bytes || 0)) > 0;
		setNode('csqtt-indicators', [
			E('span', { class: 'k' }, _('Service') + ':'),
			badge(running ? _('running') : _('Not running'), running ? 'success' : 'danger'),
			E('span', { class: 'k' }, _('Connection') + ':'),
			badge(connected ? _('Connected') : _('Not connected'), connected ? 'success' : 'warning'),
			E('span', { class: 'k' }, _('Data transfer') + ':'),
			badge(trafficSeen ? _('Traffic confirmed') : _('No traffic yet'), trafficSeen ? 'success' : ''),
		]);

		setNode('csqtt-uptime', running
			? [mainLine(fmtUptime(st.uptime_secs)), subLine('%s %s'.format(_('Version'), st.version || '?'))]
			: subLine('—'));

		var apId = st.active_profile || '',
		    apName = (this.names && this.names[apId]) || apId || '—',
		    peer = this.peers[apId] || '',
		    activeSub = [];
		if (apId && apName !== apId)
			activeSub.push(apId);
		if (peer)
			activeSub.push(peer);
		setNode('csqtt-active', running
			? [mainLine(apName), activeSub.length ? subLine(activeSub.join(' · ')) : subLine('—')]
			: subLine('—'));

		var t = st.tunnel || {}, d = splitDns(t.dns);
		setNode('csqtt-tunnel', running
			? [
				mainLine(t.interface || '—'),
				subLine('%s · MTU %s'.format(t.address || '—', t.mtu != null ? t.mtu : '—')),
				d.dns ? subLine('DNS %s'.format(d.dns)) : '',
				d.transport ? subLine('%s: %s'.format(_('transport'), d.transport)) : '',
			]
			: subLine('—'));

		setNode('csqtt-traffic', running
			? [mainLine('↓ RX %s'.format(fmtBytes(st.rx_bytes))),
				subLine('↑ TX %s'.format(fmtBytes(st.tx_bytes)))]
			: subLine('—'));

		var w = st.workers || {};
		setNode('csqtt-workers', running
			? [mainLine('%s / %s'.format(w.active != null ? w.active : '—',
					w.configured != null ? w.configured : '—')),
				subLine('%s: %s'.format(_('reconnects'), st.reconnects != null ? st.reconnects : '—'))]
			: subLine('—'));

		var r = st.routing || {};
		setNode('csqtt-routing', [
			mainLine(r.install_routes ? _('Routes installed') : _('Interface-only')),
			subLine(r.mode === 'auto' ? _('Auto') : (r.mode || '—')),
			subLine(_('No routes/rules/tables; WAN, DNS and firewall unchanged.')),
		]);

		if (lastError) {
			setNode('csqtt-error', E('div', { class: 'csqtt-error' }, lastError));
			var eb = document.getElementById('csqtt-error-box');
			if (eb) eb.style.display = '';
		} else {
			setNode('csqtt-error', subLine(_('No errors')));
			var eb2 = document.getElementById('csqtt-error-box');
			if (eb2) eb2.style.display = 'none';
		}

		setNode('csqtt-captcha', (st.captcha_pending > 0)
			? [badge('%s: %s'.format(_('CAPTCHA pending'), st.captcha_pending), 'warning'), ' ',
				E('a', { href: L.url('admin/services/csqtt/captcha') }, _('open'))]
			: subLine(_('None')));

		this.updateProfiles(st);
	},

	updateProfiles: function(st) {
		var box = document.getElementById('csqtt-profiles');
		if (!box)
			return;
		var wrap = document.getElementById('csqtt-profiles-wrap');
		if (!this.failover || !st.profiles || !st.profiles.length) {
			L.dom.content(box, '');
			if (wrap)
				wrap.style.display = 'none';
			return;
		}
		if (wrap)
			wrap.style.display = '';
		var rows = st.profiles.map(function(p) {
			var kind = p.state === 'active' ? 'success'
				: (p.state === 'captcha_required' || p.state === 'cooldown') ? 'warning'
					: (p.state === 'failed' || p.state === 'auth_failed') ? 'danger' : '';
			var isActive = (p.id === st.active_profile || p.state === 'active');
			var name = p.name || p.id;
			return E('tr', { class: 'tr%s'.format(isActive ? ' csqtt-row-active' : '') }, [
				E('td', { class: 'td col-active' }, isActive ? E('span', { class: 'csqtt-badge success' }, _('Active')) : E('span', { class: 'csqtt-empty' }, '—')),
				E('td', { class: 'td col-name' }, [
					E('span', { class: 'csqtt-name', title: name }, name),
					p.name && p.id !== p.name ? E('div', { class: 'csqtt-id' }, p.id) : '',
				]),
				E('td', { class: 'td col-state' }, badge(stateLabel(p.state), kind)),
				E('td', { class: 'td col-priority' }, '%d'.format(p.priority || 0)),
				E('td', { class: 'td col-fails' }, '%d'.format(p.consecutive_fails || 0)),
				E('td', { class: 'td col-cooldown' }, '%ds'.format(p.cooldown_remaining_secs || 0)),
			]);
		});
		L.dom.content(box, E('table', { class: 'table csqtt-table' }, [
			E('thead', {}, E('tr', { class: 'tr table-titles' }, [
				E('th', { class: 'th col-active' }, _('Active')),
				E('th', { class: 'th col-name' }, _('Name')),
				E('th', { class: 'th col-state' }, _('State')),
				E('th', { class: 'th col-priority' }, _('Priority')),
				E('th', { class: 'th col-fails' }, _('Fails')),
				E('th', { class: 'th col-cooldown' }, _('Cooldown')),
			])),
			E('tbody', {}, rows),
		]));
	},

	render: function() {
		try {
			this.failover = (uci.get('csqtt', 'main', 'failover') === '1');
			this.peers = {};
			this.names = {};
			uci.sections('csqtt', 'server').forEach(L.bind(function(s) {
				if (s && s['.name']) {
					this.peers[s['.name']] = s.peer || '';
					this.names[s['.name']] = s.name || s['.name'];
				}
			}, this));
		} catch (e) {
			this.failover = false;
			this.peers = {};
			this.names = {};
		}

		function btn(label, action, mode, self) {
			return E('button', {
				class: 'btn %s'.format(mode || ''),
				click: L.bind(self.handleService, self, action),
			}, label);
		}

		this._svc = {
			start: btn(_('Start'), 'start', '', this),
			stop: btn(_('Stop'), 'stop', '', this),
			restart: btn(_('Restart'), 'restart', 'primary', this),
		};

		var nodes = E('div', { class: 'csqtt csqtt-dash' }, [
			E('style', {}, CSS),
			E('div', { class: 'csqtt-brand' }, [
				E('span', { class: 'csqtt-brand-name' }, BRAND),
				E('span', { class: 'csqtt-brand-tag' }, _('CSQTT tunnel status')),
			]),

			E('div', { class: 'csqtt-hero' }, [
				E('div', { class: 'csqtt-hero-state' }, [
					E('div', { class: 'csqtt-hero-label' }, _('Connection')),
					E('div', { id: 'csqtt-state', class: 'csqtt-hero-value' }, E('em', _('loading…'))),
				]),
				E('div', { class: 'csqtt-hero-meta' }, [
					E('div', {}, [E('span', { class: 'k' }, _('Active profile') + ': '), E('span', { id: 'csqtt-active', class: 'v' }, '—')]),
					E('div', {}, [E('span', { class: 'k' }, _('Uptime') + ': '), E('span', { id: 'csqtt-uptime', class: 'v' }, '—')]),
				]),
				E('div', { class: 'csqtt-hero-actions' }, [
					this._svc.start, this._svc.stop, this._svc.restart,
				]),
			]),

			E('div', { id: 'csqtt-indicators', class: 'csqtt-indicators' }, E('em', _('loading…'))),

			E('div', { class: 'csqtt-grid' }, [
				card(_('Traffic RX / TX'), 'csqtt-traffic'),
				card(_('Workers / Reconnects'), 'csqtt-workers'),
				card(_('CAPTCHA'), 'csqtt-captcha'),
			]),

			E('div', { id: 'csqtt-error-box', class: 'csqtt-error-box', style: 'display:none' }, [
				E('div', { class: 'csqtt-card-title' }, _('Last error')),
				E('div', { id: 'csqtt-error' }),
			]),

			E('h3', { class: 'csqtt-sec-title' }, _('Technical details')),
			E('div', { class: 'csqtt-tech' }, [
				card(_('Tunnel csqtt0'), 'csqtt-tunnel'),
				card(_('Routing (M3X)'), 'csqtt-routing'),
			]),

			E('div', { id: 'csqtt-profiles-wrap', style: 'display:none' }, [
				E('h3', { class: 'csqtt-sec-title' }, _('Profiles')),
				E('div', { class: 'csqtt-tablewrap' }, E('div', { id: 'csqtt-profiles' })),
			]),
		]);

		this.startRefresh();
		return nodes;
	},
});
