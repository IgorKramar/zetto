# Architecture

> **What this document is.** The living architectural state of this project — what the system is, what it must satisfy, how it's structured today, what decisions got it here, and what's still open. It is updated whenever any of those things change.
>
> **What it is not.** A design document, a roadmap, or a sales pitch. Keep it short, current, and honest. Stale or aspirational content here is worse than no content.
>
> **How to update it.** Touch this file in the same change as the code/ADR that changes its state. If a section is no longer accurate, fix it now — not "later".

---

## 1. System summary

`zetto` — проект локального CLI/TUI для связанной базы знаний на Rust.
Принят контракт Markdown-хранилища, общего ядра и разделения capture/publish.
Первичный пользователь работает в vim/tmux/git. CLI и TUI равноправны;
rg/fzf — необязательные внешние инструменты. Сохранение мысли в inbox
не требует связи, публикация требует связи на другую опубликованную заметку
с одним явным root-исключением.

**Текущее состояние:** реализован v0.2.0: CLI/JSON, SQLite/FTS5,
stat/full/cached, поиск, граф и фоновый TUI с notify.
Безопасная запись и файловое восстановление действуют с v0.1.
Контракты этапа 0 приняты владельцем 2026-10-29; приёмка приложения
отделена от реализации и публикации релиза.
[План v0.2](docs/plans/2026-10-30-v0.2-search-index-concurrency.md).

Проект переименован из `zk` в `zetto` в [ADR-0001](./docs/architecture/decisions/0001-project-name-and-ecosystem-positioning.md) (2026-05-09).

См. [`STRATEGY.md`](./STRATEGY.md) для полного контекста.

---

## 2. Quality attributes

| Атрибут | Принятый критерий | Проверка |
|---|---|---|
| Capture | Подтверждённое сохранение без необходимости немедленно выбрать связь | Машинное время отдельно от пользовательского сценария <5 сек |
| Publish | Проверки структуры и связности перед переносом | Время системы отдельно от выбора связи |
| Сохранность | Backup/recovery до замены, no-clobber при создании, обнаруженные конфликты сохраняются | Реальная ФС и аварийные сценарии v0.1; [ADR-0006](docs/architecture/decisions/0006-safe-writes-and-recovery.md) |
| Свежесть | Явные full/stat/cached; индекс производный | Равенство full/incremental, alias/rename/delete, смена git-ветки, busy и cached snapshots |
| Ресурсы и масштаб | Startup/search/capture/idle CPU/RAM на фиксированных корпусах 5k/50k | Корпус v2: около 4 KiB, 3–10 связей; инструменты tools/benchmark_cli.py и tools/corpus.py |
| Поддерживаемая среда | Linux/macOS, локальная ФС, один автор и несколько процессов | Платформенная/PTY матрица до 1.0 |

### 2.1 Системные бюджеты

Исходные ориентиры сохраняются как бюджеты для измерений, а не результаты
эксперимента: startup <50 ms p50; lookup <100 ms; render/input <200 ms;
picker <500 ms; publish-check <100 ms; подтверждённая запись <300 ms.
Время набора и осмысления пользователем в эти значения не входит.
Capture не ждёт полной сверки графа. Обход/stat измеряется отдельно;
превышение бюджета требует измеренного исправления либо явного пересмотра.
Протокол не обещает общей транзакции с внешним редактором или защиту
от физической поломки диска; recovery дополняет независимый backup.

---

## 3. High-level structure

По [ADR-0007](docs/architecture/decisions/0007-shared-cli-tui-core.md)
приложение — один Cargo package с библиотекой и бинарником.
`cli`/`tui` вызывают `app`; операции используют `note`/`format`, `vault`,
`editor` и `config`. `app` не зависит от stdout, клавиатуры или terminal UI.
`vault` отвечает за пути, блокировки, backup и recovery; `format` не знает ФС.
В v0.2 `index` хранит производный SQLite/FTS5 в приватном XDG cache по хешу
канонического пути vault. File key отделён от ULID; generation публикует
files/links/tags/aliases/diagnostics/FTS одной транзакцией. Обход выполняется
вне write transaction, при commit проверяются generation и revision.
WAL/busy timeout координируют процессы; rebuild не удаляет занятую БД.

Каталог содержит метаданные без тел. `resolver` строит карты ID/path/alias/stem;
полный файловый `App.scan` остаётся эталоном и путём проверок записи без SQLite.
После durable write ставится dirty marker; отказ индекса возвращается warning,
не отменяет подтверждённый файл. `tui::worker` выполняет чтение, индекс и
операции через очереди по 8 элементов; четыре read lanes имеют отмену и IDs.
Принятые записи не отменяются. Notify + debounce 150 ms и полный проход раз
в минуту страхуют внешние изменения; `r` явно выполняет full.

`tools/format-probe` — отдельный проверочный пакет этапа 0, не пакет приложения.
Он переиспользует production parser и проверяет YAML-корпус; публичного Rust SDK нет.
Корпуса и генератор находятся в `tests/corpus` и `tools/corpus.py`.
Нормативный [format-v1](docs/format-v1.md),
[CLI/JSON](docs/contracts/cli-v1.md), [макеты TUI](docs/contracts/tui-v1.md).
Исходная [L3-схема](docs/architecture/diagrams/c4-l3-zetto-1.0-proposal.md)
остаётся предложением полной 1.0; она не свидетельствует о реализации.

---

## 4. Key constraints

- **Team** — соло-разработчик (self-проект).
- **Stack** — Rust (binding constraint, продукт строится как лёгкая нативная альтернатива Electron-решениям).
- **Дистрибуция** — single binary; никаких runtime-зависимостей у пользователя за пределами `$EDITOR`/git.
- **Формат данных** — plain markdown на диске, files-as-graph; никакой проприетарной БД как первичного хранилища (см. STRATEGY → Approach).
- **Operational** — локальный инструмент, без сервера, без cloud-зависимостей.
- **Compliance** — N/A (личные данные пользователя живут в его собственной файловой системе/git).
- **Бюджет / Time** — _TBD_; OSS-проект, темп определяется автором.

---

## 5. Decision index

<!-- Принятые ADR; прежние решения уточняются новыми, исторический текст сохраняется. -->

| # | Date | Status | Decision |
|---|---|---|---|
| [0009](./docs/architecture/decisions/0009-publish-derived-index-generations.md) | 2026-10-30 | Accepted | Производный индекс согласованными поколениями (v0.2) |
| [0008](./docs/architecture/decisions/0008-capture-publish-and-root.md) | 2026-10-29 | Accepted | Capture без связи, строгая публикация, явный root; уточнение STRATEGY |
| [0007](./docs/architecture/decisions/0007-shared-cli-tui-core.md) | 2026-10-29 | Accepted | Общее ядро CLI/TUI, один Cargo package |
| [0006](./docs/architecture/decisions/0006-safe-writes-and-recovery.md) | 2026-10-29 | Accepted | Короткий lock, hash/backup, устойчивые рабочие копии и recovery-план |
| [0005](./docs/architecture/decisions/0005-format-layout-and-resolution.md) | 2026-10-29 | Accepted | Формат/размещение, read-only чтение, однозначный resolver и YAML preservation |
| [0004](./docs/architecture/decisions/0004-frontmatter-convention.md) | 2026-05-09 | Accepted | Frontmatter convention: required `id`/`title`; standard optional `tags`/`aliases`/`created`/`updated`; `x-*` for extensions; lenient schema with lint warns; hand-rolled write |
| [0003](./docs/architecture/decisions/0003-link-representation.md) | 2026-05-09 | Accepted | Link representation: wikilink-primary `[[ULID]]` / `[[ULID|display]]`; markdown read-compat; embeds/anchors/block-refs deferred to v2 with triggers |
| [0002](./docs/architecture/decisions/0002-note-id-scheme-and-filename-layout.md) | 2026-05-09 | Accepted | Note ID scheme and filename layout: ULID in YAML frontmatter, `<ULID>-<slug>.md` filename |
| [0001](./docs/architecture/decisions/0001-project-name-and-ecosystem-positioning.md) | 2026-05-09 | Accepted | Rename project from `zk` to `zetto` |

ADR файлы живут в [`docs/architecture/decisions/`](./docs/architecture/decisions/).

Пакет этапа 0 [принят владельцем](./docs/contracts/stage-0.md).
ADR-0005–0008 имеют приоритет над перечисленными в них разделами ADR-0002–0004;
остальные положения прежних решений сохраняются.

---

## 6. Open questions

<!-- 5 seed-вопросов, выявленных при init. Каждый — кандидат на отдельный /archforge:cycle. -->

- ~~**Q1. Схема ID заметок.**~~ — закрыт [ADR-0002](./docs/architecture/decisions/0002-note-id-scheme-and-filename-layout.md): ULID во frontmatter + `<ULID>-<slug>.md` filename.
- ~~**Q2. Синтаксис ссылок.**~~ — закрыт [ADR-0003](./docs/architecture/decisions/0003-link-representation.md): wikilink-primary `[[ULID]]` / `[[ULID|display]]` + canonical markdown read-compat; embeds/anchors/block-refs deferred to v2.
- ~~**Q3. Производный индекс v0.2.**~~ — реализован по [ADR-0009](docs/architecture/decisions/0009-publish-derived-index-generations.md); дальнейшие оптимизации определяются замерами, без смены источника истины.
- ~~**Q4. Граница TUI vs CLI-pipe.**~~ — закрыт [ADR-0007](docs/architecture/decisions/0007-shared-cli-tui-core.md): общее ядро, независимые адаптеры и машинный контракт с первой команды.
- ~~**Q5. Степень принуждения constraints.**~~ — закрыт [ADR-0008](docs/architecture/decisions/0008-capture-publish-and-root.md): capture без связи; строгость при publish, видимое root-исключение.

---

## 7. Anti-patterns to avoid

<!-- Спроектные анти-паттерны, прямо вытекающие из STRATEGY + здравого смысла для Rust CLI. -->

- **Никакой проприетарной БД как первичного хранилища.** Plain markdown на диске — это контракт с пользователем (notes-as-code). Index допустим только как derived state, который можно безопасно удалить и пересобрать.
- **Папки не являются первичной организацией.** Граф строится через ссылки, а не через каталоги (см. STRATEGY → Approach: «отказ от папок-как-таксономии»).
- **Теги не заменяют ссылки.** Теги — facet, ссылки — связи. Если фича позволяет tagging вместо linking — это анти-паттерн.
- **Никакого Electron / web-UI.** Уничтожает основной тезис продукта.
- **Никакого облачного sync как зависимости.** Sync = пользовательский git (или другой механизм поверх файлов). Любое cloud-coupling — анти-паттерн.
- **Не делать rg/fzf/git обязательной средой выполнения.** Внешние инструменты композируются с CLI; базовые сценарии работают на чистой установке по принятому контракту этапа 0.
- **Никаких mouse-only путей в TUI.** Каждое действие должно иметь keybinding; mouse — опционально.

<!-- Дополнено `/archforge:observe` 2026-05-09 (находка O-12 — формализация trех subject-replacement идей, отвергнутых ideation): -->

- **Только CommonMark + frontmatter как формат заметки.** Никакого org-mode, asciidoc, neorg, custom DSL. STRATEGY фиксирует format choice через ставку на toolchain ubiquity и vim/git-совместимость (не через теоретическую близость к Luhmann). Альтернативные форматы регулярно всплывают как «более research-grounded» и должны быть закрыты этой записью.
- **Single-author по дизайну в v1.** Multi-author / collaborative-vault — отложено в `decision-map.md`, не запрещено навсегда; но *архитектурное проектирование под multi-author с первого дня* — анти-паттерн, потому что переусложняет на ровном месте (см. ideation 2026-05-09 rejection F3.5).
- **CLI-first; редакторские плагины и LSP — обёртки, не первичная форма.** Идея «начать как nvim-плагин или Obsidian-плагин» (ideation rejection F3.1, F6.5) сворачивает идентичность terminal-native инструмента и завязывает архитектуру на конкретный редактор/GUI. Плагины и LSP — легитимные surface'ы (см. C5 в decision-map), но не вместо CLI.

<!--
Примеры дополнительных анти-паттернов добавлять по мере появления решений:
- _e.g., No async для disk I/O если sync хватает — лишняя сложность._
- _e.g., No global mutable state в core lib — TUI/CLI должны быть тонкими адаптерами._
-->
