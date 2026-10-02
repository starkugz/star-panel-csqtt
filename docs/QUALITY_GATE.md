# QUALITY_GATE — low-context v2

Выполнять только после DoD и зелёных тестов текущего модуля.

Основной агент реально вызывает subagents через task/subagent. Ответы subagent не имитировать.

## Перед запуском

Сначала обновить `docs/CURRENT.md`:
- текущий модуль;
- список изменённых файлов;
- краткий итог тестов;
- известные blockers/risks.

Не прикладывать subagent целиком большие документы, diff или logs.

## Порядок

1. `rust-review`
2. `isolation-audit`
3. `review`

Каждому передавать только:
- `CURRENT_MODULE=<module>`;
- путь `docs/CURRENT.md`;
- список изменённых файлов;
- краткий итог тестов;
- путь `docs/prompts/<module>.md`.

Subagent сам может точечно открыть `docs/reference/<module>.md`, compact `PROGRESS.md` и нужные hunks.

## Если BLOCKED

- исправить только обоснованные пункты текущего модуля;
- повторить затронутые тесты;
- обновить `docs/CURRENT.md`;
- повторить соответствующий audit;
- после существенного Rust/network изменения повторить также затронутые предыдущие audits.

Отчёт каждого subagent: максимум ~60 строк.

## PASS

Модуль завершён только при:
- обязательные тесты PASS;
- `rust-review = PASS`;
- `isolation-audit = PASS`;
- `review = PASS`;
- `docs/history/<module>.md` содержит компактный финальный журнал;
- корневой `PROGRESS.md` обновлён кратко;
- `docs/reference/<module>.md` обновлён, если это требует DoD;
- `docs/CURRENT.md` помечен `DONE`.

После этого ОСТАНОВИТЬСЯ. Следующий модуль не начинать в этом чате.
