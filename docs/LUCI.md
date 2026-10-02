# LUCI — страницы, backend, i18n

LuCI-приложение `luci-app-csqtt` (персональная редакция **star-panel-csqtt**
1.0.0): 6 вью на современном LuCI (JS + `menu.d` + `rpcd-ucode`), без
Lua-контроллеров. См. также [`ARCHITECTURE.md`](ARCHITECTURE.md),
[`CONFIG.md`](CONFIG.md).

## 1. Структура

```
openwrt/luci-app-csqtt/
├─ htdocs/luci-static/resources/view/csqtt/
│    status.js  profiles.js  settings.js  logs.js  captcha.js  about.js
├─ root/usr/share/luci/menu.d/luci-app-csqtt.json      # меню/ACL-метки
├─ root/usr/share/rpcd/acl.d/luci-app-csqtt.json       # права ubus/uci
├─ root/usr/share/rpcd/ucode/csqtt                     # ubus-объект csqtt
├─ po/templates/csqtt.pot                              # EN source
├─ po/ru/csqtt.po                                      # RU перевод
└─ Makefile
```

Меню: `admin/services/csqtt/{status,profiles,captcha,settings,logs,about}`.
ACL: `luci-app-csqtt:read` для вью; запись — через ubus-методы/uci.

## 2. Вью

| Страница | Файл | Назначение |
|---|---|---|
| Status | `status.js` | состояние профиля, `csqtt0`, traffic, кнопки service |
| Profiles | `profiles.js` | список серверов, редактирование (в т.ч. advanced: `vk_hash_mode`, `vk_js_token`), **Check hashes** (валидация + подсветка: рабочие зелёным, нерабочие красным), import/export `csqtt://`, QR |
| Settings | `settings.js` | глобальные `main`/`routing`, пороги/health/captcha_policy |
| Logs | `logs.js` | хвост `/var/log/csqtt.log`, очистка |
| CAPTCHA | `captcha.js` | live-challenges, отмена, Web Helper URL/QR |
| About | `about.js` | версии редакции (`star-panel-csqtt` 1.0.0) и ядра (live из `status.json`), автор редакции, исходный проект/лицензия/уведомления |

## 3. rpcd-ucode backend (ubus-объект `csqtt`)

Методы (ACL см. выше):

- read: `status`, `logs`, `test_conf`, `profiles`, `hashes_validate`,
  `captcha_list`, `captcha_helper_info`
- write: `logs_clear`, `service`, `test_conf`, `profile_action`,
  `profile_import`, `profile_export`, `captcha_cancel`, `captcha_helper_url`

Бэкенд вызывает `/usr/bin/csqtt` через `popen` с экранированием аргументов и
таймаутом; секреты не возвращает. Runtime-состояние читается из
`/var/run/csqtt/status.json`.

## 4. Правила вью

- DOM — только через `L.dom` (никаких прямых `innerHTML`/строк).
- Поллинг — `L.Poll`; применение настроек — `uci.save().then(() => uci.apply())`.
- Строки — только `_('…')`, английский source; **кириллица в JS запрещена**
  (кроме комментариев) — это проверяет M6b-тест.
- Поля-секреты (`password`, `vk_js_token`) — write-only: пустое значение
  сохраняет прежнее; значение никогда не читается обратно.

## 5. i18n

1. Строки в JS — `_('English source')`.
2. Обновите шаблон и перевод:
   - `po/templates/csqtt.pot` — source-строки;
   - `po/ru/csqtt.po` — русский перевод.
3. При сборке SDK host-`po2lmo` компилирует `.po` → `.lmo` (пакет
   `luci-i18n-csqtt-ru`). Обязательна пустая строка заголовка `.po`; иначе
   `msgfmt`/`po2lmo` падает (проверено в `build-sdk.sh`).

## 6. Как добавить страницу/строку

- **Строку:** обернуть в `_()`, добавить в `.pot` и `.po`, пересобрать SDK.
- **Поле/опцию:** сначала в `csqtt-openwrt/uci.rs` (тип/валидация) и
  `docs/CONFIG.md`; затем UI-виджет во вью и, при необходимости, ACL-метод.
- **Страницу:** новый `view/csqtt/<name>.js` + запись в `menu.d` (title/action/
  acl) и, если нужно, новый ubus-метод + запись в `acl.d`.

## 7. Live-проверка (M8)

Живые проверки вью выполняются headless-браузером (Chrome for Testing) через
CDP в WSL: регистрируется вход, открываются вкладки, проверяются XHR к ubus,
i18n (RU/EN) и стабильность. Чекпоинт — `docs/LUCI_LIVE_CHECKPOINT.md`.
