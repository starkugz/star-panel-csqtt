'use strict';
// Тест синхронизации нормализации VK-токена в LuCI (profiles.js) с ядром
// (csqtt-openwrt/lib.rs). Функции извлекаются из реального файла по маркерам
// CSQTT_VK_TOKEN_BEGIN/END и исполняются в изоляции — тестируется именно
// поставляемый код, без копии логики в тесте.
//
// Использование: node vk_token.js <path/to/profiles.js>
// Вывод: "vk-token: N/N passed" (exit 0) либо список падений (exit 1).

const fs = require('fs');

const target = process.argv[2];
if (!target || !fs.existsSync(target)) {
	console.error('usage: node vk_token.js <profiles.js>');
	process.exit(2);
}

const source = fs.readFileSync(target, 'utf8');
const block = source.match(/\/\/ CSQTT_VK_TOKEN_BEGIN\n([\s\S]*?)\/\/ CSQTT_VK_TOKEN_END/);
if (!block) {
	console.error('FAIL: markers CSQTT_VK_TOKEN_BEGIN/END not found');
	process.exit(1);
}

let api;
try {
	api = new Function(block[1] + '\nreturn { normalizeVkToken: normalizeVkToken };')();
} catch (e) {
	console.error('FAIL: cannot evaluate normalizer: ' + e.message);
	process.exit(1);
}

const normalize = api.normalizeVkToken;

const cases = [
	// [input, expected]
	['', ''],
	['   \n\t ', ''],
	['  vk1.a.plain  ', 'vk1.a.plain'],
	['vk1.a.pad==', 'vk1.a.pad=='],
	['vk1.a.AB_CD-EF.gh', 'vk1.a.AB_CD-EF.gh'],
	['https://oauth.vk.ru/blank.html#access_token=vk1.a.AbCdEf-123_456&expires_in=0&user_id=1',
		'vk1.a.AbCdEf-123_456'],
	['https://oauth.vk.com/blank.html?access_token=vk1.a.xyz&expires_in=0', 'vk1.a.xyz'],
	['https://example.test/cb?token=vk1.a.query-tok&state=1', 'vk1.a.query-tok'],
	['result:https://x/#vk1.a.only-token&foo=bar', 'vk1.a.only-token'],
	['https://oauth.vk.ru/blank.html#access_token=vk1.a.AbC%2DdeF%5Fgh&expires_in=0',
		'vk1.a.AbC-deF_gh'],
	['https://x/?token=vk1.a.tok&access_token=vk1.a.acc', 'vk1.a.acc'],
	['https://x/?access_token=vk1.a.first&access_token=vk1.a.last', 'vk1.a.last'],
	// Пусто/отсутствует access_token/мусор -> null.
	['https://oauth.vk.ru/blank.html?error=access_denied', null],
	['https://oauth.vk.ru/blank.html', null],
	['hello world', null],
	['not-a-token', null],
	['vk1', null],
	['access_token=vk1.a.abc def', null],
];

let failed = 0;
cases.forEach(function(testCase, index) {
	const input = testCase[0], expected = testCase[1];
	let actual;
	try {
		actual = normalize(input);
	} catch (e) {
		actual = 'THREW: ' + e.message;
	}
	if (actual !== expected) {
		failed++;
		console.error('FAIL #%d: input=%j expected=%j actual=%j',
			index + 1, input, expected, actual);
	}
});

// Исторический hex-токен (80+ hex) принимается.
const legacy = 'a1B2c3D4e5'.repeat(9);
if (normalize(legacy) !== legacy) {
	failed++;
	console.error('FAIL: legacy hex token rejected');
}

if (failed > 0) {
	console.error('vk-token: %d/%d failed', failed, cases.length + 1);
	process.exit(1);
}
console.log('vk-token: %d/%d passed', cases.length + 1, cases.length + 1);
