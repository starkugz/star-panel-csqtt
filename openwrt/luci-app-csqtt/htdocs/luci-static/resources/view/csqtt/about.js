'use strict';
'require view';
'require rpc';
'require ui';

// [star-panel] Персональная редакция LuCI-панели CSQTT.
// Раздел «О программе»: имя клиента, версия редакции, фактическая версия ядра
// (из status.json запущенной службы CSQTT; fallback — версия установленного пакета),
// автор редакции/оформления и сохранённые сведения об исходном проекте,
// лицензии и обязательных уведомлениях.
// Технические идентификаторы (ubus «csqtt», UCI «csqtt», путь csqtt/about)
// НЕ меняются: это только пользовательская страница.

const callStatus = rpc.declare({ object: 'csqtt', method: 'status' });

const BRAND = 'star-panel-csqtt';
// Три независимые версии (см. docs/RELEASE.md): пакет интеграции для OpenWrt,
// панель LuCI и встроенное ядро. Ядро 2.1.9 не переименовывается в 1.0.
const INTEGRATION_VERSION = '1.0';
const PANEL_VERSION = '1.0';
const EDITION_AUTHOR = 'starkugz';
const CORE_VERSION_FALLBACK = '2.1.9';

const CSS = [
	'.csqtt-about{width:100%;max-width:54em;box-sizing:border-box;}',
	'.csqtt-about *{box-sizing:border-box;}',
	'.csqtt-brand{display:flex;flex-wrap:wrap;align-items:baseline;gap:.6em;margin:0 0 1.1em;padding:0 0 .7em;border-bottom:1px solid rgba(127,127,127,.3);}',
	'.csqtt-brand-name{font-size:1.35em;font-weight:700;letter-spacing:.01em;overflow-wrap:anywhere;}',
	'.csqtt-brand-tag{font-size:.85em;opacity:.72;}',
	'.csqtt-section{margin:0 0 1.4em;}',
	'.csqtt-sec-title{font-size:1em;font-weight:700;margin:.2em 0 .75em;padding-bottom:.35em;border-bottom:1px solid rgba(127,127,127,.3);}',
	'.csqtt-kv{display:grid;grid-template-columns:14em 1fr;gap:.45em .9em;margin:0;}',
	'.csqtt-kv dt{opacity:.72;margin:0;}',
	'.csqtt-kv dd{margin:0;font-weight:600;overflow-wrap:anywhere;}',
	'.csqtt-kv dd .csqtt-mono{font-family:monospace;font-weight:600;}',
	'.csqtt-p{font-size:.9em;line-height:1.5;margin:.5em 0;overflow-wrap:anywhere;}',
	'.csqtt-note{font-size:.85em;line-height:1.45;opacity:.78;margin:.5em 0;overflow-wrap:anywhere;}',
	'.csqtt-badge{display:inline-block;font-size:.78em;border:1px solid rgba(127,127,127,.4);border-radius:1em;padding:.05em .7em;margin-left:.5em;vertical-align:middle;}',
	'@media (max-width:560px){.csqtt-kv{grid-template-columns:1fr;gap:.1em .5em;}.csqtt-kv dd{margin:0 0 .6em;}}',
].join('');

function kv(pairs) {
	return E('dl', { class: 'csqtt-kv' }, pairs.reduce(function(acc, pair) {
		acc.push(E('dt', {}, pair[0]));
		acc.push(E('dd', {}, pair[1]));
		return acc;
	}, []));
}

function section(title, rows) {
	return E('div', { class: 'csqtt-section' }, [
		E('h3', { class: 'csqtt-sec-title' }, title),
	].concat(rows));
}

return view.extend({
	title: BRAND + ' — ' + _('About'),

	coreVersion: CORE_VERSION_FALLBACK,
	coreLive: false,

	load: function() {
		return callStatus().catch(function() { return null; });
	},

	// Версия ядра: живая из запущенной службы CSQTT; иначе — версия пакета.
	readCore: function(res) {
		var st = (res && res.status && typeof res.status === 'object') ? res.status : {};
		if (res && res.running && st.version) {
			this.coreVersion = String(st.version);
			this.coreLive = true;
		} else {
			this.coreVersion = CORE_VERSION_FALLBACK;
			this.coreLive = false;
		}
	},

	render: function(res) {
		this.readCore(res);

		return E('div', { class: 'csqtt-about' }, [
			E('style', {}, CSS),

			E('div', { class: 'csqtt-brand' }, [
				E('span', { class: 'csqtt-brand-name' }, BRAND),
				E('span', { class: 'csqtt-brand-tag' }, _('Personal edition of the CSQTT client for OpenWrt (LuCI panel).')),
			]),

			section(_('Versions'), [
				kv([
					[_('OpenWrt client package'), E('span', { class: 'csqtt-mono' }, INTEGRATION_VERSION)],
					[_('LuCI panel'), E('span', { class: 'csqtt-mono' }, BRAND + ' ' + PANEL_VERSION)],
					[_('LuCI panel author'), EDITION_AUTHOR],
					[_('Core version'), [
						E('span', { class: 'csqtt-mono' }, this.coreVersion),
						E('span', { class: 'csqtt-badge' }, this.coreLive ? _('running') : _('packaged')),
					]],
				]),
				E('p', { class: 'csqtt-note' }, _('The core engine is the unchanged original CSQTT. The OpenWrt client package and the LuCI panel are versioned separately from the core, so a package version (1.0) does not mean a new core version (2.1.9).')),
				E('p', { class: 'csqtt-note' }, _('The core version is read from the running daemon; if the service is stopped, the packaged core version is shown.')),
			]),

			section(_('Original project and notices'), [
				kv([
					[_('Original project'), 'CSQTT for OpenWrt'],
					[_('Original project author'), 'amurcanov'],
					[_('License'), 'PolyForm Noncommercial 1.0.0'],
					[_('Required notice'), E('span', { class: 'csqtt-mono' }, 'Copyright 2026 amurcanov')],
				]),
				E('p', { class: 'csqtt-p' }, _('The core engine — tunnel, protocols, profiles and service management — is the unchanged original CSQTT.')),
				E('p', { class: 'csqtt-p' }, _('This is a personal, non-commercial edition. The original project, its author, license and required notices are preserved.')),
			]),
		]);
	},

	// Своя страница без формы: базовый footer LuCI не нужен.
	handleSave: null,
	handleReset: null,
	handleSaveApply: null,
});
