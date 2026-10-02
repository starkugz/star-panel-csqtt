'use strict';
'require view';
'require rpc';
'require ui';

// [star-panel] Вью журналов CSQTT (ubus-объект "csqtt").
// Панель управления над журналом: автообновление, число строк, grep, обновить,
// скачать, очистить. Область журнала ограничена по высоте и прокручивается
// внутри; масштаб текста и перенос строк — независимо от остальной панели и
// сохраняются между посещениями. Следование за журналом включается отдельно.
// RPC-контракт (logs/logs_clear) и безопасность не менялись.

const callLogs  = rpc.declare({ object: 'csqtt', method: 'logs', params: ['source', 'lines', 'grep'] });
const callClear = rpc.declare({ object: 'csqtt', method: 'logs_clear' });

const BRAND = 'star-panel-csqtt';
const FONT_MIN = 10, FONT_MAX = 20, FONT_DEF = 13;

const CSS = [
	'.csqtt{width:100%;max-width:1080px;box-sizing:border-box;line-height:1.45;}',
	'.csqtt *,.csqtt *::before,.csqtt *::after{box-sizing:border-box;}',
	'.csqtt-brand{display:flex;flex-wrap:wrap;align-items:baseline;gap:.55em;margin:0 0 .9em;padding:0 0 .6em;border-bottom:1px solid rgba(127,127,127,.3);}',
	'.csqtt-brand-name{font-size:1.3em;font-weight:700;}',
	'.csqtt-brand-tag{font-size:.85em;opacity:.7;}',
	'.csqtt-toolbar{display:flex;flex-wrap:wrap;align-items:center;gap:.5em;margin:0 0 .75em;}',
	'.csqtt-toolbar .csqtt-spacer{margin-left:auto;}',
	'.csqtt-toolbar .csqtt-group{display:flex;flex-wrap:wrap;align-items:center;gap:.5em;}',
	'.csqtt label.csqtt-inline{display:inline-flex;align-items:center;gap:.35em;font-size:.9em;white-space:nowrap;}',
	'.csqtt button.btn,.csqtt a.btn,.csqtt .cbi-button{height:2.35em;min-height:2.35em;line-height:1;display:inline-flex;align-items:center;justify-content:center;gap:.35em;padding:.35em .9em;margin:0;border:1px solid rgba(127,127,127,.38);border-radius:8px;background:rgba(127,127,127,.07);color:inherit;font:inherit;font-size:.92em;cursor:pointer;text-decoration:none;white-space:nowrap;}',
	'.csqtt button.btn:hover,.csqtt a.btn:hover{border-color:#4a90e2;}',
	'.csqtt button.btn.negative{border-color:rgba(217,83,79,.55);}',
	'.csqtt button.icon{min-width:2.35em;padding:.35em .5em;font-size:1em;}',
	'.csqtt input.cbi-input-text,.csqtt select.cbi-select{min-height:2.35em;height:2.35em;padding:.3em .55em;border:1px solid rgba(127,127,127,.38);border-radius:8px;background:rgba(127,127,127,.05);color:inherit;font:inherit;font-size:.92em;}',
	'.csqtt input.cbi-input-text{min-width:12em;}',
	'.csqtt .csqtt-zoomval{min-width:3.6em;text-align:center;font-variant-numeric:tabular-nums;opacity:.9;}',
	'.csqtt-logmeta{display:flex;flex-wrap:wrap;align-items:center;gap:.5em;font-size:.82em;opacity:.72;margin:0 0 .35em;min-height:1.4em;}',
	'.csqtt-logmeta .csqtt-badge{border:1px solid rgba(127,127,127,.35);border-radius:1em;padding:0 .6em;}',
	'.csqtt-logwrap{width:100%;}',
	'.csqtt pre.csqtt-logview{display:block;width:100%;margin:0;padding:.6em .75em;border:1px solid rgba(127,127,127,.32);border-radius:10px;background:rgba(0,0,0,.18);font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,"Liberation Mono",monospace;font-size:13px;line-height:1.45;white-space:pre;overflow:auto;height:calc(100vh - 21em);min-height:11em;max-height:calc(100vh - 13em);tab-size:4;}',
	'.csqtt pre.csqtt-logview.csqtt-empty{color:rgba(127,127,127,.9);}',
	'.csqtt .logline{display:block;}',
	'.csqtt .logtime{color:#7fb2e5;}',
	'.csqtt .logwarn{color:#e0a44a;font-weight:600;}',
	'.csqtt .logerr{color:#e06c6c;font-weight:600;}',
	'.csqtt .loghr{opacity:.7;}',
	'.csqtt-note{font-size:.85em;opacity:.72;margin:.5em 0 0;}',
	'@media (max-width:640px){.csqtt pre.csqtt-logview{height:calc(100vh - 24em);}.csqtt input.cbi-input-text{min-width:0;flex:1 1 8em;}}',
].join('');

function notify(msg, kind) {
	ui.addTimeLimitedNotification('csqtt', E('p', msg), 5000, kind);
}

function loadPref(key, def) {
	try {
		var v = (typeof localStorage !== 'undefined') ? localStorage.getItem('csqtt.log.' + key) : null;
		return (v == null) ? def : v;
	} catch (e) { return def; }
}

function savePref(key, val) {
	try {
		if (typeof localStorage !== 'undefined')
			localStorage.setItem('csqtt.log.' + key, String(val));
	} catch (e) {}
}

// Подсветка: время, WARN, ERROR. Исходный текст строки сохраняется.
function highlight(line) {
	var text = String(line);
	var parts = [], re = /(\d{2}:\d{2}:\d{2}|\bWARN(?:ING)?\b|\bERROR\b|\bERR\b|\bFAIL(?:ED)?\b)/gi;
	var idx = 0, m;
	while ((m = re.exec(text)) !== null) {
		if (m.index > idx)
			parts.push(text.slice(idx, m.index));
		var tok = m[0], low = tok.toLowerCase(), cls = 'loghr';
		if (/^\d\d:\d\d:\d\d$/.test(tok)) cls = 'logtime';
		else if (low.indexOf('warn') === 0) cls = 'logwarn';
		else cls = 'logerr';
		parts.push(E('span', { class: cls }, tok));
		idx = m.index + tok.length;
	}
	if (idx < text.length)
		parts.push(text.slice(idx));
	if (!parts.length)
		parts.push(text);
	return parts;
}

return view.extend({
	title: BRAND + ' — ' + _('Logs'),

	auto: true,
	follow: true,
	wrap: false,
	font: FONT_DEF,
	last: {},
	preFile: null,
	preSyslog: null,

	load: function() {
		this.auto = true;
		this.follow = loadPref('follow', '1') === '1';
		this.wrap = loadPref('wrap', '0') === '1';
		this.font = Math.min(FONT_MAX, Math.max(FONT_MIN, parseInt(loadPref('font', FONT_DEF), 10) || FONT_DEF));
		return Promise.resolve();
	},

	activeSource: function() {
		var pane = this.root ? this.root.querySelector('[data-tab][data-tab-active="true"]') : null;
		return (pane && pane.getAttribute('data-tab') === 'syslog') ? 'syslog' : 'file';
	},

	activePre: function() {
		return (this.activeSource() === 'file') ? this.preFile : this.preSyslog;
	},

	showError: function(msg) {
		var box = document.getElementById('csqtt-log-err');
		if (!box)
			return;
		if (msg) {
			box.style.display = '';
			L.dom.content(box, E('div', { class: 'alert-message warning' }, msg));
		} else {
			box.style.display = 'none';
			L.dom.content(box, '');
		}
	},

	setStatus: function(nodes) {
		var el = document.getElementById('csqtt-log-status');
		if (el)
			L.dom.content(el, nodes);
	},

	// Применяем масштаб/перенос ТОЛЬКО к области журнала (не к навигации/кнопкам).
	applyView: function() {
		[this.preFile, this.preSyslog].forEach(L.bind(function(pre) {
			if (!pre || !pre.style)
				return;
			pre.style.fontSize = this.font + 'px';
			pre.style.whiteSpace = this.wrap ? 'pre-wrap' : 'pre';
			pre.style.overflowX = this.wrap ? 'hidden' : 'auto';
		}, this));
		var zv = document.getElementById('csqtt-zoom-val');
		if (zv)
			L.dom.content(zv, this.font + ' px');
		var wc = document.getElementById('csqtt-wrap-toggle');
		if (wc && 'checked' in wc) wc.checked = this.wrap;
		var fc = document.getElementById('csqtt-follow-toggle');
		if (fc && 'checked' in fc) fc.checked = this.follow;
	},

	zoom: function(delta) {
		this.font = Math.min(FONT_MAX, Math.max(FONT_MIN, this.font + delta));
		savePref('font', this.font);
		this.applyView();
	},

	resetZoom: function() {
		this.font = FONT_DEF;
		savePref('font', this.font);
		this.applyView();
	},

	toggleWrap: function(on) {
		this.wrap = !!on;
		savePref('wrap', this.wrap ? '1' : '0');
		this.applyView();
	},

	toggleFollow: function(on) {
		this.follow = !!on;
		savePref('follow', this.follow ? '1' : '0');
		if (this.follow)
			this.scrollToEnd();
	},

	scrollToEnd: function() {
		var pre = this.activePre();
		if (pre && typeof pre.scrollHeight === 'number' && 'scrollTop' in pre)
			pre.scrollTop = pre.scrollHeight;
	},

	renderLines: function(lines) {
		return lines.map(function(line) {
			return E('span', { class: 'logline' }, highlight(line));
		});
	},

	refresh: function() {
		var self = this,
		    sel = document.getElementById('csqtt-log-lines'),
		    inp = document.getElementById('csqtt-log-grep'),
		    src = this.activeSource(),
		    n = (sel ? parseInt(sel.value, 10) : 0) || 100,
		    g = (inp ? inp.value : '') || '';

		var pre = (src === 'file') ? this.preFile : this.preSyslog;
		var atBottom = true;
		if (pre && typeof pre.scrollHeight === 'number' && 'scrollTop' in pre)
			atBottom = (pre.scrollTop + pre.clientHeight) >= (pre.scrollHeight - 24);

		if (!this.last[src] && pre)
			this.setStatus(E('span', {}, _('loading…')));

		return callLogs(src, n, g)
			.then(function(res) {
				if (!res || res.error) {
					self.showError((res && res.error) || _('no data'));
					self.setStatus(E('span', { class: 'csqtt-badge' }, _('no data')));
					if (pre) L.dom.content(pre, '');
					return;
				}
				self.showError(null);
				var lines = res.lines || [];
				self.last[src] = lines;
				self.setStatus([
					E('span', {}, new Date().toLocaleTimeString()),
					E('span', { class: 'csqtt-badge' }, '%s %s'.format(lines.length, _('lines'))),
					E('span', {}, _('source') + ': ' + (src === 'file' ? _('File') : _('Syslog'))),
				]);
				if (pre) {
					L.dom.content(pre, lines.length
						? self.renderLines(lines)
						: E('span', { class: 'logline' }, _('no data')));
					self.applyView();
					if (self.follow || atBottom)
						self.scrollToEnd();
				}
			})
			.catch(function() {
				self.showError(_('RPC error (logs)'));
				self.setStatus(E('span', { class: 'csqtt-badge' }, _('RPC error (logs)')));
			});
	},

	startRefresh: function() {
		var self = this;
		this._pollFn = function() {
			if (!document.getElementById('csqtt-log-lines'))
				return;
			if (self.auto)
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

	handleDownload: function() {
		var src = this.activeSource(),
		    lines = this.last[src] || [],
		    blob = new Blob([lines.join('\n') + '\n'], { type: 'text/plain' }),
		    a = E('a', {
			href: URL.createObjectURL(blob),
			download: 'csqtt-%s.log'.format(src),
			style: 'display:none',
		});
		document.body.appendChild(a);
		a.click();
		window.setTimeout(function() {
			URL.revokeObjectURL(a.href);
			document.body.removeChild(a);
		}, 1000);
	},

	handleClear: function() {
		if (this.activeSource() !== 'file') {
			notify(_('Clear is available for the log file only'), 'warning');
			return;
		}
		if (!confirm(_('Truncate /var/log/csqtt.log? This cannot be undone.')))
			return;
		var self = this;
		callClear()
			.then(function(res) {
				if (res && res.ok)
					notify(_('Log cleared (%s bytes)').format(res.cleared_bytes), 'info');
				else
					notify(_('Clear failed'), 'error');
				self.last = {};
				self.refresh();
			})
			.catch(function() {
				notify(_('Permission denied or rpcd error'), 'error');
			});
	},

	render: function() {
		var self = this;
		this.last = {};
		this.preFile = E('pre', { class: 'csqtt-logview' }, _('loading…'));
		this.preSyslog = E('pre', { class: 'csqtt-logview' }, _('loading…'));

		function icon(label, title, handler) {
			return E('button', { class: 'btn icon', title: title, click: handler }, label);
		}

		this.root = E('div', { class: 'csqtt csqtt-logs' }, [
			E('style', {}, CSS),

			E('div', { class: 'csqtt-brand' }, [
				E('span', { class: 'csqtt-brand-name' }, BRAND),
				E('span', { class: 'csqtt-brand-tag' }, _('Service logs')),
			]),

			E('div', { class: 'csqtt-toolbar' }, [
				E('label', { class: 'csqtt-inline' }, [
					E('input', { type: 'checkbox', class: 'cbi-checkbox', checked: 'checked',
						change: function() { self.auto = this.checked; } }),
					_('auto-refresh 5s'),
				]),
				E('select', { id: 'csqtt-log-lines', class: 'cbi-select', change: function() { self.refresh(); } }, [
					E('option', { value: '50' }, '50'),
					E('option', { value: '100', selected: 'selected' }, '100'),
					E('option', { value: '500' }, '500'),
				]),
				E('input', {
					id: 'csqtt-log-grep', type: 'text', class: 'cbi-input-text',
					placeholder: _('grep (regular expression)'),
					keypress: function(ev) { if (ev.key === 'Enter') self.refresh(); },
				}),
				E('button', { class: 'btn', click: function() { self.refresh(); } }, _('Refresh')),
				E('button', { class: 'btn', click: function() { self.handleDownload(); } }, _('Download')),
				E('button', { class: 'btn negative', click: function() { self.handleClear(); } }, _('Clear')),

				E('span', { class: 'csqtt-spacer' }),
				E('div', { class: 'csqtt-group', title: _('Log text size') }, [
					icon('−', _('Smaller'), function() { self.zoom(-1); }),
					E('span', { id: 'csqtt-zoom-val', class: 'csqtt-zoomval' }, this.font + ' px'),
					icon('+', _('Bigger'), function() { self.zoom(1); }),
					E('button', { class: 'btn', click: function() { self.resetZoom(); } }, _('Reset')),
				]),
				E('label', { class: 'csqtt-inline' }, [
					E('input', { id: 'csqtt-wrap-toggle', type: 'checkbox', class: 'cbi-checkbox',
						checked: self.wrap ? 'checked' : null,
						change: function() { self.toggleWrap(this.checked); } }),
					_('Wrap lines'),
				]),
				E('label', { class: 'csqtt-inline' }, [
					E('input', { id: 'csqtt-follow-toggle', type: 'checkbox', class: 'cbi-checkbox',
						checked: self.follow ? 'checked' : null,
						change: function() { self.toggleFollow(this.checked); } }),
					_('Follow'),
				]),
			]),

			E('div', { class: 'csqtt-logmeta' }, E('span', { id: 'csqtt-log-status' }, _('loading…'))),
			E('div', { id: 'csqtt-log-err', style: 'display:none' }),
			E('div', { class: 'cbi-tab-container csqtt-logwrap' }, [
				E('div', {
					'data-tab': 'file',
					'data-tab-title': _('File'),
					'data-tab-active': 'true',
				}, this.preFile),
				E('div', {
					'data-tab': 'syslog',
					'data-tab-title': _('Syslog'),
					'data-tab-active': 'false',
				}, this.preSyslog),
			]),
			E('p', { class: 'csqtt-note' }, _('Text size and line wrapping apply to the log area only and are remembered.')),
		]);

		this.applyView();
		this.startRefresh();
		return this.root;
	},
});
