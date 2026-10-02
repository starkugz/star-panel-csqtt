#!/bin/sh
# SPDX-FileCopyrightText: 2026 amurcanov
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
#
# [M6a] Тесты rpcd-ucode бекенда + ACL + menu.d + JS-заглушек LuCI.
# [M6d] Добавлены: кросс-чек captcha.js (M4e helper API), анти-секрет static
# grep, i18n pot/ru-покрытие, QR round-trip (node + независимый python-декодер,
# опционально эталон segno).
# Чистый POSIX sh, root НЕ требуется: мок-окружение (fake csqtt/uci/logread/
# init.d + fixtures статуса/лога). Запуск:
#   sh openwrt/tests/luci/run.sh
# Инструменты: ucode (parse + функциональный прогон бекенда), jq (валидация
# ACL/menu.d), node --check (заглушки вью). Нет инструмента -> SKIP.

# shellcheck disable=SC2015
#   SC2015: A && ok || bad — ok/bad всегда exit 0 (только PASS/FAIL++/echo),
#   поэтому ветка bad ложно не срабатывает при успехе ok; паттерн согласован с
#   openwrt/tests/openwrt/run.sh (M5).
set -u

TESTS_DIR=$(cd "$(dirname "$0")" && pwd)
OPENWRT_DIR=$(cd "$TESTS_DIR/../.." && pwd)
APP="$OPENWRT_DIR/luci-app-csqtt"
BACKEND="$APP/root/usr/share/rpcd/ucode/csqtt"
ACL="$APP/root/usr/share/rpcd/acl.d/luci-app-csqtt.json"
MENUD="$APP/root/usr/share/luci/menu.d/luci-app-csqtt.json"
VIEWDIR="$APP/htdocs/luci-static/resources/view/csqtt"

WORK=$(mktemp -d) || exit 1
trap 'rm -rf "$WORK"' EXIT HUP INT TERM

PASS=0
FAIL=0
SKIP=0
ok() { PASS=$((PASS + 1)); echo "ok   - $1"; }
bad() { FAIL=$((FAIL + 1)); echo "FAIL - $1"; }
sk() { SKIP=$((SKIP + 1)); echo "SKIP - $1"; }

have() { command -v "$1" >/dev/null 2>&1; }

# --- 0. Наличие файлов -------------------------------------------------------
for f in "$BACKEND" "$ACL" "$MENUD"; do
	if [ -f "$f" ]; then ok "файл: $(basename "$f")"; else bad "нет файла $f"; fi
done
for v in status profiles captcha settings logs; do
	if [ -f "$VIEWDIR/$v.js" ]; then ok "вью-файл: $v.js"; else bad "нет $VIEWDIR/$v.js"; fi
done

# --- 1. ucode: синтаксис/парсинг бекенда -------------------------------------
if have ucode; then
	if ucode -c "$BACKEND" 2>"$WORK/parse.err"; then
		ok "ucode -c: бекенд компилируется без синтаксических ошибок"
	else
		bad "ucode -c: $(head -3 "$WORK/parse.err" | tr '\n' ' ')"
	fi
else
	sk "ucode не установлен — parse/func тесты пропущены"
fi

# --- 2. jq: валидация ACL ------------------------------------------------------
if have jq; then
	if jq -e . "$ACL" >/dev/null 2>&1; then ok "ACL: валидный JSON" || bad "ACL: невалидный JSON"; else bad "ACL: невалидный JSON"; fi
	jq -e '.["luci-app-csqtt"].read.ubus.csqtt | index("status")' "$ACL" >/dev/null 2>&1 \
		&& ok "ACL read: csqtt.status" || bad "ACL read: нет csqtt.status"
	jq -e '.["luci-app-csqtt"].read.ubus.csqtt | index("logs")' "$ACL" >/dev/null 2>&1 \
		&& ok "ACL read: csqtt.logs" || bad "ACL read: нет csqtt.logs"
	jq -e '.["luci-app-csqtt"].read.ubus.csqtt | index("test_conf")' "$ACL" >/dev/null 2>&1 \
		&& ok "ACL read: csqtt.test_conf" || bad "ACL read: нет csqtt.test_conf"
	jq -e '.["luci-app-csqtt"].write.ubus.csqtt | index("logs_clear")' "$ACL" >/dev/null 2>&1 \
		&& ok "ACL write: csqtt.logs_clear" || bad "ACL write: нет csqtt.logs_clear"
	jq -e '.["luci-app-csqtt"].write.ubus.csqtt | index("service")' "$ACL" >/dev/null 2>&1 \
		&& ok "ACL write: csqtt.service" || bad "ACL write: нет csqtt.service"
	jq -e '.["luci-app-csqtt"].read.uci.csqtt == true' "$ACL" >/dev/null 2>&1 \
		&& ok "ACL read: uci csqtt" || bad "ACL read: нет uci csqtt"
	jq -e '.["luci-app-csqtt"].write.uci.csqtt == true' "$ACL" >/dev/null 2>&1 \
		&& ok "ACL write: uci csqtt" || bad "ACL write: нет uci csqtt"
	# profile_export (несёт секрет) обязан быть ТОЛЬКО в write, не в read
	jq -e '.["luci-app-csqtt"].write.ubus.csqtt | index("profile_export")' "$ACL" >/dev/null 2>&1 \
		&& ok "ACL write: csqtt.profile_export" || bad "ACL write: нет csqtt.profile_export"
	jq -e '.["luci-app-csqtt"].read.ubus.csqtt | index("profile_export")' "$ACL" >/dev/null 2>&1 \
		&& bad "ACL read: profile_export НЕ должен быть read-only" || ok "ACL read: profile_export отсутствует (секрет)"
	jq -e '.["luci-app-csqtt"].read.ubus.csqtt | index("logs_clear")' "$ACL" >/dev/null 2>&1 \
		&& bad "ACL read: logs_clear не должен быть в read" || ok "ACL read: logs_clear отсутствует (только write)"
else
	sk "jq не установлен — ACL-валидация пропущена"
fi

# --- 3. jq: валидация menu.d ---------------------------------------------------
if have jq; then
	if jq -e . "$MENUD" >/dev/null 2>&1; then ok "menu.d: валидный JSON" || bad "menu.d: невалидный JSON"; else bad "menu.d: невалидный JSON"; fi
	jq -e '."admin/services/csqtt".action.type == "alias"' "$MENUD" >/dev/null 2>&1 \
		&& ok "menu.d: родитель CSQTT (alias)" || bad "menu.d: нет родителя alias"
	for item in status profiles captcha settings logs; do
		jq -e --arg p "admin/services/csqtt/$item" \
			'.[$p].action.type == "view" and (.[$p].action.path | startswith("csqtt/"))' \
			"$MENUD" >/dev/null 2>&1 \
			&& ok "menu.d: пункт $item (view)" || bad "menu.d: нет/неверен пункт $item"
	done
	# каждый leaf-пункт обязан ссылаться на ACL-грант (ненустой массив acl)
	for item in status profiles captcha settings logs; do
		jq -e --arg p "admin/services/csqtt/$item" \
			'(.[$p].acl | type=="array" and length>=1) and (.[$p].acl|index("luci-app-csqtt:read"))!=null' \
			"$MENUD" >/dev/null 2>&1 \
			&& ok "menu.d: $item привязан к ACL" || bad "menu.d: $item без ACL-гранта"
	done
else
	sk "jq не установлен — menu.d-валидация пропущена"
fi

# --- 4. node --check заглушек вью ----------------------------------------------
if have node; then
	for v in status profiles captcha settings logs; do
		if node --check "$VIEWDIR/$v.js" 2>"$WORK/node.err"; then
			ok "node --check: $v.js"
		else
			bad "node --check $v.js: $(head -1 "$WORK/node.err")"
		fi
	done
else
	sk "node не установлен — проверка JS пропущена"
fi

# --- 4b. [M6b] Кросс-чек: методы, вызываемые вью, есть в бекенде и ACL --------
# Формат вью: rpc.declare({ object: 'csqtt', method: '<m>' ... }) (строка одна).
METHODS=$(grep -hoE "object: *'csqtt', *method: *'[a-z_]+'" "$VIEWDIR/status.js" "$VIEWDIR/logs.js" \
	| sed -E "s/.*method: *'([a-z_]+)'.*/\1/" | sort -u)
if [ -n "$METHODS" ]; then
	ok "M6b: вью объявляют методы: $(echo "$METHODS" | tr '\n' ' ')"
else
	bad "M6b: в status.js/logs.js нет rpc.declare(object:'csqtt',...)"
fi
# shellcheck disable=SC2086  # METHODS — безопасные токены [a-z_], splitting намерен
for m in $METHODS; do
	grep -Eq "^[[:space:]]+$m: \{" "$BACKEND" \
		&& ok "M6b: метод $m есть в бекенде" || bad "M6b: метод $m не найден в бекенде"
	jq -e --arg m "$m" '.["luci-app-csqtt"].read.ubus.csqtt | index($m)' "$ACL" >/dev/null 2>&1 \
		|| jq -e --arg m "$m" '.["luci-app-csqtt"].write.ubus.csqtt | index($m)' "$ACL" >/dev/null 2>&1 \
		&& ok "M6b: метод $m разрешён ACL" || bad "M6b: метод $m отсутствует в ACL"
done
# status.js/logs.js больше не заглушки M6a
for v in status logs; do
	grep -q 'M6a] Заглушка' "$VIEWDIR/$v.js" \
		&& bad "M6b: $v.js всё ещё заглушка" || ok "M6b: $v.js — реальное вью"
done

# --- 4c. [M6c] Кросс-чек вью profiles.js/settings.js --------------------------
METHODS_C=$(grep -hoE "object: *'csqtt', *method: *'[a-z_]+'" "$VIEWDIR/profiles.js" "$VIEWDIR/settings.js" \
	| sed -E "s/.*method: *'([a-z_]+)'.*/\1/" | sort -u)
if [ -n "$METHODS_C" ]; then
	ok "M6c: вью объявляют методы: $(echo "$METHODS_C" | tr '\n' ' ')"
else
	bad "M6c: в profiles.js/settings.js нет rpc.declare(object:'csqtt',...)"
fi
# shellcheck disable=SC2086  # METHODS_C — безопасные токены [a-z_], splitting намерен
for m in $METHODS_C; do
	grep -Eq "^[[:space:]]+$m: \{" "$BACKEND" \
		&& ok "M6c: метод $m есть в бекенде" || bad "M6c: метод $m не найден в бекенде"
	jq -e --arg m "$m" '.["luci-app-csqtt"].read.ubus.csqtt | index($m)' "$ACL" >/dev/null 2>&1 \
		|| jq -e --arg m "$m" '.["luci-app-csqtt"].write.ubus.csqtt | index($m)' "$ACL" >/dev/null 2>&1 \
		&& ok "M6c: метод $m разрешён ACL" || bad "M6c: метод $m отсутствует в ACL"
done
# profiles.js/settings.js больше не заглушки M6a
for v in profiles settings; do
	grep -q 'M6a] Заглушка' "$VIEWDIR/$v.js" \
		&& bad "M6c: $v.js всё ещё заглушка" || ok "M6c: $v.js — реальное вью"
done
# секреты не должны эхо-иться в вью (password/vk_js_token — только пустые поля)
if grep -Eq "get\('(password|vk_js_token)'\)" "$VIEWDIR/profiles.js"; then
	bad "M6c: profiles.js читает секретное поле из UCI для показа"
else
	ok "M6c: profiles.js не подставляет password/vk_js_token в DOM"
fi
# [FIX] Новый профиль в LuCI по умолчанию включён: иначе пользователь вводит
# данные, профиль остаётся enabled=0 — соединения нет (дефект «первого запуска»).
if grep -Eq "chkInput\('csqtt-ed-enabled', isNew \? true" "$VIEWDIR/profiles.js"; then
	ok "M6c: новый профиль включён по умолчанию (enabled=1)"
else
	bad "M6c: новый профиль по умолчанию выключен — соединение не поднимется"
fi
# [FIX] Токен VK может прийти как implicit-flow redirect-URL
# (`...#access_token=vk1.a…`): LuCI обязан нормализовать vk_js_token при
# сохранении (извлечь токен, определить мусор), иначе клиент отправляет URL
# и VK отвечает API 5. Клиент делает то же самое (lib.rs).
if grep -q 'CSQTT_VK_TOKEN_BEGIN' "$VIEWDIR/profiles.js" \
	&& grep -q 'normalizeVkToken' "$VIEWDIR/profiles.js" \
	&& grep -Eq 'normalizeVkToken\(f\.vk_js_token\.value\)' "$VIEWDIR/profiles.js"; then
	ok "M6c: vk_js_token нормализуется (redirect-URL/пробелы) при сохранении"
else
	bad "M6c: vk_js_token не нормализуется — VK auth упадёт (API 5)"
fi
# Поведение LuCI и ядра должно совпадать: прогоняем реальные функции
# normalizeVkToken из profiles.js (node, без копии логики).
if have node; then
	if out=$(node "$TESTS_DIR/vk_token.js" "$VIEWDIR/profiles.js" 2>&1); then
		ok "M6c: $(echo "$out" | tail -1)"
	else
		bad "M6c: vk_token: $(echo "$out" | grep -m1 'FAIL' || echo провал)"
	fi
else
	sk "node не установлен — тест normalizeVkToken пропущен"
fi

# --- 4d. [M6d] Кросс-чек вью captcha.js (поверх M4e Web Helper API) -----------
METHODS_D=$(grep -hoE "object: *'csqtt', *method: *'[a-z_]+'" "$VIEWDIR/captcha.js" \
	| sed -E "s/.*method: *'([a-z_]+)'.*/\1/" | sort -u)
if [ -n "$METHODS_D" ]; then
	ok "M6d: captcha.js объявляет методы: $(echo "$METHODS_D" | tr '\n' ' ')"
else
	bad "M6d: в captcha.js нет rpc.declare(object:'csqtt',...)"
fi
# shellcheck disable=SC2086  # METHODS_D — безопасные токены [a-z_], splitting намерен
for m in $METHODS_D; do
	grep -Eq "^[[:space:]]+$m: \{" "$BACKEND" \
		&& ok "M6d: метод $m есть в бекенде" || bad "M6d: метод $m не найден в бекенде"
	jq -e --arg m "$m" '.["luci-app-csqtt"].read.ubus.csqtt | index($m)' "$ACL" >/dev/null 2>&1 \
		|| jq -e --arg m "$m" '.["luci-app-csqtt"].write.ubus.csqtt | index($m)' "$ACL" >/dev/null 2>&1 \
		&& ok "M6d: метод $m разрешён ACL" || bad "M6d: метод $m отсутствует в ACL"
done
grep -q 'M6a] Заглушка' "$VIEWDIR/captcha.js" \
	&& bad "M6d: captcha.js всё ещё заглушка" || ok "M6d: captcha.js — реальное вью"
# captcha_helper_url несёт одноразовую capability — только write (прецедент profile_export)
jq -e '.["luci-app-csqtt"].read.ubus.csqtt | index("captcha_helper_url")' "$ACL" >/dev/null 2>&1 \
	&& bad "M6d: captcha_helper_url не должен быть read-only" || ok "M6d: captcha_helper_url только write"
# статический запрет: вью капчи не оперирует секретными полями вообще
if grep -Eq 'session_token|success_token|vk_js_token|password' "$VIEWDIR/captcha.js"; then
	bad "M6d: captcha.js содержит запрещённое секретное поле"
else
	ok "M6d: captcha.js без запрещённых секретных полей (static grep)"
fi
# алгоритм капчи (PoW/VK API) в JS не переносится
if grep -Eq 'captchaNotRobot|solve_pow|api\.vk\.' "$VIEWDIR/captcha.js"; then
	bad "M6d: в captcha.js утёк алгоритм капчи (должен быть только UI M4e)"
else
	ok "M6d: captcha.js не содержит алгоритм капчи"
fi

# --- 4e. [M8] LuCI polling API ------------------------------------------------
# View lifecycle (luci.js) вызывает только load()/render() — НЕ poll(); глобал
# называется L.Poll, а не L.poll. Раньше status/logs/captcha полагались на
# poll(), из-за чего были вечный «loading…» и пустой журнал. Регресс-гвард:
if grep -Eq 'L\.poll\b|^[[:space:]]*poll[[:space:]]*: function' "$VIEWDIR"/*.js; then
	bad "M8: вью использует несуществующий L.poll / poll()-хук (нет авто-обновления)"
else
	ok "M8: нет L.poll/poll()-хука (корректный API — L.Poll)"
fi
for v in status logs captcha profiles; do
	grep -q 'L\.Poll\.add' "$VIEWDIR/$v.js" \
		&& ok "M8: $v.js регистрирует L.Poll.add" \
		|| bad "M8: $v.js без L.Poll.add (авто-обновление не работает)"
done
for v in status logs profiles; do
	grep -q 'startRefresh();' "$VIEWDIR/$v.js" \
		&& ok "M8: $v.js делает первый запрос в render" \
		|| bad "M8: $v.js не делает первый запрос (вечный loading…)"
done
# Бекенд: stderr CLI собирается в output (иначе LuCI видит только «ERROR (code N)»),
# и profile-id валидируется по правилам uci.rs (без дефиса).
grep -q '2>&1' "$BACKEND" \
	&& ok "M8: exec собирает stderr (2>&1) — причина ошибки CLI видна в LuCI" \
	|| bad "M8: stderr CLI не собирается (UI покажет только код)"
grep -q 'PROFILE_ID_RE' "$BACKEND" \
	&& grep -q 'isProfileId' "$BACKEND" \
	&& ok "M8: PROFILE_ID_RE/isProfileId (id секции без дефиса, как uci.rs)" \
	|| bad "M8: profile-id не валидируется отдельно (расхождение с uci.rs)"
# [LIVE FIX] В LuCI 24.10 нет глобала `dom` — только `L.dom`; bare `dom.`
# даёт ReferenceError в браузере и вечный «loading…»/«…».
if grep -Eq '(^|[^A-Za-z.])dom\.content' "$VIEWDIR"/*.js; then
	bad "M8: bare dom.content (нужно L.dom.content) — ReferenceError в браузере"
else
	ok "M8: нет bare dom. (используется L.dom)"
fi
# [M4e/M8] Клиентский helper URL — из origin браузера, не loopback.
if grep -q '127\.0\.0\.1' "$VIEWDIR/captcha.js"; then
	bad "M8: captcha.js хардкодит 127.0.0.1 (неверно для браузера)"
else
	ok "M8: captcha.js без 127.0.0.1 (client-facing URL из origin)"
fi
grep -q 'location.hostname' "$VIEWDIR/captcha.js" \
	&& ok "M8: captcha.js берёт LAN host из location.hostname" \
	|| bad "M8: captcha.js не использует location.hostname"
grep -q 'port: HELPER_PORT' "$BACKEND" \
	&& ok "M8: backend captcha_helper_info отдаёт port" \
	|| bad "M8: backend не отдаёт helper port"
# [i18n] EN-локаль: в JS не должно быть кириллицы вне комментариев (UI только _()).
# Явный набор букв не зависит от правил сортировки локали. Ошибка grep не является PASS.
LC_ALL=C.UTF-8 grep -nE '[АБВГДЕЁЖЗИЙКЛМНОПРСТУФХЦЧШЩЪЫЬЭЮЯабвгдеёжзийклмнопрстуфхцчшщъыьэюя]' "$VIEWDIR"/*.js > "$WORK/cyrillic.txt"
CYR_RC=$?
case "$CYR_RC" in
	0)
		grep -vE ':[[:space:]]*(//|\*|/\*)' "$WORK/cyrillic.txt" > "$WORK/cyrillic-code.txt"
		CYR_RC=$?
		case "$CYR_RC" in
			0) bad "i18n: кириллица вне комментариев в JS ($(head -1 "$WORK/cyrillic-code.txt" | cut -d: -f1))" ;;
			1) ok "i18n: нет hardcoded кириллицы в UI (только po/ru)" ;;
			*) bad "i18n: ошибка фильтрации комментариев (grep: $CYR_RC)" ;;
		esac
		;;
	1) ok "i18n: нет hardcoded кириллицы в UI (только po/ru)" ;;
	*) bad "i18n: проверка кириллицы не выполнена (grep: $CYR_RC)" ;;
esac
for v in status logs captcha profiles; do
	grep -q 'unload:' "$VIEWDIR/$v.js" \
		&& ok "M8: $v.js снимает poller в unload()" \
		|| bad "M8: $v.js не снимает poller (утечка на другие страницы)"
done

# --- 4f. [M8] runtime вью под node (эмуляция LuCI: load/render/L.Poll/unload) -
if have node; then
	if out=$(node "$TESTS_DIR/view-runtime.js" "$VIEWDIR" 2>&1); then
		ok "M8: view-runtime (load/render/L.Poll/unload) PASS"
	else
		bad "M8: view-runtime: $(echo "$out" | grep -m1 'FAIL' || echo провал)"
	fi
else
	sk "node нет — runtime вью пропущен"
fi

# --- 4g. [UI] Поведение Profiles/Settings (карточки, переключатели, сохранение)
# Profiles: компактный список карточек вместо широкой таблицы; раскрываемые
# «Подробности»; действия отдельной строкой; подтверждение удаления.
if grep -q "class: 'csqtt-list'" "$VIEWDIR/profiles.js" \
	&& ! grep -q "E('table'" "$VIEWDIR/profiles.js"; then
	ok "UI: profiles.js — список карточек вместо таблицы"
else
	bad "UI: profiles.js всё ещё таблица/нет списка карточек"
fi
for token in "class: 'csqtt-card" "E('details'" "class: 'csqtt-details'" \
	"class: 'csqtt-card-actions'" "handleRemove" "Delete profile"; do
	grep -qF "$token" "$VIEWDIR/profiles.js" \
		&& ok "UI: profiles.js содержит: $token" \
		|| bad "UI: profiles.js без: $token"
done
if grep -Eq "enabled \? _\('Enabled'\) : _\('Disabled'\)" "$VIEWDIR/profiles.js"; then
	ok "UI: переключатель профиля подписан Enabled/Disabled"
else
	bad "UI: нет явной подписи переключателя профиля"
fi
# Settings: boolean-переключатели, зависимые поля по режиму, состояния
# сохранения и понятная кнопка диагностики (без технического test_conf).
grep -q 'function boolSwitch' "$VIEWDIR/settings.js" \
	&& grep -q "boolSwitch('csqtt-set-enabled'" "$VIEWDIR/settings.js" \
	&& grep -q "boolSwitch('csqtt-set-failover'" "$VIEWDIR/settings.js" \
	&& ok "UI: settings.js использует переключатели для boolean" \
	|| bad "UI: settings.js без переключателей boolean"
grep -q 'syncDeps' "$VIEWDIR/settings.js" \
	&& grep -q 'ap.disabled = !manual' "$VIEWDIR/settings.js" \
	&& ok "UI: зависимое поле active_profile учитывает ручной режим" \
	|| bad "UI: нет зависимости active_profile от режима выбора"
for state in dirty saving saved error; do
	grep -q "setSaveState('$state'" "$VIEWDIR/settings.js" \
		&& ok "UI: состояние сохранения '$state' обрабатывается" \
		|| bad "UI: нет состояния сохранения '$state'"
done
if grep -q 'Run diagnostics (test_conf)' "$VIEWDIR/settings.js"; then
	bad "UI: техническое имя test_conf осталось в тексте кнопки"
else
	ok "UI: кнопка диагностики без технического имени test_conf"
fi
grep -q 'Run configuration check' "$VIEWDIR/settings.js" \
	&& ok "UI: понятная подпись кнопки диагностики" \
	|| bad "UI: нет понятной подписи кнопки диагностики"
# Сохранение без фактических изменений не должно показывать ложную ошибку
# (rpcd uci.apply возвращает NO_DATA при отсутствии изменений).
grep -q 'hasChanges: function' "$VIEWDIR/settings.js" \
	&& grep -q 'if (!this.hasChanges(data))' "$VIEWDIR/settings.js" \
	&& ok "UI: сохранение без изменений не вызывает uci.apply (без ложной ошибки)" \
	|| bad "UI: нет защиты от ложной ошибки при сохранении без изменений"

# --- 5. Функциональный прогон бекенда на моках (требует ucode) -----------------
if have ucode; then
	mkdir -p "$WORK/bin" "$WORK/out"
	# мок csqtt: status/doctor/profile ...
	cat > "$WORK/bin/csqtt" <<'MOCK'
#!/bin/sh
case "$1" in
  status) printf '{"version":"2.1.9","daemon_state":"running","captcha_pending":1,"profiles":[{"id":"p1","name":"Main","state":"captcha_required"}],"captcha_challenges":[{"id":"CH1","profile_id":"p1","mode":"slider","state":"pending","created_at":1,"expires_at":2}]}\n'; exit "${FAKE_STATUS_RC:-0}" ;;
   doctor) printf '{"checks":[{"name":"config","status":"ok","message":""}]}\n'; exit "${FAKE_DOCTOR_RC:-0}" ;;
  profile)
    case "$2" in
      export) printf 'csqtt://secret@example\n'; exit 0 ;;
      import) printf 'preview-ok link=%s\n' "$3"; exit 0 ;;
      *) printf 'OK: profile %s %s\n' "$2" "$3"; exit 0 ;;
    esac ;;
esac
exit 0
MOCK
	cat > "$WORK/bin/uci" <<'MOCK'
#!/bin/sh
# [M8] реальный `uci show csqtt` (текстовый формат, без -j)
cat <<'CFG'
csqtt.main=csqtt
csqtt.main.selection_mode='priority'
csqtt.main.active_profile='p1'
csqtt.routing=csqtt
csqtt.routing.mode='auto'
csqtt.p1=server
csqtt.p1.name='Main'
csqtt.p1.enabled='1'
csqtt.p1.priority='10'
csqtt.p1.peer='1.2.3.4:46000'
csqtt.p1.password='SECRET'
csqtt.p1.vk_js_token='TOK'
csqtt.p1.workers='18'
csqtt.p1.obfs='audio'
csqtt.p1.turn_transport='udp'
csqtt.p1.captcha_mode='auto'
csqtt.p1.fingerprint='chrome'
csqtt.p1.note='office'
CFG
MOCK
	cat > "$WORK/bin/logread" <<'MOCK'
#!/bin/sh
printf 'csqtt[1]: hello line\ncsqtt[1]: another\n'
MOCK
	cat > "$WORK/bin/csqtt-init" <<'MOCK'
#!/bin/sh
echo "init action: $1"; exit 0
MOCK
	chmod +x "$WORK/bin/"*
	printf 'l1 secret session_token=XXX\nl2 keep me\nl3 tail\n' > "$WORK/csqtt.log"
	printf '{"daemon_state":"running","captcha_pending":2,"profiles":[{"id":"p1","name":"Main","state":"captcha_required"}],"captcha_challenges":[{"id":"CH1","profile_id":"p1","mode":"slider","state":"pending","created_at":1,"expires_at":2},{"id":"CH2","profile_id":"p1","mode":"slider","state":"solved","created_at":1,"expires_at":2}]}\n' > "$WORK/status.json"

	TIMEOUT_BIN=$(command -v timeout || echo /usr/bin/timeout)
	export CSQTT_LUCI_BIN="$WORK/bin/csqtt" CSQTT_LUCI_INIT="$WORK/bin/csqtt-init" \
		CSQTT_LUCI_STATUS="$WORK/status.json" CSQTT_LUCI_LOG="$WORK/csqtt.log" \
		CSQTT_LUCI_UCI="$WORK/bin/uci" CSQTT_LUCI_LOGREAD="$WORK/bin/logread" \
		CSQTT_LUCI_TIMEOUT="$TIMEOUT_BIN" \
		CSQTT_LUCI_HELPER_HOST="127.0.0.1" CSQTT_LUCI_HELPER_PORT="18999" \
		CSQTT_LUCI_OUT="$WORK/out"

	# top-level `return {` -> `let __sig = {`, затем драйвер пишет JSON в $OUT
	sed 's/^return {$/let __sig = {/' "$BACKEND" > "$WORK/combined.uc"
	cat >> "$WORK/combined.uc" <<'UC'

function req(a) { return { args: (a != null ? a : {}), info: { acl: { user: "root" } } }; }
let OUT = getenv("CSQTT_LUCI_OUT");
function emit(name, v) { fs.writefile(OUT + "/" + name + ".json", sprintf("%.J", v)); }
let o = __sig.csqtt;
emit("status", o.status.call(req()));
emit("logs_file", o.logs.call(req({source:"file", lines:2})));
emit("logs_grep", o.logs.call(req({source:"file", lines:10, grep:"keep"})));
emit("logs_badregex", o.logs.call(req({source:"file", grep:"(["})));
emit("logs_syslog", o.logs.call(req({source:"syslog", lines:5})));
emit("service_ok", o.service.call(req({action:"restart"})));
emit("service_bad", o.service.call(req({action:"poweroff"})));
emit("test_conf", o.test_conf.call(req()));
emit("profiles", o.profiles.call(req()));
emit("profile_action", o.profile_action.call(req({action:"enable", id:"p1"})));
emit("profile_action_badid", o.profile_action.call(req({action:"enable", id:"../etc/passwd"})));
emit("profile_import", o.profile_import.call(req({link:"csqtt://x@h", commit:true})));
emit("profile_import_bad", o.profile_import.call(req({link:"http://evil"})));
emit("profile_export", o.profile_export.call(req({id:"p1"})));
 emit("captcha_list", o.captcha_list.call(req()));
 emit("captcha_cancel_unreach", o.captcha_cancel.call(req({id:"CH1"})));
 emit("captcha_helper_url_unreach", o.captcha_helper_url.call(req({id:"CH1"})));
 emit("captcha_helper_url_badid", o.captcha_helper_url.call(req({id:"../etc/passwd"})));
 emit("captcha_helper_info", o.captcha_helper_info.call(req()));
emit("logs_clear", o.logs_clear.call(req()));
UC
	if ucode "$WORK/combined.uc" 2>"$WORK/func.err"; then
		ok "ucode func: бекенд отработал все методы"
	else
		bad "ucode func: $(head -3 "$WORK/func.err" | tr '\n' ' ')"
	fi

	# jq-ассерты по результатам (если jq есть)
	if have jq; then
		J() { jq -e "$1" "$WORK/out/$2.json" >/dev/null 2>&1; }
		J '.connected==true and .code==0' status && ok "func: status connected" || bad "func: status"
		J '.count==2 and (.lines|index("l3 tail"))!=null' logs_file && ok "func: logs file tail" || bad "func: logs file"
		J '.count==1 and .lines[0]=="l2 keep me"' logs_grep && ok "func: logs grep" || bad "func: logs grep"
		J '.error!=null' logs_badregex && ok "func: logs bad regex отклонён" || bad "func: logs bad regex"
		J '.source=="syslog" and .count==2' logs_syslog && ok "func: logs syslog" || bad "func: logs syslog"
		J '.ok==true and .code==0' service_ok && ok "func: service restart" || bad "func: service restart"
		J '.ok==false and .error!=null' service_bad && ok "func: service whitelist" || bad "func: service whitelist"
		J '.ok==true and .report.checks[0].name=="config"' test_conf && ok "func: test_conf doctor" || bad "func: test_conf"
		J '.profiles[0].peer=="1.2.3.4:46000"' profiles && ok "func: profiles peer" || bad "func: profiles"
		J '.profiles[0].captcha_mode=="auto" and .profiles[0].note=="office"' profiles \
			&& ok "func: profiles note/captcha_mode (M6c safe-поля)" || bad "func: profiles note/captcha_mode"
		# секреты не должны просочиться в profiles
		if jq -e '.profiles[] | has("password") or has("vk_js_token")' "$WORK/out/profiles.json" >/dev/null 2>&1; then
			bad "func: profiles протекают секреты"
		else
			ok "func: profiles без секретов"
		fi
		if grep -q 'SECRET\|vk_js_token\|TOK' "$WORK/out/profiles.json"; then
			bad "func: в profiles.json есть подстрока секрета"
		else
			ok "func: profiles.json не содержит значений секретов"
		fi
		J '.ok==true' profile_action && ok "func: profile_action enable" || bad "func: profile_action"
		J '.ok==false and .error!=null' profile_action_badid && ok "func: profile_action id-валидация" || bad "func: profile_action id"
		J '.ok==true' profile_import && ok "func: profile_import preview" || bad "func: profile_import"
		J '.ok==false' profile_import_bad && ok "func: profile_import отклонил не-csqtt://" || bad "func: profile_import bad"
		J '.ok==true and (.link|startswith("csqtt://"))' profile_export && ok "func: profile_export link" || bad "func: profile_export"
		J '(.challenges|length)==1 and .challenges[0].id=="CH1"' captcha_list && ok "func: captcha_list фильтрует solved" || bad "func: captcha_list"
		# safe fields + анти-протечка секретов в payload капчи (M6d)
		J '.challenges[0] | has("profile") and has("mode") and has("state") and has("created_at") and has("expires_at")' captcha_list \
			&& ok "func: captcha_list отдаёт safe-поля (profile/mode/state/created/expires)" || bad "func: captcha_list safe-поля"
		if grep -Eq 'session_token|success_token|SECRET|TOK' "$WORK/out/captcha_list.json"; then
			bad "func: captcha_list протекают секреты"
		else
			ok "func: captcha_list без секретов"
		fi
		J '.ok==false and .error!=null' captcha_cancel_unreach && ok "func: captcha_cancel graceful (нет API)" || bad "func: captcha_cancel"
		J '.ok==false and .error!=null' captcha_helper_url_unreach && ok "func: captcha_helper_url graceful (нет API)" || bad "func: captcha_helper_url unreach"
		J '.ok==false and .error!=null' captcha_helper_url_badid && ok "func: captcha_helper_url id-валидация" || bad "func: captcha_helper_url badid"
		J '.reachable==false and .endpoint!=null' captcha_helper_info && ok "func: captcha_helper_info" || bad "func: captcha_helper_info"
		J '.ok==true and .cleared_bytes>0' logs_clear && ok "func: logs_clear" || bad "func: logs_clear"
		[ -s "$WORK/csqtt.log" ] && bad "func: лог не обнулён" || ok "func: лог-файлtruncate до 0"
	else
		sk "jq нет — ассерты функционального прогона пропущены"
	fi
else
	sk "ucode не установлен — функциональный прогон пропущен"
fi

# --- 6. Статические проверки безопасности бекенда ------------------------------
# 6a. [M8] popen — строкой, но КАЖДЫЙ argv через shellquote (array-popen
# отсутствует в ucode 2023.07.11 на устройстве); сырой ввод в shell запрещён.
if grep -Eq 'popen\(\s*\[' "$BACKEND"; then
	bad "security: array-popen не портируется на ucode устройства"
else
	ok "security: нет array-popen (строковый popen)"
fi
if grep -q 'shellquote(' "$BACKEND"; then
	ok "security: exec-аргументы экранируются shellquote"
else
	bad "security: exec без shellquote (shell-инъекция возможна)"
fi
# 6b. Нет system()/exec shell-обёрток
if grep -Eq '\bsystem\(|/bin/sh|sh -c|bash -c' "$BACKEND"; then
	bad "security: найдена shell-обёртка (system/sh -c)"
else
	ok "security: нет shell-обёрток в бекенде"
fi
# 6c. Пути зашиты (константы), ubus-аргументы не подставляются в путь exec
if grep -Eq 'run\(\[ *(req|a\.|request)' "$BACKEND"; then
	bad "security: ubus-аргумент попадает в argv[0] exec"
else
	ok "security: argv[0] exec — только константы путей"
fi

# --- 7. Live loopback API (captcha_cancel/info) — опционально, требует python3 --
if have ucode && have python3; then
	# python сам выбирает свободный эфемерный порт и пишет его в $WORK/port
	python3 - "$WORK/port" <<'PYSRV' &
import http.server, threading, socketserver, sys
class H(http.server.BaseHTTPRequestHandler):
    def _r(self): self.rfile.read(int(self.headers.get('Content-Length','0') or 0))
    def do_GET(self):
        self._r()
        if self.path.endswith('/helper-url'):
            b=('{"id":"CH1","url":"http://127.0.0.1:%d/c/CH1?cap=CAP-TEST"}' % self.server.server_address[1]).encode()
        else:
            b=b'[{"id":"CH1"}]'
        self.send_response(200); self.send_header('Content-Length',str(len(b))); self.end_headers(); self.wfile.write(b)
    def do_POST(self):
        self._r(); b=b'{"cancelled":true}' if self.path.endswith('/cancel') else b'{}'
        self.send_response(200); self.send_header('Content-Length',str(len(b))); self.end_headers(); self.wfile.write(b)
    def log_message(self,*a): pass
socketserver.TCPServer.allow_reuse_address = True
srv=socketserver.TCPServer(('127.0.0.1',0),H)
open(sys.argv[1],"w").write(str(srv.server_address[1]))
threading.Thread(target=srv.serve_forever,daemon=True).start()
import time; time.sleep(15)
PYSRV
	SRV=$!
	# ждем появления файла порта (до ~5с)
	i=0
	while [ "$i" -lt 50 ] && [ ! -s "$WORK/port" ]; do sleep 0.1; i=$((i + 1)); done
	if [ -s "$WORK/port" ]; then
		PORT=$(cat "$WORK/port")
		sed 's/^return {$/let __sig = {/' "$BACKEND" > "$WORK/live2.uc"
		cat >> "$WORK/live2.uc" <<'UC'

function req(a){ return { args:(a != null ? a : {}), info:{} }; }
let o = __sig.csqtt;
let c = o.captcha_cancel.call(req({id:"CH1"}));
let u = o.captcha_helper_url.call(req({id:"CH1"}));
let i = o.captcha_helper_info.call(req());
fs.writefile(getenv("CSQTT_LUCI_OUT") + "/live_cancel.json", sprintf("%.J", c));
fs.writefile(getenv("CSQTT_LUCI_OUT") + "/live_helper_url.json", sprintf("%.J", u));
fs.writefile(getenv("CSQTT_LUCI_OUT") + "/live_info.json", sprintf("%.J", i));
UC
		if CSQTT_LUCI_HELPER_PORT="$PORT" ucode "$WORK/live2.uc" 2>"$WORK/live.err"; then
			if have jq; then
				jq -e '.ok==true and .cancelled==true' "$WORK/out/live_cancel.json" >/dev/null 2>&1 \
					&& ok "live: captcha_cancel через loopback API" || bad "live: captcha_cancel"
				jq -e '.ok==true and (.url|test("/c/CH1\\?cap="))' "$WORK/out/live_helper_url.json" >/dev/null 2>&1 \
					&& ok "live: captcha_helper_url через M4e API (url с cap)" || bad "live: captcha_helper_url"
				jq -e '.reachable==true' "$WORK/out/live_info.json" >/dev/null 2>&1 \
					&& ok "live: captcha_helper_info reachable" || bad "live: captcha_helper_info"
			else
				sk "live: jq нет — ассерты пропущены"
			fi
		else
			bad "live: прогон упал: $(head -2 "$WORK/live.err" | tr '\n' ' ')"
		fi
	else
		sk "live: python-сервер не поднялся (порт не выбран)"
	fi
	kill "$SRV" 2>/dev/null || true
	wait "$SRV" 2>/dev/null || true
else
	sk "live API тест пропущен (нужны ucode+python3)"
fi

# --- 8. [M6d] i18n: pot/ru po покрыты и переводы полные -----------------------
POT="$APP/po/templates/csqtt.pot"
PO="$APP/po/ru/csqtt.po"
if [ -f "$POT" ]; then ok "i18n: po/templates/csqtt.pot существует"; else bad "i18n: нет po/templates/csqtt.pot"; fi
if [ -f "$PO" ]; then ok "i18n: po/ru/csqtt.po существует"; else bad "i18n: нет po/ru/csqtt.po"; fi
if [ -f "$POT" ] && [ -f "$PO" ]; then
	# msgid из вью (все пять) — каждый обязан быть в pot
	grep -hoE "_\('[^']*'\)" "$VIEWDIR"/*.js \
		| sed -e "s/^_(//" -e "s/)$//" -e "s/^'//" -e "s/'$//" | sort -u > "$WORK/ids_view.txt"
	grep -E '^msgid "' "$POT" | sed -e 's/^msgid "//' -e 's/"$//' -e 's/\\"/"/g' \
		| grep -v '^$' | sort -u > "$WORK/ids_pot.txt"
	if comm -23 "$WORK/ids_view.txt" "$WORK/ids_pot.txt" | grep -q .; then
		bad "i18n: msgid вью отсутствуют в pot: $(comm -23 "$WORK/ids_view.txt" "$WORK/ids_pot.txt" | head -3 | tr '\n' ';')"
	else
		ok "i18n: все msgid вью (status/profiles/captcha/settings/logs) есть в pot"
	fi
	# pot и ru po имеют один набор msgid
	grep -E '^msgid "' "$PO" | sed -e 's/^msgid "//' -e 's/"$//' -e 's/\\"/"/g' \
		| grep -v '^$' | sort -u > "$WORK/ids_po.txt"
	if diff -q "$WORK/ids_pot.txt" "$WORK/ids_po.txt" >/dev/null 2>&1; then
		ok "i18n: набор msgid pot == набор ru po"
	else
		bad "i18n: наборы msgid pot и ru po расходятся"
	fi
	# полный русский: ни одного пустого msgstr (кроме заголовка)
	if awk '/^msgid "/{if ($0 != "msgid \"\"") id=1} /^msgstr ""$/{if (id) {print "EMPTY"; id=0}}' "$PO" | grep -q EMPTY; then
		bad "i18n: в ru po есть непереведённые (пустые) msgstr"
	else
		ok "i18n: ru po — все msgstr непустые (полный перевод)"
	fi
	# %s-плейсхолдеры сохранены в переводах
	if awk '/^msgid "/{m=$0} /^msgstr "/{nm=gsub(/%s/,"",m); ns=gsub(/%s/,"",$0); if (nm!=ns) print "MISMATCH"}' "$PO" | grep -q MISMATCH; then
		bad "i18n: количество %s в msgid/msgstr расходится"
	else
		ok "i18n: %s-плейсхолдеры сохранены"
	fi
	if have msgfmt; then
		if msgfmt --check-format -o /dev/null "$PO" 2>"$WORK/msgfmt.err"; then
			ok "i18n: msgfmt --check-format PASS"
		else
			bad "i18n: msgfmt: $(head -2 "$WORK/msgfmt.err" | tr '\n' ' ')"
		fi
	else
		sk "msgfmt не установлен — синтаксис po не проверен"
	fi
fi

# --- 9. [M6d] QR-генератор captcha.js: round-trip + эталон (node+python3) ------
if have node && have python3; then
	if node "$TESTS_DIR/qr-roundtrip.js" "$VIEWDIR/captcha.js" > "$WORK/qr.json" 2>"$WORK/qrjs.err"; then
		if python3 "$TESTS_DIR/qr-roundtrip.py" < "$WORK/qr.json" > "$WORK/qr.out" 2>&1; then
			while IFS= read -r line; do
				case "$line" in
					ok\ *) ok "QR: ${line#ok - }" ;;
					note\ *) sk "QR: ${line#note - }" ;;
					*) bad "QR: $line" ;;
				esac
			done < "$WORK/qr.out"
		else
			bad "QR: декодер сообщил о сбоях: $(grep -c '^FAIL' "$WORK/qr.out")"
			grep '^FAIL' "$WORK/qr.out" | head -3
		fi
	else
		bad "QR: генератор упал: $(head -2 "$WORK/qrjs.err" | tr '\n' ' ')"
	fi
else
	sk "node/python3 не установлены — QR-тест пропущен"
fi

# --- Итог ----------------------------------------------------------------------
echo
echo "M6a-M6d luci tests: PASS=$PASS FAIL=$FAIL SKIP=$SKIP"
if [ "$FAIL" -ne 0 ]; then
	echo "RESULT: FAIL"
	exit 1
fi
echo "RESULT: PASS"
