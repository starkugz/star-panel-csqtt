---
name: Bug report
about: Сообщить о проблеме (сеть, LuCI, сборка, установка)
title: '[bug] '
labels: bug
assignees: ''
---

<!--
ВАЖНО: НЕ прикладывайте секреты — password, vk_js_token, VK-хеши, device_id,
токены CAPTCHA, реальные IP/URL. Секреты в issue = инцидент (см. docs/SECURITY.md).
-->

## Что не работает

Кратко опишите проблему.

## Ожидаемое поведение

## Как воспроизвести

1.
2.

## Окружение

- Устройство / платформа:
- OpenWrt:
- Версия CSQTT (`csqtt doctor` / статус):
- Режим авторизации (`vkcalls` / `auto_js`):

## Диагностика (без секретов)

```
# csqtt doctor
# ubus call csqtt status
# logread | grep csqtt | tail -50
```

## Дополнительно

Скриншоты LuCI (без секретов), конфиг с замаскированными значениями.
