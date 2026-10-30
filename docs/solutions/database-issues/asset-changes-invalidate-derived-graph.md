---
title: "Изменение вложения должно обновлять производный граф"
date: "2026-10-30"
category: database-issues
module: "zetto index"
problem_type: database_issue
component: database
severity: medium
symptoms:
  - "После появления вложения ссылка остаётся broken в cached-графе"
root_cause: logic_error
resolution_type: code_fix
tags: ["sqlite", "invalidation", "graph", "assets"]
---

# Изменение вложения должно обновлять производный граф

Автор: Igor Kramar. Исправление проверено в ветке v0.2; слияние ещё не заявляется.

## Problem

Ссылка `[вложение](assets/test.txt)` меняет состояние при появлении или удалении
вложения. Это пример пути внутри vault, файла `assets/test.txt` в репозитории нет.
При этом размер, mtime и хеш всех Markdown-файлов могут остаться прежними.

## Symptoms

- Пересчитанный в памяти каталог уже видит attachment вместо broken,
  а cached SQLite продолжает возвращать прежнее ребро.

## What Didn't Work

Флаг изменения только по штампам и хешам заметок не учитывает зависимость
разрешения ссылки от существования вложения. Сам повторный resolver недостаточен,
если его новый результат не публикуется в SQLite.

## Solution

В [индексе](../../../src/index.rs) сравнивается хеш производного каталога
с предыдущим значением. Он включает разрешения и диагностики:

```rust
let derived_digest = hash(encode(&catalog.records)?.as_bytes());
changed |= base.derived_digest != derived_digest;
```

При изменении публикуется согласованное поколение, даже если `files_read == 0`.
Это часть уже выбранного [ADR-0009](../../architecture/decisions/0009-publish-derived-index-generations.md),
отдельный механизм инвалидации не нужен.

## Why This Works

Изменение входного файла и изменение производного результата — разные события.
Проверка результата охватывает внешнюю зависимость от вложений, сохраняя
статическую оптимизацию чтения неизменённых заметок. Полный проход по метаданным
остаётся осознанной границей v0.2.

## Prevention

Проверять появление и удаление вложения без редактирования заметки: ноль чтений
Markdown, новая generation при появлении, правильный тип ребра и равенство
живого/cached графа. Регрессия
`alias_collision_and_asset_lifecycle_refresh_persisted_edges` находится в
[tests/index.rs](../../../tests/index.rs).

## Related Issues

- [Отчёт проверки v0.2](../../architecture/reviews/2026-10-30-v0.2-verification.md).
