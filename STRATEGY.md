---
name: zetto
last_updated: 2026-10-29
---

# zetto Strategy

## Target problem

Заметки — plain text, который должен жить и редактироваться в той же цепочке инструментов, что и код (vim, git, grep). Существующие PKM-инструменты (Obsidian) принудительно вытаскивают граф знаний в отдельный GUI-мир, делая его второсортным гражданином рабочего процесса инженера.

## Our approach

Поведение продукта диктуется доказанными паттернами Zettelkasten (Luhmann/Ahrens): atomic notes, link-before-publish, фиксированная ID-схема, отказ от папок-как-таксономии и тегов-как-замены-ссылок. Быстрый capture сохраняет мысль без связи в inbox; publish требует осмысленной связи на другую опубликованную заметку. Одно явное root-исключение создаётся при init. Принятый контракт — [ADR-0008](docs/architecture/decisions/0008-capture-publish-and-root.md). Размер заметки остаётся советом, а структурные защиты данных обязательны.

## Who it's for

**Primary:** Terminal-native инженеры/исследователи — они уже живут в vim+tmux+git и нанимают zetto, чтобы вести Zettelkasten не выходя из своей цепочки инструментов.

## Key metrics

- **Orphan-note ratio** — доля опубликованных заметок без исходящей связи на другую опубликованную заметку, исключая явно назначенный root; цель <5%. Inbox учитывается отдельно. *Leading.*
- **Avg outgoing links per note** — плотность связей опубликованной базы; диагностический показатель, без требования добавлять связи ради числа. *Leading.*
- **Capture latency** — машинное время от подтверждения готового текста до долговечного сохранения в inbox; отдельно время пользователя «готовая мысль → inbox», ориентир <5 сек. *Leading.*
- **Publish latency** — системное время проверки и переноса отдельно от времени выбора связи человеком. *Leading.*
- **Inbox size / age / time-to-publish** — число и возраст черновиков, время до публикации; рост inbox не должен скрываться улучшением доли сирот. *Leading.*
- **Cross-session usage frequency** — работа с заметками внутри привычной coding-сессии (vim/tmux/git). *Leading.*
- **Median time-to-first-link** — время до первой осмысленной связи у нового пользователя. *Lagging.*

## Tracks

### Note graph engine

Парсинг markdown, ID-схема, индекс ссылок, FS-watcher, инкрементальная пересборка графа. Чистая Rust-библиотека без UI.

_Why it serves the approach:_ без надёжного графа constraints (orphan-detection, link-validation) не на чем держать — это substrate-слой для всех остальных треков.

### TUI & capture flow

Single-keystroke capture, fuzzy-linking, навигация по графу, инкрементальный поиск, бюджет латентности по операциям.

_Why it serves the approach:_ constraints должны срабатывать в момент работы, иначе на них забьют. Воплощает capture → обработка → link-before-publish в реальном UX.

### Toolchain interop

Совместимость с git, $EDITOR, ripgrep, fzf; формат файлов как stable contract; CLI-команды, композирующиеся в pipe; hooks для скриптов.

_Why it serves the approach:_ прямо отвечает Target Problem — без этого трека заметки снова закрываются в собственном мирке вместо того, чтобы жить рядом с кодом.

### Methodology enforcement

Линтер заметок (atomic-check, orphan-check, ID-format), миграции, диагностики, опциональные strict-mode/guided пресеты.

_Why it serves the approach:_ прямо воплощает research-grounded constraints — продукт отказывается от анти-паттернов. Без этого трека zetto становится «ещё одним markdown-редактором с TUI».
