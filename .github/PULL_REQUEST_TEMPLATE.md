## Что меняется

<!-- Кратко: суть изменений и зачем. -->

## Проверено

- [ ] `(cd csqtt-openwrt && bash scripts/run-tests.sh)` — зелёный
- [ ] `sh openwrt/tests/openwrt/run.sh` (M5)
- [ ] `sh openwrt/tests/luci/run.sh` (M6)
- [ ] `sh openwrt/tests/packages/verify-apk.sh` (M7)

## Контракт

- [ ] Не нарушает **interface-only (M3X)**: нет route/rule/table, WAN/DNS/dnsmasq/
      firewall/NAT не меняются, LAN не захватывается.
- [ ] Нет секретов/реальной инфраструктуры в коде, тестах, фикстурах, доках
      (пароль, `vk_js_token`, VK-хеши, `device_id`, токены, IP peer'а).
- [ ] `docs/CONFIG.md`/`CHANGELOG.md` обновлены при необходимости.