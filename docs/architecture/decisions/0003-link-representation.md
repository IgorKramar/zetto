# ADR-0003: Link representation

> Принятое уточнение от 2026-10-29:
> [ADR-0005](0005-format-layout-and-resolution.md) заменяет раздел
> «Resolver — ordered passes» и связанные edge cases: единый resolver,
> полная проверка ULID, явная неоднозначность, suffix/embeds без fallback
> на целую заметку. Нормативный контракт — [format-v1](../../format-v1.md).
> Синтаксис создаваемых ссылок сохраняется; исторический текст ниже оставлен.

- **Date**: 2026-05-09
- **Status**: Accepted
- **Authors**: Igor Kramar
- **Cycle**: A2 (decision-map.md), deep
- **Predecessor**: ADR-0002 (Note ID scheme + filename layout)
- **Decided**: A2 — индекс открытых архитектурных решений в [`decision-map.md`](../decision-map.md)
- **Affects (разблокирует или ограничивает)**: A3, B2, C2a, C4, C5, D1, D4

## Context

ADR-0002 зафиксировал, что **identifier внутри линка — ULID**, frontmatter `id:` canonical, filename `<ULID>-<slug>.md` ergonomic projection. ADR-0003 фиксирует **полный link-syntax-контракт zetto**: какой синтаксис zetto генерирует и читает, как резолвит target, как рендерит display-text, как обрабатывает ошибки и какие feature отложены в v2.

Idiom выбора задан экосистемой PKM 2025–2026: wikilinks `[[X]]` — de-facto стандарт через Obsidian-влияние; canonical markdown `[text](path)` — universal CommonMark. `pulldown-cmark` 0.13.3 (март 2026) поддерживает wikilinks нативно через `Options::ENABLE_WIKILINKS`; `LinkType::WikiLink { has_pothole: bool }` различает `[[X]]` и `[[X|display]]`. Anchor refs `[[X#H]]` и block refs `[[X#^id]]` парсер принимает (suffix попадает в `dest_url` как часть строки) — это даёт forward-compat без миграции для отложенных в v2 фич.

**Discovery** ([2026-05-09-A2-link-representation-discovery.md](../research/2026-05-09-A2-link-representation-discovery.md)) зафиксировал 7 leans: wikilink-primary при write; canonical markdown read-compat; ULID literal в брекетах; `[[ULID|display]]` опционально + render-time fallback на title; embeds/anchors/block-refs defer в v2; `zetto/no-broken-link` lint flag + literal preservation; external URLs canonical-markdown-only.

**Research digest** ([2026-05-09-A2-link-representation-research.md](../research/2026-05-09-A2-link-representation-research.md)) подтвердил state-of-the-art: pulldown-cmark стабилен, альтернативные парсеры ландшафт не сдвинули, render-fallback паттерны во всех зрелых PKM полагаются на persistent index, block-IDs — Obsidian convention `^[a-z0-9]{6}`.

**Roast** ([reviews/2026-05-09-roast-A2-link-representation/](../reviews/2026-05-09-roast-A2-link-representation/00-summary.md)) пятью ролями обнаружил 42 finding, 7 cross-cutting concerns. Решение по сути не оспорено. 12 текстовых правок применены ниже.

## Decision

**Internal links** — wikilink-primary. zetto **генерирует** `[[ULID]]` или `[[ULID|display]]`. zetto **читает**: оба wikilink-формата + canonical markdown `[text](filename.md)` (для back-compat с заметками, импортированными извне — Obsidian-vault, mdbook, произвольный markdown). **External URLs** — canonical markdown `[text](https://...)`; wikilinks reserved internal-only.

### Wikilink syntax v1

- **Write-form**: `[[<ULID>]]` или `[[<ULID>|<display>]]`. Display — UTF-8 строка, может содержать markdown-inline (`[[ULID|*emphasis*]]` валидно).
- **Что НЕ пишется в v1** (zetto не генерирует, но parser принимает; lint flag-ит): `![[ULID]]` (embed), `[[ULID#Heading]]` (anchor), `[[ULID#^block-id]]` (block ref).

### Parser

- `pulldown-cmark` 0.13.3 (см. ADR-0002 § Crate dependencies).
- Активация: `Parser::new_ext(input, Options::ENABLE_WIKILINKS)`.
- Detection: `Event::Start(Tag::Link { link_type: LinkType::WikiLink { has_pothole }, dest_url, .. })`. Поле `has_pothole` (терминология `pulldown-cmark`) — `true` если внутри wikilink есть pipe-разделитель `|`.
- `[[X]]` → `has_pothole=false`, `dest_url=="X"`.
- `[[X|display]]` → `has_pothole=true`, `dest_url=="X"`, display-text приходит как `Event::Text` между `Tag::Link` start/end.
- Embed detection: `Event::Start(Tag::Image { link_type: LinkType::WikiLink, .. })` → `![[X]]`.

### Resolver — ordered passes (применение F-2)

Resolver описан как **последовательность proof-passes**; добавление нового pass — локальное изменение, не переписывание. Текущие passes для wikilink-target-а (`dest_url`):

1. **Suffix split**: `(id_part, suffix_opt) = dest_url.split_once('#')`. В v1 `suffix_opt` игнорируется (см. § Deferred); в v2 — передаётся в anchor/block-ref pass.
2. **External-URL detection** (применение B-5): allow-list схем `[http, https, ftp, mailto, sftp, ssh]` через regex `^(https?|s?ftp|mailto|ssh):`. На match — lint flag `zetto/external-url-as-wikilink` (severity error в `recommended-luhmann`); render literal. Деni-list схем `file:`, `javascript:`, `data:` явно блокируется sanitization-rule перед glob — глоб не получает строку с двоеточием.
3. **ULID format + range validation** (применение B-3): regex `^[0-9A-HJKMNP-TV-Z]{26}$` (Crockford-алфавит) на `id_part`. На fail — lint flag `zetto/non-ulid-wikilink-target`; render literal. Дополнительная range-check: первые 6 chars декодируются в ms-timestamp; если значение `< 0` или `> 281474976710655` (макс 48-bit) — lint flag (out-of-range timestamp). **Case-sensitivity**: ULID-литерал чувствителен к регистру (Crockford каноничен в uppercase); zetto не нормализует case при чтении — нарушение даёт `non-ulid-wikilink-target` lint flag (alphabet `[0-9A-HJKMNP-TV-Z]` именно uppercase).
4. **File lookup**: glob `<id_part>-*.md` или `<id_part>.md` в notes-каталоге. Найдено — resolved. Не найдено — lint flag `zetto/no-broken-link`; render literal в broken-style.

Для markdown-link-target-а (`dest_url`):

1. **External detection**: то же allow-list — match → external link, render как стандартный CommonMark, не lint-flag.
2. **Basename extract** (применение B-4): `basename = Path::new(dest_url).file_stem().unwrap_or("")`. Это поднимает `./inbox/01J9X...md` → `01J9X...`, относительный путь не мешает ULID-prefix-extract.
3. **ULID-prefix extract**: regex `^[0-9A-HJKMNP-TV-Z]{26}` (без `$`-anchor) на `basename`. Match → резолв через ULID-glob (как pass 4 wikilink); нет — стандартный filename match для importable content.

### Render

Display-text приоритет (применение CC-5):

1. Inline display из `[[ULID|display]]` — если есть, используем literally.
2. Frontmatter `title` из target-заметки — synchronous scan в v1 (см. § B1 trigger ниже).
3. ULID literal — если target не резолвится; визуально — broken-style (CSS-class и TUI-colour — implementation-detail, не часть public ABI; конкретные имена — в C3).

### Lint rules — имена резервируются здесь, engine архитектура в C2a (применение CC-4)

В v1 zetto резервирует следующие правила; **семантика severity (`error`/`warn`/`off`), engine-архитектура и `recommended-luhmann` preset content** — определяются в **C2a** (Methodology rule engine architecture, см. `decision-map.md`, статус — open). До C2a этот ADR резервирует только names и предлагаемые severity defaults.

| Rule ID | Description | Default (предложение) |
|---|---|---|
| `zetto/no-broken-link` | wikilink/markdown-link с unresolvable target | warn |
| `zetto/external-url-as-wikilink` | URL-схема внутри wikilink | error |
| `zetto/non-ulid-wikilink-target` | wikilink-target не валидный ULID или out-of-range timestamp | warn |
| `zetto/embed-not-supported-in-v1` | `![[X]]` обнаружен — не render-ится в v1 | warn |
| `zetto/anchor-not-supported-in-v1` | `[[ULID#Heading]]` обнаружен | warn |
| `zetto/block-ref-not-supported-in-v1` | `[[ULID#^block-id]]` обнаружен | warn |

**Lint vs render decoupling** (применение B-8): эти rules — **observation-rules**, они не управляют render-behavior. Render всегда treats embed/anchor/block-ref в v1 как literal независимо от lint severity. Если пользователь поставил `zetto/embed-not-supported-in-v1: off`, embed `![[X]]` всё равно рендерится как literal text, не как HTML `<img>`.

### Deferred в v2 — с явными trigger conditions (применение CC-1 / F-1)

Не открытые «когда-то потом», а defer-with-trigger:

- **Embeds `![[ULID]]`** — trigger: первая заметка в проекте использует `![[X]]` ≥10 раз ИЛИ user explicitly запрашивает feature. Реализация: render-pass inline-ит target body. Migration: parser-уровень не меняется; v2-binary рендерит inline, v1-binary рендерит literal (см. § Forward-compat).
- **Anchor refs `[[ULID#Heading]]`** — trigger: B1 (graph index) закрыт И heading-stability подвопрос обсуждён в отдельном sub-decision (как генерировать stable anchor-id из heading text при retitle heading).
- **Block refs `[[ULID#^block-id]]`** — trigger: первая заметка в проекте использует block-refs ≥10 раз. Реализация: Obsidian-convention `^[a-z0-9]{6}` per-paragraph block-IDs, auto-generated при write; lint на коллизии within-file.

Без срабатывания триггера — feature остаётся в defer-state; lint-warn фиксирует факт использования. **Если три trigger никогда не срабатывают за 18 месяцев — переоткрыть Open question**: продолжать ли defer или признать «v2 не наступит» и переименовать lint-rules.

### Forward-compat — weak forward-compat (применение CC-6 / F-4)

Уточнение к ADR-0002 § Format versioning anchor:

- **Parser-level forward-compat — strong**: parser принимает синтаксис v1 + v2. Файлы валидны в обе стороны.
- **Renderer-level forward-compat — weak**: v1-binary рендерит embed/anchor/block-ref как literal с lint-warn; v2-binary рендерит как inline / jump / transclude.
- **User-visible behavior**: один и тот же файл, обработанный v1-binary и v2-binary, даёт разный output на embed/anchor/block-ref-формах. Файлы остаются валидными в обе стороны, но visible behavior расходится.
- **Это НЕ требует bump format-v1**: ADR-0002 § Format versioning anchor зафиксировал, что ID-rendering в линке — implementation detail, не public ABI. Anchor/block-ref/embed расширения — то же самое.

### B1 (graph index) — metric-trigger для внеплановой разблокировки (применение CC-2)

Render-fallback на frontmatter `title` (приоритет 2) делает synchronous scan target-файла. На N заметках при render-pass с M wikilinks без display — N×M FS-reads. ARCHITECTURE.md §2.1 фиксирует <100 ms на index lookup и <500 ms на fuzzy-link picker.

**Trigger**: если synchronous scan превышает **50 ms p50** при vault ≥ 500 заметок, B1 разблокируется внепланово (до закрытия A3/A4/A5).

**Interim mitigation** (опциональный): in-memory cache title-by-id, заполняется при первом scan, инвалидируется по mtime target-файла. ~50 строк кода, даёт 1–2 года breathing room до B1.

**Latency budget**: render-fallback укладывается в строку «Save» (<300 ms) или «TUI render» (<200 ms) capture-latency budget из `ARCHITECTURE.md` §2.1 — конкретное распределение зависит от реализации.

### Edge cases

- **Empty wikilink `[[]]`**: `dest_url == ""`. Lint flag `zetto/no-broken-link` (target empty); render literal.
- **Pipe-only `[[|display]]`**: `dest_url == ""`, `has_pothole = true`. Аналогично.
- **Multi-pipe `[[ID|a|b]]`** (применение B-7): parser обрабатывает по своим правилам — первый `|` разделитель, остаток (`a|b`) — display-text. zetto не делает additional checks; render использует `a|b` как display.
- **Empty display `[[ID|]]`**: `has_pothole = true`, display = empty Event::Text. Trigger fallback на frontmatter title (приоритет 2).
- **Wikilink внутри inline-code** `` `[[X]]` `` (применение B-9): parser НЕ парсит wikilinks внутри `Tag::CodeBlock`/`Code`; они приходят как plain text. zetto lint их не флагает — это контентное упоминание синтаксиса.
- **Wikilink внутри markdown-link-text** `[see [[X]] for details](other.md)`: parser принимает inline wikilink — `[[X]]` парсится отдельно от outer markdown-link, оба резолвятся независимо.

## Consequences

### Easier

- **Brevity**: `[[01J9X...]]` короче `[on fixed IDs](01J9X...-on-fixed-ids.md)`; muscle-memory быстрее.
- **Render fallback**: pipe-display (`[[X|t]]`) в исходнике + frontmatter title как automatic display — работает out-of-the-box без B1.
- **Forward-compat**: defer embeds/anchors/blocks — zero parser cost; v2 = добавить resolver-pass + render-pass без миграции.
- **Парсер уже выбран** в ADR-0002; `Options::ENABLE_WIKILINKS` — single-line изменение.
- **D4 совместимость** read-only и read-write (с aliases): `[[ULID]]` рендерится Obsidian как literal, но `aliases:` frontmatter (если включён в A3) даст резолв.

### Harder

- **Wikilinks нестандарт CommonMark**: pandoc/mdbook/Hugo без plugin не отрендерят. Митигация — опциональный `zetto export --to-markdown` режим, конвертирующий wikilinks в markdown-стиль (вне scope этого ADR; в C5/D1).
- **Synchronous scan** в v1 для render-fallback — реальная стоимость; trigger в § B1 trigger выше митигирует.
- **Lint-rules без C2a engine**: 6 имён зарезервировано; реализация ждёт C2a. До C2a — names живут как documentation, не как working code.

### Risks accepted

- **«Defer = forever» риск** (применение F-1): три trigger conditions выше дают надежду, что embed/anchor/block-ref доберутся до реализации; если не сработают за 18 месяцев — open question переоткроется.
- **`pulldown-cmark` 0.13 → 0.14 API break** на горизонте 1–2 года. Миграция — точечный рефакторинг 1–2 точек вызова.
- **Resolver complexity drift**: текущие 4+3 passes к 2028 могут вырасти до 8–12 (aliases, slug-rename detection, case-insensitive Obsidian import). Это нормальный жизненный путь — закладываем «ordered passes» как extension point вместо «одна функция».
- **D4 sub-narrowing**: A2 совместима только с D4 ∈ {own format, read-only, read-write через aliases}. Strict-checker variant D4 (zetto читает arbitrary Obsidian-vault, не диктуя filename) уже исключён в ADR-0002. Если D4 в итоге пойдёт в strict-checker — A2 пересматривается.

## Alternatives considered

Подробное сравнение и trade-off matrix — в [2026-05-09-A2-link-representation-design.md](../research/2026-05-09-A2-link-representation-design.md). Здесь — выжимка с reasoning отбраковки.

### 1. Wikilink-primary с canonical markdown read-compat (выбрана)

zetto генерирует `[[ULID]]` или `[[ULID|display]]` для internal links; читает оба wikilink-формата + canonical markdown `[text](filename.md)` для back-compat с импортированным извне контентом. External URLs — только canonical markdown. Embeds/anchors/block-refs deferred в v2 с trigger conditions; parser принимает их синтаксис уже сейчас, но render — literal.

*Сильная сторона*: brevity (`[[ID]]` короче `[text](path.md)`); максимальная D4-compat в variants {own format, read-only, read-write через aliases}; forward-compat для отложенных фич без миграции; парсер уже зафиксирован в ADR-0002 (`pulldown-cmark` + `Options::ENABLE_WIKILINKS`).

*Слабая сторона*: wikilinks нестандарт CommonMark — pandoc/mdbook/Hugo без plugin не отрендерят (митигация: future export-mode переводит в markdown-стиль); render-fallback на frontmatter title требует synchronous scan в v1 до B1 (митигация: metric-trigger разблокировки B1 + опциональный in-memory cache).

### 2. Markdown-only canonical

zetto генерирует и читает только `[text](<ULID>-<slug>.md)`. Pure CommonMark; никаких wikilinks. Внешние URL — стандартный markdown-link.

*Сильная сторона*: pure CommonMark — работает в любом mdbook/Hugo/pandoc-renderer без extension; парсер `Options::ENABLE_WIKILINKS` не нужен; D4 read-only поверх Obsidian-vault — Obsidian тоже поддерживает markdown-links.

*Слабая сторона*: длинные линки в исходнике (`[on fixed IDs](01J9XQK7ZBV5G2D8X3K7C4M9P0-on-fixed-ids.md)` против `[[01J9XQK7ZBV5G2D8X3K7C4M9P0|on fixed IDs]]`); rename slug формально ломает текстовый label (хотя ULID-prefix-extract его исцеляет на target side); D4 read-write слабее — Obsidian primary syntax wikilinks. *Lost because*: brevity-loss и слабый D4 read-write не компенсируются pure-CommonMark преимуществом для целевой аудитории terminal-native инженеров (которые редко рендерят свой vault как static site).

### 3. Full-Obsidian-superset

Альтернатива 1 + embeds (`![[ULID]]`), anchor refs (`[[ULID#Heading]]`), block refs (`[[ULID#^block-id]]`) в v1. Полная D4-compat read-write с Obsidian-vault.

*Сильная сторона*: max D4-compat сразу — Obsidian-пользователи могут импортировать vault с минимальными правками; богатый PKM-функционал из коробки.

*Слабая сторона*: ×3 parser-rules + render-pass complexity; block-IDs требуют отдельной auto-id-схемы (Obsidian convention `^[a-z0-9]{6}` per-paragraph); heading-stable contract (rename heading text ломает anchor-ref); migration tool для embeds (если v2 их меняет) — гарантированная боль; CI для всех трёх features в pre-alpha. **Существенно** утяжеляет первый release. *Lost because*: цена ≈3× v1-complexity не оправдана текущим состоянием D4 (вариант ещё не выбран; и read-write через aliases в Альтернативе 1 + parser-accepts-syntax forward-compat дают почти то же без upfront-cost).

### 4. Status quo — отложить A2 до B1 / реализации

Не определять link-syntax сейчас; ждать B1 (graph index) или появления первого реального кода, который потребует резолвить линки.

*Сильная сторона*: zero effort немедленно; решение откладывается в момент, когда есть больше empirical data.

*Слабая сторона*: A2 блокирует A3 (frontmatter convention — нужно знать, нужны ли `aliases:`), B2 (markdown parser — нужно знать, включать ли wikilinks-extension), C4 (capture flow — нужно знать, как генерировать линки), D1 (search — резолвить ли по ULID или по filename), D4 (Obsidian-compat — нужен ли read-write). *Lost because*: блокировка пяти других решений не оправдана ожидаемым ROI от отложения; empirical data не появится без любого hypothesis в production, а текущий выбор оставляет escape-hatches (forward-compat parser, weak forward-compat renderer).

### Explicitly not considered

| Variant | Reason rejected |
|---|---|
| **Org-mode `[[id:UUID][text]]`** | Иной экосистемный format; CommonMark-несовместим; STRATEGY-выбор markdown в ADR-0002. |
| **Pure tag-graph (no link syntax, ссылки через теги)** | STRATEGY anti-pattern «теги ≠ замены ссылок»; полностью противоречит подходу. |
| **`[text](#ULID)` URL-fragment style** | `#ULID` обычно — anchor внутри текущего документа, не cross-doc reference. Сломает pandoc/mdbook. |
| **Free-form text resolved-at-runtime** | Too magical; breaks WYSIWYG; STRATEGY-anti-pattern «продукт диктуется доказанными паттернами», free-form resolution — анти-паттерн в PKM-лите. |
| **Mediawiki-style anchors в v1** | Это вариант 3 (Full-Obsidian-superset); отвергнут по сложности и не-нужности в v1. |
| **`[[ULID:#ID]]` или `[[ULID:#^block]]` (двоеточие как разделитель)** | Org-mode-style; не совместим с pulldown-cmark wikilinks-extension; нестандартен в PKM-экосистеме. |

## Privacy and security considerations (применение CC-5)

Этот ADR расширяет приватность-периметр ADR-0002 § Privacy на link-syntax:

- **`[[ULID]]` декодируется в creation-time** (первые 48 бит ULID — ms Unix epoch). При публикации одной заметки наружу её raw markdown содержит wikilinks-таргеты, **раскрывающие existence + creation-time других заметок** vault-а. Принятый trade-off — ULID sortability ценнее этой metadata leakage; при необходимости — future export-mode (отдельный ADR) рендерит ULID literal как opaque hash или вшивает display-text inline.
- **Render-fallback на frontmatter title** при export может **leak-ить title чужих заметок**. Если пользователь публикует заметку A с `[[ULID-of-B]]` и не публикует B, но render автоматически подставляет B's title — title B утечёт. Future export-mode должен либо требовать explicit display, либо рендерить ULID literal.
- **Lint-сообщения цитируют ULID-литерал** (`error: link [[01J9X...]] is broken`). Pasted в issue/PR/Slack/gist — leak ULID broken-target. Implementation note: lint-output умеет режим `--quiet-targets`, ограничивающий diagnostic до позиции (file:line:col).
- **Encryption-at-rest** — наследуется из ADR-0002 § Privacy (responsibility of the user, FS-level).
- **Note deletion** — наследуется из ADR-0002 § Privacy. Wikilinks в коммитах git history не удаляются вместе с target-заметкой; для полного стирания требуется `git filter-repo` / BFG.

## Implementation

### Synchronous changes (этот коммит)

- **ADR-0003** — этот файл.
- **`ARCHITECTURE.md` §5 Decision Index** — запись для ADR-0003.
- **`ARCHITECTURE.md` §6 Open Questions** — Q2 (Link representation) переходит в strikethrough + reference.
- **`docs/architecture/README.md` ADR Index** — та же запись.
- **`docs/architecture/decision-map.md`** — A2 переходит open → decided со ссылкой на ADR-0003. Suggested order пересобран: A3/A4/B1/B2/C1 разблокированы.

### Crate dependencies (delta к ADR-0002)

`pulldown-cmark` 0.13.3 уже зафиксирован в ADR-0002. Этот ADR требует только включения `Options::ENABLE_WIKILINKS` — нет новых крейтов.

## Open questions, отложенные в смежные ADR / v2

- **A3** (frontmatter convention): обязательное поле `title:` подразумевается этим решением (render-fallback). A3 формализует mandatory vs optional.
- **B1** (graph index): synchronous scan для render-fallback — temporary v1; B1 заменит на index lookup. Внеплановый trigger описан выше.
- **C2a** (rule engine): 6 lint-rule names резервированы здесь; semantic, severity-семантика, `--fix` mode и `recommended-luhmann` preset content — в C2a.
- **C3** (TUI library): broken-link рендер-стиль (CSS class в HTML, colour в TUI) — implementation-detail, имена фиксируются здесь.
- **C5** (editor integration): wikilinks резолвинг через LSP-сервер — отдельное решение.
- **D1** (search backend): rg/fzf integration с wikilink-syntax — отдельное решение.
- **D4** (Obsidian compat): A2 совместима только с D4 ∈ {own format, read-only, read-write через aliases}. Strict-checker исключён. **Если D4 пойдёт в read-write — A3 должен включить frontmatter `aliases:` поле для bridge between zetto-ID-resolve и Obsidian-title-resolve.**
- **v2-cycles для embeds/anchors/block-refs** — отдельные ADR при срабатывании trigger conditions (см. § Deferred в v2).
- **Defer-conditions sunset** — если три trigger condition не срабатывают за 18 месяцев, открыть question «переименовать `*-not-supported-in-v1` lint-rules в `*-not-supported`». Документирует отказ от фичи, не отложение.

## Reviews trail

- 2026-05-09 — Discovery ([2026-05-09-A2-link-representation-discovery.md](../research/2026-05-09-A2-link-representation-discovery.md))
- 2026-05-09 — Research digest ([2026-05-09-A2-link-representation-research.md](../research/2026-05-09-A2-link-representation-research.md))
- 2026-05-09 — Design ([2026-05-09-A2-link-representation-design.md](../research/2026-05-09-A2-link-representation-design.md))
- 2026-05-09 — Decision summary ([2026-05-09-A2-decision-summary.md](../research/2026-05-09-A2-decision-summary.md))
- 2026-05-09 — Roast (5 roles, severity: 3H/13M/8L) — [00-summary.md](../reviews/2026-05-09-roast-A2-link-representation/00-summary.md)
- 2026-05-09 — Meta-review on roast — [99-meta-review.md](../reviews/2026-05-09-roast-A2-link-representation/99-meta-review.md)
