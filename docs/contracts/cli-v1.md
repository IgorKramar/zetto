# CLI и машинный контракт v1

Автор: Igor Kramar. 2026-10-29. **Принято Igor Kramar; команды v0.1 реализованы.**
Основание: ADR-0005–0008 и [format-v1](../format-v1.md).
Команда появляется вместе со своим JSON-ответом. Версия строки вывода
не зависит от версии SQLite или Cargo package.

## Запуск и общий ввод

`zetto [GLOBAL] COMMAND [OPTIONS]`. Без COMMAND запускается `tui`, только
если stdin и stdout — TTY. Иначе краткая справка в stderr, exit `2`,
терминальные режимы не включаются. `--help` и `--version` работают без vault.
`--no-interactive` запрещает вопросы и запуск редактора. В машинном режиме
вопросы также запрещены; `edit` явно запускает редактор только с TTY и
не допускается с `--no-interactive`. Редактор: пользовательский config argv,
иначе `$VISUAL`, иначе `$EDITOR`; если ничего не задано — invalid_config
с инструкцией настройки. Vault не задаёт argv.

Глобальные параметры:

| Параметр | Контракт |
|---|---|
| `--vault PATH` | Приоритет: флаг → `ZETTO_VAULT` → ближайшая `.zettorc` вверх от cwd → `~/.zetto/notes/`. Отсутствующий vault не создаётся скрыто. |
| `--format table\|json\|jsonl` | По умолчанию table; JSON/JSONL не содержат ANSI. `tui` несовместим с машинным выводом. |
| `--no-interactive` | Вопрос вместо обязательного аргумента становится ошибкой `interaction_required`. |
| `--color auto\|always\|never` | По умолчанию auto; без TTY/при `NO_COLOR` цвета выключены; явный always сильнее `NO_COLOR`. Машинные форматы всегда без цвета. |
| `--fresh`, `--cached` | Взаимоисключающие. С v0.2: full content check либо без сверки. Без флага — stat-сверка. В v0.1 полный разбор, cached недоступен (exit 2), свежесть `full`. |

Неизвестные команды/параметры — exit 2, без изменений. ID-аргументы — полные
ULID, никаких сокращений или неявного выбора alias. Пути — отдельные argv.
`capture/new`: ровно один источник TEXT, `--stdin` или `--file PATH`.
Ввод UTF-8, байты тела (включая конечные переводы строк) сохраняются;
невалидный UTF-8 → ошибка до изменения файлов. Пустое тело capture допустимо.
Никакого неявного чтения pipe без `--stdin`. При отсутствии источника
интерактивный ввод разрешён только в TTY; без него `interaction_required`.

## Команды по первой доступной версии

| Версия | Синтаксис | Результат / существенные условия |
|---|---|---|
| 0.1 | `init [PATH] [--root-title TEXT]` | Новый vault, default title `Корень`; config, каталоги и root. Существующий непустой PATH → конфликт без перезаписи. Не требует вопроса для default root. PATH и глобальный vault одновременно → ошибка. |
| 0.1 | `capture [TEXT] [--stdin\|--file PATH] [--title TEXT]` | ID/путь inbox; title по умолчанию пустой, без связи; `saved: true`. |
| 0.1 | `show ID [--raw]` | Данные выбранной заметки. raw — исходные байты, только table/plain; JSON + raw → usage. |
| 0.1 | `list [--stage published\|inbox\|all] [--tag TAG] [--limit N]` | По умолчанию published, 100 результатов, ID ascending → path. Не скрывает ошибочные файлы: у записи `id: null`, diagnostics. |
| 0.1 | `edit ID` | Устойчивая рабочая копия редактора; возврат saved/path/recovery. |
| 0.1 | `link SOURCE TARGET [--display TEXT]` | Добавляет каноническую ссылку в конец тела, не переформатирует остальное. Повтор уже существующей связи — no-op, `changed: false`. Self-link допустим с warning. |
| 0.1 | `publish ID [--link TARGET]` | Существующий draft, проверка, перенос в корень по плану. Повтор для уже published — no-op после проверки идентичности, не новый файл. |
| 0.1 | `new [TEXT] [--stdin\|--file PATH] [--title TEXT] [--link TARGET]` | Capture → editor/picker если TTY и нет источника тела → publish. stdin не запускает редактор. На неуспешную публикацию — сохранённый draft. |
| 0.1 | `tui` | Минимальный список/предпросмотр, capture/edit/link/publish. Завершение восстанавливает терминал. |
| 0.2 | `search QUERY [--literal\|--regex\|--fts] [--limit N]` | FTS по умолчанию с экранированием запроса; fts — явный расширенный синтаксис. Limit 50, сортировка relevance desc → ID → path. Пустой результат успешен. |
| 0.2 | `links ID [--direction incoming\|outgoing\|both]` | По умолчанию both, исходные targets, resolved IDs и причины неразрешения. |
| 0.2 | `graph ID [--depth N]` | По умолчанию 1, диапазон 1…3; локальные узлы и рёбра, признак усечения. |
| 0.2 | `tags` | Теги с counts, sort lexical. Inline tags — производное представление, без записи YAML. |
| 0.2 | `path ID... [--null]` | Абсолютные пути. В null-режиме ровно `path\0`, без заголовков; несовместим с json/jsonl. |
| 0.2 | `index rebuild` | Согласованная generation; не удаляет занятую базу. |
| 0.4 | `lint [ID] [--warnings-as-errors] [--fix --dry-run\|--apply PLAN]` | Весь vault либо заметка. Без fix не меняет заметки. Apply — только сохранённый план, сверяет хеши. |
| 0.5 | `retitle ID TITLE` | Сохраняет ID; title/slug изменяются по recovery-плану. До этой версии title можно менять через edit с диагностикой slug. |
| 0.5 | `tag add\|remove ID TAG` | Явное изменение tags; неверный исходный tags требует ручного разрешения. |
| 0.5 | `import SOURCE [--adapter markdown\|uuid] --dry-run\|--apply PLAN` | Dry-run сохраняет immutable mapping и отчёт; apply использует этот mapping, resume тем же PLAN. Исходники не меняет. |
| 0.5 | `export --to-markdown --output PATH [--dry-run]` | Отдельное новое дерево; отказ при существующей цели. |
| 0.5 | `attach ID SOURCE` | Скопировать в assets без выхода за vault и добавить Markdown-ссылку. |
| 0.5 | `resync [ID] --dry-run\|--apply PLAN` | Согласование slug только по сохранённому плану; чтение этого не делает. |
| 0.1 (полный инструментарий — 0.5) | `doctor [--operation ID --resume\|--restore]` | По умолчанию только диагностика; recovery работает без SQLite. До 0.5 достаточный минимальный механизм восстановления уже обязателен в файловом слое 0.1. |
| 0.5 | `completion SHELL`, `man` | Генерация из определения clap; shell bash/zsh/fish/elvish/powershell. |

Limit: положительное число, не более 10 000; результаты сверх него дают
`complete: false`, `truncated: true`, next offset. Пагинация `--offset N`
поддерживается list/search, default 0; между запросами generation может
измениться, стабильный snapshot между процессами не обещается. Прочие
коллекции сортируются по ID → path, рёбра дополнительно по source offset.
Graph при ограничении также возвращает `truncated: true`.

## JSON

Один документ и завершающий LF в stdout даже при ошибке:

```json
{
  "schema_version": 1,
  "status": "draft_saved",
  "saved": true,
  "data": {"id": "01ARZ3NDEKTSV4RRFFQ69G5FAV", "path": "inbox/01ARZ3NDEKTSV4RRFFQ69G5FAV.md"},
  "error": {"code": "publish_rejected", "message": "Нужна связь на опубликованную заметку"},
  "warnings": [],
  "diagnostics": [{"rule_id": "zetto/link-before-publish", "severity": "error", "path": "inbox/01ARZ3NDEKTSV4RRFFQ69G5FAV.md", "span": null, "message": "Нет подходящей связи", "fix": null}],
  "recovery": [],
  "freshness": {"mode": "full", "generation": null},
  "complete": true,
  "truncated": false
}
```

Exit `1`. `status`: `ok`, `draft_saved`, `error`. `saved` означает, что
пользовательское изменение подтверждено на диске, а не что прошла публикация.
Для read-only `saved: false`. `error: null` при успехе; warnings — объекты
`{code,message,path}` с nullable path. `recovery` — пути сохранённых копий/плана.
`complete` — ответ сформирован полностью, а не «всё структурно валидно»;
при ошибке обработки части vault/усечении false. `freshness.mode`:
`full`, `stat`, `cached`, `not_applicable`; generation nullable до SQLite.
Freshness описывает наблюдение, не обещает атомарный snapshot внешнего диска.
Диапазоны — по format-v1. JSON-paths относительны vault с `/`, кроме
команды path и внешних входов/выходов import/export; encoding — UTF-8.

JSONL: каждая строка — объект с `schema_version`, `type` и `data`.
`type`: `item`, `diagnostic`, `warning`, `summary`. Последняя `summary.data`
содержит status/saved/error/recovery/freshness/complete/truncated/count.
Без summary или с оборванной строкой поток неполон, даже если уже получены
items. Ошибка выдаёт summary с error и соответствующий exit. Сигнал может
помешать записать summary; клиент обязан проверять и поток, и код завершения.

Stdout содержит только выбранный формат; progress — stderr. В машинном
режиме диагностики присутствуют в stdout, stderr допустим только для
дополнительного человеческого progress, без смешивания JSON-строк.
Если stdout оборван (broken pipe), не писать повторно пользовательский файл
и не повторять операцию; exit `4`, сохранность файла от этого не отменяется.

## Ошибки и коды завершения

| Exit | Стабильные error codes | Смысл |
|---|---|---|
| 0 | `error: null`; warning `index_stale` возможен | Успех, в том числе нулевая выдача, no-op и сохранение при отказе индекса |
| 1 | `validation_failed`, `publish_rejected`, `not_found`, `unsupported_format` | Проверка/публикация не пройдена; new возвращает draft_saved с ID/путём |
| 2 | `usage`, `invalid_config`, `interaction_required` | Неверный вызов/настройка; обязательный скрытый вопрос запрещён |
| 3 | `conflict`, `lock_busy`, `duplicate_id`, `target_exists` | Конфликт версий/идентичности, блокировка; повторный ID не затирает файл |
| 4 | `io`, `editor_failed`, `recovery_required`, `index_unavailable` | I/O, редактор, незавершённая операция; последняя ошибка только если нет подтверждённой записи, иначе index_stale |
| 130 | `interrupted` | Управляемое прерывание; введённый и принятый текст в draft/recovery |

Error code описывает операцию, rule ID — конкретную проблему файла.
`lint` exit 1 при error; `--warnings-as-errors` добавляет warnings.
Неверный regexp/FTS query — usage. Текст message локализуем, code стабилен.
CLI-syntax ошибки должны сериализоваться адаптером clap в выбранный формат,
а не прерывать процесс встроенным human-only сообщением.

Добавление необязательных полей/новых кодов в minor допустимо; удаление,
переименование, смена типа или смысла поля — major schema. Клиенты должны
игнорировать неизвестные поля и неизвестные диагностические rule IDs.
