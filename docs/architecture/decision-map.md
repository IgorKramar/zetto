# Decision Map

> 2026-10-29: владелец принял ADR-0005–0008 и
> [пакет этапа 0](../contracts/stage-0.md). Закрыты A4/A5/C1/C2b/C4;
> A1–A3 уточнены новыми ADR. Реализация начинается с v0.1.

> Living document. Updated whenever a cycle completes or a new architectural question is identified.
> Last updated: 2026-10-29 (принятие контрактов этапа 0)

Источники: 5 open questions из [`ARCHITECTURE.md`](../../ARCHITECTURE.md) + неявные архитектурные решения, подразумеваемые [`STRATEGY.md`](../../STRATEGY.md) и треками работы (Note graph engine / TUI & capture flow / Toolchain interop / Methodology enforcement) + находки `/ce-ideate` от 2026-05-09 (см. [`docs/ideation/2026-05-09-zk-surprise-me-ideation.md`](../ideation/2026-05-09-zk-surprise-me-ideation.md)). Приняты ADR-0001–0008; исторические исследования сохраняются.

---

## Group A — On-disk contract (principal stakes)

Это контракт между `zetto` и пользователем («notes-as-code»). Всё, что внутри движка, должно ему подчиняться. Эти решения почти не обратимы — они мигрируют через переписывание всех существующих заметок.

**A1. Note ID scheme + filename convention** (= Q1 в ARCHITECTURE.md)
- *Forces*: human-readable vs collision-safe; sortable в `ls/git diff`; стабильность при переименовании; совместимость с file-system-as-graph.
- *Status*: **decided** — см. [ADR-0002](decisions/0002-note-id-scheme-and-filename-layout.md). ULID во frontmatter (canonical) + `<ULID>-<slug>.md` filename.
- *Blocks*: A2, A3, A4, A5, B1, B2, C4, D1, D4 (все разблокированы)
- *Blocked by*: —

**A2. Link representation** (= Q2)
- *Forces*: parser complexity; interop с vim-плагинами/pandoc/mdbook; UX fuzzy-linking; поведение при rename.
- *Status*: **decided** — см. [ADR-0003](decisions/0003-link-representation.md). Wikilink-primary `[[ULID]]` / `[[ULID|display]]` + canonical markdown read-compat; embeds/anchors/block-refs deferred to v2 with triggers.
- *Blocks*: B1, B2, C4, D4 (все разблокированы)
- *Blocked by*: A1 (закрыт ADR-0002)
- *Mutual constraint with A1*: разрешён в виде двух последовательных ADR (ADR-0002 + ADR-0003).

**A3. Frontmatter convention**
- *Forces*: YAML (de-facto стандарт markdown PKM, совместим с Obsidian/Hugo) vs TOML (проще парсится, ближе к Rust-экосистеме) vs no frontmatter (всё в теле). Влияет на хранение метаданных: ID, title, теги, created, links.
- *Status*: **decided** — см. [ADR-0004](decisions/0004-frontmatter-convention.md). YAML с required `id`/`title`, standard optional `tags`/`aliases`/`created`/`updated`, custom `x-*`, lenient + lint-warn, hand-rolled write.
- *Blocks*: A5, B2 (parser), C4 — все разблокированы.
- *Blocked by*: A1 (закрыт ADR-0002), implicitly A2 (закрыт ADR-0003 — `title:` обязателен per render-fallback).

**A4. Notes directory layout**
- *Forces*: single flat root vs id-prefix-buckets (`a/`, `b/`...) vs freeform (пользователь сам решает). STRATEGY запрещает «папки-как-таксономия» — но папки как операционный shard остаются на столе. Влияет на скорость FS-обхода и читаемость в `ls`.
- *Status*: **decided** — [ADR-0005](decisions/0005-format-layout-and-resolution.md): плоский корень, inbox/assets/recovery с разным жизненным циклом.
- *Blocks*: A5, B1 (стратегия индексации), B3 (FS-watcher scope), D4
- *Blocked by*: A1 (если ID hierarchical — раскладка следует из ID)

**A5. Format versioning policy & public spec contract** *(добавлено `/archforge:observe` 2026-05-09; находка O-2)*
- *Forces*: что входит в публичную спецификацию формата (frontmatter-схема, ID-формат, link-syntax, layout-правила); как обозначаются мажорные/минорные изменения; кто и когда принимает PR, ломающий формат; какие миграционные инструменты обязательны при bump-е версии. Без этой политики «заморозка A1» — пустое обещание.
- *Status*: **decided** — [ADR-0005](decisions/0005-format-layout-and-resolution.md), [format-v1](../format-v1.md) и [контракт этапа 0](../contracts/stage-0.md): независимые версии формата/config/JSON/index, явная миграция несовместимых изменений; лицензия отдельной публикации спецификации фиксируется до beta.
- *Blocks*: D4 (Obsidian-compat зависит от того, чем именно совместимы); миграционная политика всех будущих изменений
- *Blocked by*: A1, A2, A3, A4 (нечего фиксировать в спецификации, пока эти не разрешены)

---

## Group B — Engine internals

Структурные решения внутри движка. Обратимы дороже, чем UX, но дешевле, чем on-disk contract — можно мигрировать без переписывания заметок пользователя.

**B1. Graph index strategy** *(reframed `/archforge:observe` 2026-05-09; находка O-4 — добавлена третья ось)*
- *Forces*: три варианта вместо двух — (a) **in-memory rebuild** при каждом запуске (zero state, dependency-free, но cold-start ∝ N заметок); (b) **persistent index** (sqlite/sled/JSON sidecar — быстро, но invalidation, sync, миграции схемы); (c) **event-sourced**: append-only журнал FS-событий (`note_created`, `link_added`, `note_renamed`, ...) + материализованная SQLite-проекция; cold start = replay snapshot, никогда не full rebuild. Прецеденты: postmortem Org-roam (SQLite быстра, узким местом было преобразование данных, не сам индекс), Foam #347 (full-rebuild + Electron = 65–75% CPU на 330 заметках), системы интервального повторения.
- *Status*: **decided** — SQLite/FTS5, generation/revision и сверка по [ADR-0009](decisions/0009-publish-derived-index-generations.md), реализованы в v0.2.
- *Blocks*: B3, D1, частично C4 (бюджет capture latency)
- *Blocked by*: A1, A2, A4 (что индексируется и как из этого собирается граф)

**B2. Markdown parser/AST**
- *Forces*: pulldown-cmark (de-facto стандарт в Rust, fast, CommonMark) vs markdown-rs (более фичевый) vs собственный минимальный парсер (только то, что нужно для extracting links + frontmatter).
- *Status*: **decided** — production parser на pulldown-cmark, [ADR-0005](decisions/0005-format-layout-and-resolution.md) и format-v1; работает с v0.1.
- *Blocks*: B1
- *Blocked by*: A2 (link syntax определяет, что вообще надо парсить)

**B3. FS-watcher strategy**
- *Forces*: notify-rs (cross-platform inotify/FSEvents) для live updates vs lazy on-demand check (стат файлов при запуске) vs hybrid. Имеет смысл только если есть persistent index B1.
- *Status*: **decided** — notify как сигнал, полный проход после потери и периодическая сверка; [ADR-0009](decisions/0009-publish-derived-index-generations.md), v0.2.
- *Blocks*: TUI live-update в C-группе
- *Blocked by*: B1

---

## Group C — Surface / UX

Самая обратимая категория — можно перекрутить TUI/CLI без миграции данных. Зависят от A слабо (через формат отображения) и от B минимально.

**C1. TUI ↔ CLI-pipe boundary** (= Q4)
- *Forces*: какие операции pipe-friendly (`zetto new`, `zetto list`, `zetto lint`) vs только TUI (живая навигация графа, fuzzy-link picker). Контракт Toolchain interop track. Решение «всё через TUI» убивает scriptability; «всё через CLI» убивает UX.
- *Status*: **decided** — [ADR-0007](decisions/0007-shared-cli-tui-core.md): общее ядро CLI/TUI и JSON с первой команды.
- *Blocks*: C2a, C2b, C3, C4, C5, D1
- *Blocked by*: — (можно решать почти параллельно с A)

**C2a. Methodology rule engine architecture** *(расщеплено из C2 `/archforge:observe` 2026-05-09; находка O-5)*
- *Forces*: API правил (имена `zetto/no-orphan`, `zetto/atomic-size`, etc.; категории; metadata); модель уровней (off / warn / error per-rule); inline-disable-механизм (frontmatter-флаг или HTML-комментарий); поддержка `--fix`; модель пресетов (`recommended-luhmann`, ship-time комплекты); точки запуска (pre-commit, runtime check before save, `zetto lint` batch). Это рамка для всей Methodology enforcement track.
- *Status*: open
- *Blocks*: C2b, C4
- *Blocked by*: C1

**C2b. Default strictness for `link-before-publish` rule** *(уточнено ADR-0008)*
- *Forces*: сохранить мысль до методологической проверки, сохранив требование связности опубликованной базы. Capture не блокируется отсутствием связи; публикация требует связь на другую опубликованную заметку.
- *Status*: **decided** — [ADR-0008](decisions/0008-capture-publish-and-root.md): `link-before-publish` блокирует публикацию, capture сохраняет без связи; root — явное исключение.
- *Blocks*: C4
- *Blocked by*: C1, C2a (уровень имеет смысл только в рамках общей модели уровней)

**C3. TUI library choice**
- *Forces*: ratatui (most active in Rust 2024–2026 ecosystem) vs cursive vs raw crossterm. Влияет на сложность layout, perf, биндинги.
- *Status*: **decided** — ratatui/crossterm, используются с v0.1 в общем package по [ADR-0007](decisions/0007-shared-cli-tui-core.md).
- *Blocks*: C4 implementation
- *Blocked by*: C1 (scope TUI определяет, какие фичи нужны от библиотеки)
- *Reversibility*: средняя — миграция между TUI-библиотеками ≈ переписывание UI-слоя.

**C4. Capture flow architecture** *(reframed `/archforge:observe` 2026-05-09; находка O-7 — расширено с UX-дизайна до архитектуры потоков)*
- *Forces*: верхнеуровневый выбор — (a) **single mode**: один поток, в нём же применяются ограничения; (b) **service / prep split** (mise-en-place): `zetto capture` — service-mode <5 с, без валидации, append в inbox-buffer; `zetto prep` — отдельный методичный ритуал, где применяются `zetto/link-before-save` и пр. Трактует напряжение метрик capture-latency ↔ orphan-ratio как архитектурное, а не правило-уровневое. Подвопросы (single-keystroke shape, fuzzy-linking UX, бюджет latency) — внутри выбранного верхнеуровневого варианта.
- *Status*: **decided** — [ADR-0008](decisions/0008-capture-publish-and-root.md): capture → обработка → publish; new объединяет шаги с сохранением draft при неудаче.
- *Blocks*: —
- *Blocked by*: A1, A2, B1, C1, C2a, C2b, C3 (это конечная UX-сборка верхнего уровня)

**C5. Editor-integration strategy** *(добавлено `/archforge:observe` 2026-05-09; находка O-3)*
- *Forces*: STRATEGY-первичный пользователь — vim+tmux+git, но *как именно* zetto встречает их в редакторе — открытый вопрос. Варианты: (a) **CLI-only** + shell-completion, пользователь сам пишет vim-команды/маппинги; (b) **vim-плагин-обёртка** поверх CLI; (c) **LSP-сервер** (универсально для vim/Helix/Zed/VS Code, link-validation/orphan-warnings/hover-backlinks как diagnostics); (d) **vim-плагин с встроенным движком** (Rust-core через nvim-oxi/mlua). Прецеденты: rust-analyzer / pyright / org-roam / telekasten.nvim / zk.nvim.
- *Status*: open
- *Blocks*: —
- *Blocked by*: C1 (граница TUI/CLI определяет surface, на которую плагины опираются); рекомендуется `/archforge:research` перед циклом
- *Note*: STRATEGY-метрика `cross-session usage frequency` напрямую зависит от выбора; чем интрузивнее интеграция в редактор, тем выше метрика, но тем выше и стоимость (LSP-сервер ≠ выходные на скрипт)

---

## Group D — Interop & ecosystem

**D1. Search backend**
- *Forces*: делегировать ripgrep (быстро, zero implementation, отвечает Track «Toolchain interop») vs использовать собственный B1-индекс (быстрее на типовых запросах, но дублирует ripgrep) vs hybrid (полнотекст → rg, метаданные → index).
- *Status*: **decided** — встроенный SQLite/FTS5, отдельные literal/regex и fuzzy title/alias; [ADR-0009](decisions/0009-publish-derived-index-generations.md), v0.2.
- *Blocks*: —
- *Blocked by*: B1, A1

**D2. Git coupling model**
- *Forces*: `zetto` производит коммиты сам vs только пишет файлы и оставляет git пользователю vs опциональные хуки (`zetto commit`). Старая история проекта (см. удалённые `src/`) к git напрямую не привязывалась. STRATEGY говорит «files в git-репозитории как обычные файлы» — это аргумент за hands-off.
- *Status*: open (имплицитно)
- *Blocks*: —
- *Blocked by*: A1, A4

**D3. Project name & ecosystem positioning** *(добавлено и закрыто `/archforge:adr` 2026-05-09)*
- *Forces*: коллизия имени `zk` с действующим Go-проектом [zk-org/zk](https://github.com/zk-org/zk) (2,6k★, активный май 2026); SEO/install-collision; стратегия резерва имени по реестрам распространения (crates.io, Homebrew, AUR).
- *Status*: **decided** — см. [ADR-0001](decisions/0001-project-name-and-ecosystem-positioning.md). Проект переименовывается в `zetto`.
- *Blocks*: D5 (теперь разблокирован)
- *Blocked by*: —

**D4. Obsidian-vault compatibility posture** *(добавлено `/archforge:observe` 2026-05-09; находка O-6)*
- *Forces*: четыре варианта по возрастанию совместимости — (a) **полностью свой формат**, никакого диалога с Obsidian; (b) **read-only совместимость** (zetto умеет читать существующий Obsidian-vault, но пишет в свой формат — путь миграции в одну сторону); (c) **read-write совместимость** (zetto и Obsidian могут параллельно работать с одним vault, без конфликтов); (d) **strict checker поверх vault** (zetto не редактирует, только проверяет/линтит существующий Obsidian-vault — позиционирование как методологический add-on). Прецеденты: Obsidian имеет ~1M пользователей, его формат de-facto стандарт plain-markdown PKM; native wikilinks-extension у pulldown-cmark облегчает совместимость. Влияет на выбор A2 (link syntax), на распространение (плагин-store как канал) и на анти-паттерн «no Electron» (он не запрещает совместимости *по формату* с Electron-инструментом).
- *Status*: open
- *Blocks*: —
- *Blocked by*: A1, A2, A3, A4, A5

**D5. Distribution & packaging strategy** *(добавлено `/archforge:observe` 2026-05-09; находка O-10; soft-deferred до v0.1)*
- *Forces*: каналы (cargo install / pre-built GitHub releases / Homebrew tap / scoop / пакеты дистрибутивов); release cadence; signing (особенно macOS); cross-compilation matrix; стабилизация CLI-surface как публичного контракта.
- *Status*: deferred (см. ниже) — wait for: подход к v0.1 release plan
- *Blocks*: —
- *Blocked by*: ADR-0001 разблокирован (имя `zetto` зафиксировано); требуется резерв `zetto` на crates.io как squat-defense до публикации

---

## Suggested order

1. **v0.1:** реализовать принятый формат и безопасный файловый слой,
   общее ядро, CLI/JSON и минимальный TUI. B2/C3 — конкретные зависимости
   и их совместимость фиксируются с потребляющим кодом; полный обход в памяти.
2. **v0.2:** B1/B3/D1 — производный SQLite-индекс, схема/сверка/свежесть,
   watcher и поиск. Обязательное сравнение full/incremental графа.
3. **v0.3–0.4:** ежедневный TUI и C2a — общий движок диагностик/пресетов.
   Базовые защиты и условие publish действуют уже с v0.1.
4. **v0.5:** C5/D4 — инструменты редактора/импорта/экспорта и recovery;
   произвольная двусторонняя совместимость Obsidian вне 1.0.
5. **До beta/RC:** D2/D5 — документация git-workflow и поставка проверенных
   пакетов. Публикация релиза требует отдельного явного действия владельца.

A1–A5, C1, C2b и C4 больше не являются очередью открытого проектирования.
Принятое поведение и критерии версий — в [пакете этапа 0](../contracts/stage-0.md).

---

## Cross-cutting (через несколько групп)

- **Latency budget allocation** — машинное время capture/publish измеряется отдельно от времени пользователя по ADR-0008. Системные бюджеты — в [`ARCHITECTURE.md`](../../ARCHITECTURE.md) §2; tools/benchmark_cli.py отдельно записывает обход/stat и end-to-end время на зафиксированном корпусе.

---

## Deferred (do not run yet)

- **Built-in third-party plugin host / pluggable rule loader** *(переформулировано `/archforge:observe` 2026-05-09; находка O-11)* — отдельно от C2a (built-in rule engine + ship-time presets, который НЕ deferred). Wait for: ≥2 third-party preset реально предложены сообществом, с явным запросом на загрузку без пересборки бинарника. До этого «один правильный пресет + ship-time альтернативы» дешевле и честнее.
- **Multi-vault / multi-root support** — wait for: реальный пользовательский use case за пределами single-user-single-vault. До этого single-root — не упрощение, а единственное оправданное решение.
- **Multi-author / collaboration** — wait for: явное желание пользователя поделиться vault (см. ideation 2026-05-09 rejection F3.5 как subject-replacement). Даже тогда — открытый вопрос, где заканчивается «multi-author» и начинается обычный git-workflow.
- **Cloud sync layer** — wait for: NEVER (см. [`ARCHITECTURE.md`](../../ARCHITECTURE.md) anti-patterns). Это не отложенное решение, а закрытое; sync = git пользователя.
- **Proprietary database as primary store** — закрыто (anti-pattern в `ARCHITECTURE.md`).
- **D5. Distribution & packaging** — отложено до подхода к v0.1 release plan; см. соответствующую запись выше.

---

## Notes

- A1 и A2 имеют **mutual constraint**: их трудно решать порознь. Опции: (а) единый bundled ADR на «on-disk note format», (б) два очень близких последовательных ADR с явной взаимной ссылкой. Выбор формы — на уровне `/archforge:design` для A1.
- Существует git-история до текущего пустого state (см. `git log` — `74543aa add tags search`, `e416aa4 add tags support`, `899bafd feat: add support for opening notes in various editors`). Эти коммиты содержат имплицитные решения по A1–A4 и D2, которые были сделаны *до* появления STRATEGY.md. **Перед запуском A1 имеет смысл прочитать удалённые `src/notes/store.rs`, `src/notes/metadata.rs` из последнего реального коммита** — там лежит бесплатный prior-art из собственного проекта.
- Старые ветки кода (TUI, search, tags) — это спайки, а не контракты. Не стоит таскать их решения в новые ADR без re-evaluation на фоне нового STRATEGY (research-grounded constraints).
- **Кросс-рамочные сходимости из ideation 2026-05-09** (X1–X6 в ideation-документе) полезны как сигналы того, какие решения стоит рассматривать bundled: X4 (event-sourced индекс) объединяет B1+B3+D1; X1 (engine kernel + thin UI clients) объединяет C1+C5; X3 (имя проекта) разрешено в [ADR-0001](decisions/0001-project-name-and-ecosystem-positioning.md).
