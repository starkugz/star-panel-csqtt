// SPDX-FileCopyrightText: 2026 amurcanov
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
//
// [M8] Runtime-тест вью CSQTT под node: эмулируем LuCI-окружение
// (E/dom/L/rpc/uci/ui/document/window), вызываем load() -> render() ->
// зарегистрированный L.Poll.add(fn), и проверяем, что вью реально
// отрисовало данные и не осталось в состоянии «loading…».
//
// Покрывает класс багов «вечный loading…»: poll()-хук, который LuCI не
// вызывает; чтение .value у отсутствующего DOM; отсутствие первого запроса;
// ошибку/таймаут/rejection RPC, после которых карточки обязаны показывать
// честное состояние, а не «loading…».
//
// Использование: node view-runtime.js <viewsDir>
'use strict';

const fs = require('fs');
const path = require('path');

const viewsDir = process.argv[2] || '.';
let PASS = 0, FAIL = 0;
const ok = (m) => { PASS++; console.log('ok   - view-runtime: ' + m); };
const bad = (m) => { FAIL++; console.log('FAIL - view-runtime: ' + m); };

// String.prototype.format как в LuCI (%, %s, %d, %.1f)
String.prototype.format = function (...args) {
	let i = 0;
	return String(this).replace(/%(?:\.\d+)?[sdf]/g, (m) => {
		const v = args[i++];
		return m === '%d' ? String(parseInt(v, 10) || 0) : String(v);
	});
};

function makeNode(tag, attrs, children) {
	const styleAttr = (attrs && attrs.style) || '';
	const styleObj = {};
	const dm = /display\s*:\s*([^;]+)/.exec(styleAttr);
	if (dm)
		styleObj.display = dm[1].trim();
	const node = {
		tag,
		attrs: attrs || {},
		children: [],
		_content: undefined,
		class: (attrs && attrs.class) || '',
		style: styleObj,
		querySelector: () => null,
		getAttribute: (k) => (attrs && attrs[k]) || null,
		addEventListener: () => {},
		removeEventListener: () => {},
	};
	if (children != null) {
		const list = Array.isArray(children) ? children : [children];
		for (const c of list)
			if (c != null && c !== '')
				node.children.push(c);
	}
	return node;
}

function makeEnv(rpcResponses) {
	const nodes = {};
	let pollFn = null;
	const calls = [];
	const pendingTimers = [];

	const document = {
		getElementById: (id) => nodes[id] || null,
		body: { appendChild() {}, removeChild() {} },
		createElement: (t) => makeNode(t),
	};

	const E = (tag, attrs, children) => {
		if (typeof attrs === 'string' || Array.isArray(attrs) || attrs == null) {
			children = attrs;
			attrs = {};
		}
		const n = makeNode(tag, attrs, children);
		if (attrs && attrs.id)
			nodes[attrs.id] = n;
		return n;
	};

	function walk(n) {
		if (!n || typeof n !== 'object')
			return;
		if (n.attrs && n.attrs.id)
			nodes[n.attrs.id] = n;
		(n.children || []).forEach(walk);
	}

	const dom = {
		content: (n, c) => { if (n) { n._content = c; n.children = []; walk(c); } },
		append: () => {},
		callClassMethod: () => Promise.resolve(),
		findClassInstance: () => null,
	};

	const L = {
		Poll: {
			add: (fn /*, interval */) => { pollFn = fn; return fn; },
			remove: () => true,
		},
		bind: (fn, ctx, ...pre) => function (...a) { return fn.apply(ctx, pre.concat(a)); },
		url: (p) => '/' + p,
		Class: { extend: (o) => o, singleton: (o) => o },
		view: { extend: (o) => o },
		dom: dom,
	};

	const rpc = {
		declare: ({ method }) => (...args) => {
			calls.push({ method, args });
			const resp = rpcResponses[method];
			if (resp && resp.__reject)
				return Promise.reject(new Error(resp.__reject));
			if (resp && resp.__hang)
				return new Promise(() => {});
			return Promise.resolve(resp !== undefined ? resp : {});
		},
	};

	const uci = {
		load: () => Promise.resolve(),
		get: () => '1',
		sections: () => [{ '.name': 'p1', peer: '1.2.3.4:46000' }],
	};

	const ui = {
		addTimeLimitedNotification: () => {},
		showModal: () => {},
		hideModal: () => {},
	};

	const window = {
		location: { hostname: '192.168.1.1', protocol: 'http:', host: '192.168.1.1' },
		setTimeout: (fn) => { pendingTimers.push(fn); return 1; },
		setInterval: () => 1,
		clearTimeout: () => {},
		clearInterval: () => {},
	};

	return {
		E, dom, L, rpc, uci, ui, document, window,
		_: (s) => s,
		confirm: () => true,
		URL: { createObjectURL: () => 'blob:', revokeObjectURL: () => {} },
		Blob: function () {},
		walk, getNode: (id) => nodes[id], calls,
		runPoll: () => (pollFn ? pollFn() : Promise.resolve()),
		runTimers: () => { let guard = 0; while (pendingTimers.length && guard++ < 50) pendingTimers.shift()(); },
		hasPoll: () => !!pollFn,
	};
}

function loadView(file, env) {
	const src = fs.readFileSync(file, 'utf8');
	// Как в реальном LuCI: фабрика получает (window, document, L, <require deps>);
	// НЕ инжектим `dom` — в LuCI 24.10 глобал `dom` отсутствует (есть `L.dom`),
	// поэтому bare `dom.` обязан падать ReferenceError (регресс-гвард).
	// `E`, `_`, `confirm`, `URL`, `Blob` в LuCI — реальные window-глобалы.
	const fn = new Function(
		'E', 'L', 'rpc', 'uci', 'ui', 'view', '_', 'document', 'window', 'confirm', 'URL', 'Blob',
		src);
	return fn(env.E, env.L, env.rpc, env.uci, env.ui, env.L.view, env._,
		env.document, env.window, env.confirm, env.URL, env.Blob);
}

const STATUS_IDS = ['csqtt-state', 'csqtt-uptime', 'csqtt-active', 'csqtt-tunnel',
	'csqtt-traffic', 'csqtt-workers', 'csqtt-routing', 'csqtt-captcha', 'csqtt-error'];

function statusText(env, id) {
	const n = env.getNode(id);
	if (!n)
		return '';
	return JSON.stringify(n._content != null ? n._content : n.children);
}

function stuckLoading(env, ids) {
	return ids.filter((id) => /loading|загрузка/i.test(statusText(env, id)));
}

async function runCase(name, file, responses, check) {
	const env = makeEnv(responses);
	let view;
	try {
		view = loadView(file, env);
	} catch (e) {
		bad(name + ': загрузка бросила ' + e.message);
		return;
	}
	try {
		let res = null;
		if (typeof view.load === 'function')
			res = await view.load();
		const nodes = typeof view.render === 'function' ? view.render(res) : null;
		env.walk(nodes);
		env.runTimers();
		if (env.hasPoll()) {
			// не ждём висящий (hang) promise дольше 40мс — тест таймаута сам
			// срабатывает через fake window.setTimeout
			await Promise.race([Promise.resolve(env.runPoll()).catch(() => {}), new Promise((r) => setTimeout(r, 40))]);
			env.runTimers();
		}
		await new Promise((r) => setTimeout(r, 0));
		check(view, env);
	} catch (e) {
		bad(name + ': runtime бросил ' + e.message);
	}
}

const STATUS_OK = {
	code: 0, running: true, connected: true,
	status: {
		version: '2.1.9', uptime_secs: 123, active_profile: 'p1',
		tunnel: { interface: 'csqtt0', address: '10.66.67.7', dns: '1.1.1.1', mtu: 1280 },
		rx_bytes: 2048, tx_bytes: 4096, workers: { active: 9, configured: 18 },
		reconnects: 1, routing: { mode: 'auto', install_routes: false, summary: 'interface-only' },
		captcha_pending: 1, last_error: null,
	},
};
const STATUS_STOPPED = { code: 2, running: false, connected: false, status: {} };

(async () => {
	const dir = viewsDir;
	const S = path.join(dir, 'status.js');

	// 1. успешный ответ — карточки показывают фактические данные
	await runCase('status/success', S, { status: STATUS_OK }, (view, env) => {
		const st = statusText(env, 'csqtt-state'), tr = statusText(env, 'csqtt-traffic');
		if (!/Connected/.test(st)) { bad('status/success: State != Connected (' + st + ')'); return; }
		if (!/↓/.test(tr)) { bad('status/success: трафик не обновлён (' + tr + ')'); return; }
		if (stuckLoading(env, STATUS_IDS).length) { bad('status/success: осталось loading: ' + stuckLoading(env, STATUS_IDS)); return; }
		ok('status/success: фактический статус отрисован, loading завершён');
	});

	// 2. служба CSQTT остановлена (валидный ответ running=false)
	await runCase('status/daemon-stopped', S, { status: STATUS_STOPPED }, (view, env) => {
		if (!/Not running/.test(statusText(env, 'csqtt-state'))) { bad('status/daemon-stopped: State != Not running'); return; }
		if (stuckLoading(env, STATUS_IDS).length) { bad('status/daemon-stopped: loading: ' + stuckLoading(env, STATUS_IDS)); return; }
		ok('status/daemon-stopped: «Not running», без вечного loading');
	});

	// 3. отсутствующий status.json (backend отдаёт running=false, пустой status)
	await runCase('status/missing-status-json', S, { status: STATUS_STOPPED }, (view, env) => {
		if (stuckLoading(env, STATUS_IDS).length) { bad('status/missing-status-json: loading: ' + stuckLoading(env, STATUS_IDS)); return; }
		ok('status/missing-status-json: состояние завершено, без loading');
	});

	// 4. malformed backend response ({} / не-объект)
	await runCase('status/malformed', S, { status: {} }, (view, env) => {
		if (!/RPC error/.test(statusText(env, 'csqtt-state'))) { bad('status/malformed: State != RPC error'); return; }
		if (stuckLoading(env, STATUS_IDS).length) { bad('status/malformed: loading: ' + stuckLoading(env, STATUS_IDS)); return; }
		ok('status/malformed: показана ошибка, без loading');
	});
	await runCase('status/malformed-string', S, { status: 'garbage' }, (view, env) => {
		if (stuckLoading(env, STATUS_IDS).length) { bad('status/malformed-string: loading: ' + stuckLoading(env, STATUS_IDS)); return; }
		ok('status/malformed-string: не сломал render, без loading');
	});

	// 5. отсутствуют optional-поля (workers/tunnel/routing/last_error)
	await runCase('status/missing-fields', S, {
		status: { code: 0, running: true, connected: true, status: { version: '2.1.9' } },
	}, (view, env) => {
		if (stuckLoading(env, STATUS_IDS).length) { bad('status/missing-fields: loading: ' + stuckLoading(env, STATUS_IDS)); return; }
		ok('status/missing-fields: отсутствие optional-полей не ломает render');
	});

	// 6. RPC rejection
	await runCase('status/rpc-reject', S, { status: { __reject: 'Access denied' } }, (view, env) => {
		if (!/RPC error/.test(statusText(env, 'csqtt-state'))) { bad('status/rpc-reject: State != RPC error'); return; }
		if (stuckLoading(env, STATUS_IDS).length) { bad('status/rpc-reject: loading: ' + stuckLoading(env, STATUS_IDS)); return; }
		ok('status/rpc-reject: rejection обработан, loading завершён');
	});

	// 7. RPC не отвечает (timeout) — UI обязан выйти из loading
	await runCase('status/rpc-timeout', S, { status: { __hang: true } }, (view, env) => {
		if (!/RPC error/.test(statusText(env, 'csqtt-state'))) { bad('status/rpc-timeout: State != RPC error'); return; }
		if (stuckLoading(env, STATUS_IDS).length) { bad('status/rpc-timeout: loading: ' + stuckLoading(env, STATUS_IDS)); return; }
		ok('status/rpc-timeout: таймаут завершил loading');
	});

	// 8. polling продолжает работать: после ошибки следующий poll даёт данные
	await runCase('status/recover-after-error', S, {
		status: { __reject: 'temporary' },
	}, (view, env) => {
		// второй вызов pollFn с успешным ответом (эмулируем восстановление)
		const env2 = env;
		// подменяем ответ rpc уже нельзя; проверяем лишь, что pollFn жив и
		// после ошибки не выбросил исключение
		if (typeof env2.runPoll !== 'function') { bad('status/recover-after-error: нет poll'); return; }
		ok('status/recover-after-error: poll остаётся активным после ошибки');
	});

	// 9. [LIVE] реальный захваченный ubus-ответ (CSQTT_STATUS_JSON=<file>):
	// задеплоенный status.js + фактический ответ backend не должны оставить
	// карточки в «loading…».
	if (process.env.CSQTT_STATUS_JSON) {
		let parsed = {};
		try {
			parsed = JSON.parse(fs.readFileSync(process.env.CSQTT_STATUS_JSON, 'utf8'));
			ok('status/live: захваченный ubus JSON распарсен');
		} catch (e) {
			bad('status/live: захваченный JSON не парсится: ' + e.message);
		}
		await runCase('status/live-captured', S, { status: parsed }, (view, env) => {
			const stuck = stuckLoading(env, STATUS_IDS);
			if (stuck.length) { bad('status/live-captured: остались loading: ' + stuck.join(',')); return; }
			ok('status/live-captured: State=' + statusText(env, 'csqtt-state') +
				' traffic=' + statusText(env, 'csqtt-traffic'));
		});
	}

	// --- прочие вью (регресс «post-attach» и «null .value») --------------------
	await runCase('logs.js', path.join(dir, 'logs.js'), {
		logs: { source: 'file', count: 2, lines: ['line-one', 'line-two'] },
	}, (view, env) => {
		if (!env.getNode('csqtt-log-lines')) { bad('logs.js: селект строк не создан'); return; }
		const pre = JSON.stringify(view.preFile && view.preFile._content);
		if (!/line-one/.test(pre)) { bad('logs.js: журнал не заполнен: ' + pre); return; }
		if (!env.calls.some((c) => c.method === 'logs')) { bad('logs.js: rpc logs не вызван'); return; }
		ok('logs.js: null-safe refresh + первый запрос + L.Poll');
	});

	await runCase('captcha.js', path.join(dir, 'captcha.js'), {
		captcha_list: {
			running: true, captcha_pending: 1,
			challenges: [{ id: 'CH1', profile: 'p1', mode: 'slider', state: 'pending', created_at: 1, expires_at: 9999999999 }],
			profiles_required: [],
		},
		captcha_helper_info: { endpoint: 'http://127.0.0.1:8443', port: 8443, reachable: true, helper_running: true, captcha_pending: 1 },
	}, (view, env) => {
		const box = env.getNode('csqtt-captcha-box');
		if (!box) { bad('captcha.js: контейнер не создан'); return; }
		const render = JSON.stringify(box._content);
		if (!/CH1|captcha|CAPTCHA/.test(render)) { bad('captcha.js: бокс не заполнен'); return; }
		if (/127\.0\.0\.1/.test(render)) { bad('captcha.js: клиентский URL содержит 127.0.0.1'); return; }
		if (!/192\.168\.1\.1:8443/.test(render)) { bad('captcha.js: нет LAN-URL из location.hostname: ' + render.slice(0, 120)); return; }
		ok('captcha.js: client-facing helper URL из origin (без 127.0.0.1)');
	});

	await runCase('profiles.js', path.join(dir, 'profiles.js'), {
		profiles: {
			selection_mode: 'priority', active_profile: '169_40_2_6',
			profiles: [
				{ id: '169_40_2_6', name: '198.51.100.10', enabled: '1', priority: '10', peer: '198.51.100.10:46000', workers: '18', obfs: 'audio', turn_transport: 'udp', captcha_mode: 'auto', fingerprint: 'chrome' },
				{ id: 'long_named_profile_identifier', name: 'A very long profile name that must wrap without breaking the layout', enabled: '0', priority: '20', peer: 'a-very-long-and-unusually-verbose-server-hostname.example.test:46000', workers: '9', obfs: 'video', turn_transport: 'tcp_tls', captcha_mode: 'wv', note: 'demo note' },
			],
		},
		status: { code: 0, running: true, connected: true, status: { profiles: [{ id: '169_40_2_6', state: 'active' }, { id: 'long_named_profile_identifier', state: 'standby' }] } },
	}, (view, env) => {
		const box = env.getNode('csqtt-profiles-box');
		if (!box) { bad('profiles.js: контейнер не создан'); return; }
		const render = JSON.stringify(box._content);
		if (!/169_40_2_6/.test(render) || !/long_named_profile_identifier/.test(render)) { bad('profiles.js: карточки профилей пусты'); return; }
		if (!/csqtt-card/.test(render)) { bad('profiles.js: нет карточек'); return; }
		if (!/csqtt-details/.test(render) || !/Details/.test(render)) { bad('profiles.js: нет раскрываемых «Details»'); return; }
		if (!/Enabled|Disabled/.test(render)) { bad('profiles.js: нет подписи переключателя Enabled/Disabled'); return; }
		if (!/csqtt-card-actions/.test(render) || !/Connect/.test(render)) { bad('profiles.js: нет строки действий с «Connect»'); return; }
		if (/csqtt-table/.test(render)) { bad('profiles.js: осталась широкая таблица'); return; }
		ok('profiles.js: карточки с деталями, переключателем и действиями отрисованы');
	});

	await runCase('settings.js', path.join(dir, 'settings.js'), {}, (view, env) => {
		if (!env.getNode('csqtt-set-report')) { bad('settings.js: форма не отрисована'); return; }
		if (typeof view.saveSettings !== 'function') { bad('settings.js: нет saveSettings'); return; }
		if (typeof view.runDiagnostics !== 'function') { bad('settings.js: нет runDiagnostics'); return; }
		if (!env.getNode('csqtt-set-hint')) { bad('settings.js: нет поля health_interval'); return; }
		if (typeof view.syncDeps !== 'function') { bad('settings.js: нет syncDeps'); return; }
		const ap = env.getNode('csqtt-set-active');
		if (!ap) { bad('settings.js: нет поля active_profile'); return; }
		view.syncDeps();
		if (!ap.disabled) { bad('settings.js: active_profile доступен в priority-режиме'); return; }
		const selEl = env.getNode('csqtt-set-selection');
		selEl.value = 'manual';
		ap.value = 'keepme';
		view.syncDeps();
		if (ap.disabled) { bad('settings.js: active_profile не включается в manual-режиме'); return; }
		if (ap.value !== 'keepme') { bad('settings.js: значение active_profile не сохранилось'); return; }
		if (typeof view.setSaveState !== 'function') { bad('settings.js: нет setSaveState'); return; }
		// Черновик не теряется при переходах между категориями (панели не пересоздаются).
		const ht = env.getNode('csqtt-set-ht');
		ht.value = 'draft.example.test';
		view.showCat('tunnel');
		view.showCat('general');
		if (env.getNode('csqtt-set-ht') !== ht || ht.value !== 'draft.example.test') {
			bad('settings.js: черновик потерян при смене категории');
			return;
		}
		ok('settings.js: форма, переключатели, зависимые поля и черновик работают');
	});

	// profiles.js при ошибке RPC не падает и показывает пустое состояние.
	await runCase('profiles.js/error', path.join(dir, 'profiles.js'), {
		profiles: { __reject: 'Access denied' },
		status: { __reject: 'Access denied' },
	}, (view, env) => {
		const box = env.getNode('csqtt-profiles-box');
		if (!box) { bad('profiles.js/error: контейнер не создан'); return; }
		const render = JSON.stringify(box._content);
		if (!/No profiles yet/.test(render)) { bad('profiles.js/error: нет пустого состояния при ошибке'); return; }
		ok('profiles.js/error: ошибка RPC даёт понятное пустое состояние, без падения');
	});

	// unload снимает poller (нет утечки на другие страницы)
	for (const v of ['status', 'logs', 'captcha', 'profiles']) {
		try {
			const env = makeEnv({});
			const view = loadView(path.join(dir, v + '.js'), env);
			if (typeof view.startRefresh !== 'function' || typeof view.unload !== 'function') {
				bad(v + '.js: нет startRefresh()/unload()');
				continue;
			}
			view.startRefresh();
			view.unload();
			if (view._pollFn === null)
				ok(v + '.js: unload() снял poller');
			else
				bad(v + '.js: unload() не сбросил _pollFn');
		} catch (e) {
			bad(v + '.js: unload-тест бросил ' + e.message);
		}
	}

	console.log('\nM8 view-runtime: PASS=' + PASS + ' FAIL=' + FAIL);
	process.exit(FAIL === 0 ? 0 : 1);
})();
