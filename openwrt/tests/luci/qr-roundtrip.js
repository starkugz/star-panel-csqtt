'use strict';
// [M6d] Тестовый генератор: вырезает автономный QR-блок из captcha.js,
// кодирует сэмплы (включая реалистичный helper-URL M4e) и печатает матрицы
// в JSON (stdout). Парная проверка — qr-roundtrip.py (независимый декодер).
// Запуск: node qr-roundtrip.js <captcha.js>
const fs = require('fs');
const file = process.argv[2];
if (!file) {
	console.error('usage: node qr-roundtrip.js <captcha.js>');
	process.exit(2);
}
const src = fs.readFileSync(file, 'utf8');
const start = src.indexOf('/* QR-BEGIN');
const end = src.indexOf('/* QR-END');
if (start < 0 || end < 0) {
	console.error('QR block markers not found');
	process.exit(2);
}
const body = src.slice(start, end);
const code = body.slice(body.indexOf('*/') + 2);
const qr = new Function(code + '\nreturn { qrMatrix: qrMatrix };')();
const samples = [
	'http://192.168.1.10:8443/c/AbCdEf0123456789?cap=Zm9vYmFyYmF6cXV1eDEyMzQ1Njc4OTBhYmNkZWY',
	'http://10.0.0.5:8443/c/x1y2z3?cap=short',
	'https://example.com/very/long/path?token=' + 'x'.repeat(80),
	'HELLO',
	'A'.repeat(14),
	'x'.repeat(122),
];
const out = samples.map(function(s) {
	const q = qr.qrMatrix(s);
	return {
		text: s,
		version: q.version,
		mask: q.mask,
		size: q.size,
		rows: q.modules.map(function(r) { return r.map(function(v) { return v ? 1 : 0; }).join(''); }),
	};
});
process.stdout.write(JSON.stringify(out));
