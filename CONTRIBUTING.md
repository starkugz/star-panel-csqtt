# Contributing

Спасибо за интерес к проекту. Ниже — правила, которые важно соблюдать, чтобы
изменения не ломали сетевой контракт и приватность пользователей.

## Принципы (не нарушать)

1. **M3X — interface-only.** Служба CSQTT не становится default-шлюзом, не ставит
   route/rule/table, не меняет WAN/DNS/dnsmasq/firewall/NAT, не захватывает LAN.
   Трафик в туннель — только через `SO_BINDTODEVICE` (`interface-name: csqtt0`).
2. **Секреты.** Никогда не коммитьте `password`, `vk_js_token`, VK-хеши,
   `device_id`, токены CAPTCHA, реальные IP/URL инфраструктуры. В тестах и
   примерах — только плейсхолдеры и документационные адреса
   (`198.51.100.0/24`, `203.0.113.0/24`). См. [`docs/SECURITY.md`](docs/SECURITY.md).
3. **`device_id` привязан к паролю** на сервере — не регенерировать.
4. **musl-фиксы** (`tun_ioctl`, `udp_batch`, clap `help`) не откатывать.
5. **`MAX_WORKERS=126`**; `workers` — любое `9..126` (нормализуется к кратному 9).

## Окружение разработки

Linux/WSL с ext4 (SDK ломает симлинки на drvfs/9p). Нужны: `build-essential`,
`pkg-config`, `cmake`, `python3`, `curl`, `file`, `zstd`, `git`, `shellcheck`,
Rust + target `aarch64-unknown-linux-musl`.

```sh
rustup target add aarch64-unknown-linux-musl
```

## Сборка и тесты

```sh
sh scripts/build-all.sh                              # ядро + .apk → dist/
(cd csqtt-openwrt && bash scripts/run-tests.sh)      # fmt + clippy + unit (обязательно)
sh openwrt/tests/openwrt/run.sh                      # M5
sh openwrt/tests/luci/run.sh                         # M6
sh openwrt/tests/packages/verify-apk.sh              # M7
SSHHOST=<ip> SSHPASS=… sh scripts/smoke-test.sh      # M8 live (опционально)
```

PR должен проходить `run-tests.sh` и (по возможности) M5–M7. Любой FAIL — блокер.

## Стиль кода

- Rust: `cargo fmt`, `cargo clippy --all-targets -- -D warnings` без замечаний.
- LuCI JS: только `L.dom`, строки через `_('English source')`; **кириллица в JS
  вне комментариев запрещена** (проверяется M6-тестом). Новые строки — в
  `po/templates/csqtt.pot` и `po/ru/csqtt.po` (полный перевод, число `%s`
  совпадает).
- Обновляйте [`docs/CONFIG.md`](docs/CONFIG.md) при изменении опций UCI.
- Держите комментарии по существу; не добавляйте «шумные» комментарии.

## Коммиты и PR

- Небольшие тематические коммиты с понятным сообщением в императиве.
- В PR опишите: что меняется, зачем, как проверено (команды/результаты).
- Обновляйте [`CHANGELOG.md`](CHANGELOG.md) для заметных изменений.
- Для релиза — см. [`docs/RELEASE.md`](docs/RELEASE.md).

## Лицензия вклада

Отправляя изменения, вы соглашаетесь лицензировать их на условиях
PolyForm Noncommercial License 1.0.0 (см. [`LICENSE`](LICENSE)) и подтверждаете,
что не включаете сторонний код без совместимой лицензии.

## Сообщения об уязвимостях

Не публикуйте секреты/эксплойты в issue. См. раздел «Реакция на инцидент» в
[`docs/SECURITY.md`](docs/SECURITY.md).
