# ADR-0002: Note ID scheme and filename layout

- **Date**: 2026-05-09
- **Status**: Accepted
- **Authors**: Igor Kramar
- **Cycle**: A1 (decision-map.md), deep
- **Predecessor**: ADR-0001 (project name)
- **Decided**: A1 — индекс открытых архитектурных решений в [`decision-map.md`](../decision-map.md)
- **Affects (разблокирует или ограничивает)**: A2, A3, A4, A5, B1, B2, C4, C5, D1, D4, D5

> Принятые уточнения от 2026-10-29:
> [ADR-0005](0005-format-layout-and-resolution.md) заменяет положения разделов
> «Линкование», «Empty-slug lifecycle» и «Invariant validation» об источнике
> идентичности resolver и записи при чтении; YAML-зависимость/writer — также
> по ADR-0005. [ADR-0006](0006-safe-writes-and-recovery.md) заменяет протокол
> retitle/recovery. Остальная ID/filename-схема остаётся в силе;
> исторический текст ниже сохранён для прослеживаемости.

## Context

После того как 2026-05-09 написаны `STRATEGY.md`, `ARCHITECTURE.md` и `decision-map.md`, A1 (Note ID scheme + filename convention) выделен в decision-map как решение с наибольшим радиусом влияния — блокирует A2 (link representation), A3 (frontmatter convention), A4 (notes directory layout), A5 (format versioning), B1 (graph index strategy), B2 (markdown parser), C4 (capture flow architecture), D1 (search backend) и D4 (Obsidian-vault compatibility posture). Каждая заметка в zetto должна иметь стабильный идентификатор, переживающий retitle, не сталкивающийся с другими, читаемый в `ls`/`fzf`/`grep`, и совместимый с фиксированной публичной спецификацией формата.

Решение требует одновременно: (1) человеко-удобное имя файла для terminal-native пользователя; (2) стабильный идентификатор для backlink-резолва; (3) глобальную уникальность для multi-device git-sync; (4) хронологическую сортируемость для `ls -1`. Ни один однокомпонентный ID не закрывает все требования.

**Прецеденты, на которые опирается решение** (полные источники в [research digest](../research/2026-05-09-A1-ulid-slug-rust-research.md)):

- ULID-spec ([ulid/spec](https://github.com/ulid/spec)) — 26 chars Crockford base32, первые 48 бит = ms Unix epoch, остальные 80 бит = entropy. Lexicographic sort = chronological sort. RFC 9562 UUIDv7 (май 2024) даёт похожие свойства, но 36 chars hex; проигрывает читаемости.
- **Crockford base32** ([Crockford spec](https://www.crockford.com/base32.html)) — алфавит 32 символа, исключающий визуально схожие `I`/`L`/`O`/`U`. ULID-spec предписывает именно его, не RFC 4648 base32; generic base32-encoder выдаёт несовместимые строки (см. [`ulid/spec` issue #81](https://github.com/ulid/spec/issues/81)). Crockford-кодирование встроено в крейт `ulid`; внешние `crockford`/`fast32` не использовать.
- Сообщество Zettelkasten ([forum.zettelkasten.de](https://forum.zettelkasten.de/)) сошлось на UUID/ULID во frontmatter + slug в имени файла; timestamp-prefix отвергнут как «visually noisy»; иерархические Luhmann ID — paper-era artifact.
- Прежняя реализация zetto (commit `293a1e4`, ноябрь 2024) использовала UUID v4 в frontmatter — спайк, ему предшествовала текущая STRATEGY; не контракт.

**Discovery** ([2026-05-09-A1-note-id-scheme-discovery.md](../research/2026-05-09-A1-note-id-scheme-discovery.md)) зафиксировал 7 leans: ULID format; frontmatter source of truth; auto-generation на `zetto new`; slug — pure ergonomic; глобальная уникальность; retitle меняет slug, ID константен; A1 и A2 — два соседних ADR.

**Design** ([2026-05-09-A1-note-id-scheme-design.md](../research/2026-05-09-A1-note-id-scheme-design.md)) из 7 leans вывел три альтернативы filename layout. **Roast** ([2026-05-09-roast-A1-note-id-scheme/](../reviews/2026-05-09-roast-A1-note-id-scheme/00-summary.md)) пятью ролями обнаружил ~42 finding, 6 cross-cutting concerns; решение по сути не оспорено, но потребовало 12 текстовых уточнений в этом ADR.

## Decision

**ID заметки — ULID** (Crockford base32, 26 chars, time-prefixed), хранится канонически в YAML frontmatter поле `id`. Имя файла — эргономическая проекция вида `<ULID>-<slug>.md`; при отсутствии slug — `<ULID>.md`.

### Канонический ID

- **Формат**: ULID. Реализация — крейт [`ulid`](https://crates.io/crates/ulid) 1.2.1 (dylanhart, MIT/Apache-2.0).
- **Хранение**: YAML frontmatter поле `id`. Это canonical permalink заметки — устойчивая опорная точка через любые retitle/move/sync операции.
- **Генерация**: автоматическая на `zetto new`. Используется `Ulid::new()` (single-call API). **Monotonicity между отдельными вызовами `Ulid::new()` не гарантируется** и не нужна для personal-CLI с single-digit creates per second; collision-вероятность при двух вызовах в одной мс ≈ 2⁻⁸⁰ — пренебрежимо. Если в будущем потребуется (например, batch-import), переключиться на `ulid::Generator::generate_from_datetime` со статичным state-ом.
- **Жизненный цикл**: ID присваивается при создании заметки и **никогда не меняется**. Ручное изменение `id:` пользователем — нарушение контракта (см. § Invariant validation ниже).
- **Cross-machine guarantee**: ULID глобально уникален бесплатно (80 бит entropy + ms timestamp); sync через git между машинами не требует никакой координации **на уровне создания**. Координация retitle между машинами — отдельный case (см. § Retitle lifecycle and recovery).
- **Sortability**: первые 48 бит — ms Unix epoch UTC. Crockford lexicographic order = chronological order. Cross-machine ordering — approximately chronological modulo wall-clock skew; задокументировано как известный caveat.

### Filename layout

- **Pattern**: `<ULID>-<slug>.md` или `<ULID>.md` при пустом slug.
- **Пример**: `01J9XQK7ZBV5G2D8X3K7C4M9P0-on-fixed-ids.md`.
- **Slug — pure эргономика, не часть identifier-семантики.** Линки используют ULID. Slug помогает только в `ls`/`fzf`/`grep` workflow.
- **Charset filename**: `[a-z0-9-]` для slug, `[0-9A-HJKMNP-TV-Z]` для ULID (Crockford-алфавит без `I`/`L`/`O`/`U`). Никаких пробелов, спецсимволов, мульти-байт.
- **Max length**: slug-часть капится на **60 chars** (труним по word boundary). Полный filename ≤ ~92 ASCII chars + `.md`. Кросс-платформенно безопасно.
- **Slug normalization**: крейт [`slug`](https://crates.io/crates/slug) (Stebalien, MIT/Apache-2.0; deunicode-backed). Поведение по языкам — в [research digest](../research/2026-05-09-A1-ulid-slug-rust-research.md) §3. Кратко: кириллица и латиница — readable, японские кандзи мапятся на путунхуа-pronunciation (документированное ограничение `deunicode`), эмодзи — `🔥` → `fire`. Empty-slug fallback — когда `slug::slugify(title)` возвращает пустую строку либо строку из одних разделителей.

### Линкование (зона ответственности A1)

ADR-0003 (link representation, A2) определит полную семантику wikilink: display-text, embeds, anchor refs. **A1 фиксирует только что identifier внутри линка — ULID** (т.е. внутри любого wikilink, который A2 в итоге изберёт, в позиции «target identifier» стоит canonical 26-char ULID, не slug, не filename).

- **Резолюция `[[ULID]]`**: ULID-glob по каталогу заметок ищет файл `<ULID>-*.md` или `<ULID>.md` (один FS-call для конкретного префикса; читает один directory entry, не сканирует все). Это O(1) **относительно числа matches на конкретный prefix**, не O(1) относительно общего числа заметок (на N заметках readdir остаётся O(N) при первом обращении; FS-кэш амортизирует на повторных). Persistent index для ускорения inverse-lookup (backlinks) — отдельный вопрос B1.
- **Источник истины для resolve**: filename. Это намеренная асимметрия: для записи canonical — frontmatter `id`; для чтения резолва — filename-glob (быстро, без полного frontmatter scan). См. § Invariant validation ниже.

### Retitle lifecycle and recovery (применение CC-1)

**Retitle** — изменение `title:` в frontmatter заметки. Поведение:

1. zetto читает new title из frontmatter (после того как пользователь сохранил файл — vim/nvim/`zetto edit`).
2. zetto регенерирует slug через `slug::slugify(new_title)`, обрезанный до 60 chars.
3. Если new slug ≠ old slug в filename: `std::fs::rename(old_path, new_path)`, где `old_path` сохраняет ULID-префикс, меняется только slug-часть. ULID не трогается.
4. Если new slug = old slug: no-op.

**Atomicity**: rename файла + edit frontmatter — две независимые FS-операции. zetto **не оборачивает их в транзакцию** (frontmatter обычно правится `vim`, не zetto). Это создаёт окно partial state.

**Recovery procedure (при detected partial state)**:

- При запуске любой операции zetto проверяет invariant: ULID-префикс filename совпадает с frontmatter `id`. Если не совпадает — это либо partial-rename (vim сохранил new title, zetto не успел переименовать), либо ручной edit.
- В обоих случаях zetto **не делает auto-correct**. Выводит diagnostic: «file `01J9X-old.md` имеет frontmatter `id: 01J9X...` и title не соответствует slug; ожидается ULID-префикс совпадает с filename. Запусти `zetto resync <ULID>` для согласования slug или вручную правь filename».
- `zetto resync <ULID>` — best-effort: читает frontmatter title, регенерирует slug, переименовывает file. Это явная пользовательская команда, не auto-recovery.

**Multi-machine sync через git**: rename + body-edit на разных машинах в офлайне может произвести merge-конфликт уровня identity (delete-old + add-new vs modify-old). Это **известное ограничение**, не block для personal-use. Runbook:

1. Если git pull сообщает rename-conflict на заметке — открыть оба варианта вручную (`git log --follow`).
2. Решить: сохранить retitle (применить вторую сторону) или body-edit (применить первую). Обычно retitle важнее — он отражает финальное название.
3. Закоммитить merge resolution отдельным коммитом.

Совет: retitle делать в одной машине; body-edit — в любой; sync чаще, чем раз в неделю — снижает вероятность параллельного retitle + body-edit на одной заметке.

### Empty-slug lifecycle (применение CC-4)

`zetto new` без аргумента создаёт skeleton-файл `<ULID>.md` с пустым slug-ом и spawn-ит `$EDITOR`. Когда пользователь сохраняет файл с непустым `title:` в frontmatter:

- При следующем запуске zetto (`zetto open`, `zetto link`, `zetto lint`, etc.) zetto проверяет: filename имеет вид `<ULID>.md` (без slug), но frontmatter `title` непустой → автоматически срабатывает retitle-flow (см. выше) для подтягивания slug в filename. Это **не silent operation** — zetto выводит «pulled slug from title: `<ULID>.md` → `<ULID>-<slug>.md`».
- Альтернативно: `zetto resync` — explicit команда для обхода всех заметок с этим состоянием.
- Если пользователь намеренно хочет slug-less filename — установить `frontmatter: slug_locked: true` (escape-hatch). zetto в этом случае не регенерирует slug.

### Invariant validation (применение CC-2 / B-3, B-6)

**Инвариант**: `frontmatter.id == filename_ulid_prefix`. Когда пользователь правит `id:` руками или копирует файл (`cp note.md duplicate.md`) — invariant нарушается.

zetto при любой операции, читающей заметку, проверяет invariant. На detected violation:

- **Read-only операции** (`zetto open`, `zetto graph`): warning в stderr, продолжает работу, использует filename-prefix как identifier (потому что lookup идёт через filename-glob).
- **Mutate операции** (`zetto link`, `zetto retitle`, `zetto delete`): отказ с error-message «invariant violation: frontmatter `id: <X>` не совпадает с filename ULID-prefix `<Y>`. Resolve manually before continuing.»
- `zetto lint` отдельно перечисляет все nodes с broken invariant.

**Copy-paste deduplication**: при `cp` один ULID попадает в два файла. zetto при `lint` видит коллизию (два файла, один frontmatter `id:`) и отчётом просит выбрать одну из заметок. Auto-resolve не предлагается.

## Consequences

### Easier

- **Lookup `[[ULID]]`**: O(1) FS-glob запрос по ULID-префиксу filename. Persistent index не нужен для этой операции.
- **Sortable filenames**: `ls -1` показывает заметки в порядке создания. `git log --diff-filter=A` тоже хронологически выстроен.
- **Collision-free filenames**: ULID-префикс гарантирует, что `<ULID>-<slug>.md` не сталкивается с другой заметкой даже при одинаковых slug-ах.
- **Cross-machine sync**: ULID глобально уникален; параллельное создание на двух машинах не требует координации.
- **Brand identity**: ULID-prefix + slug — distinctive паттерн, узнаваемый в `ls`/`fzf` без TUI.

### Harder

- **Long filenames**: 26-char ULID + `-` + 60-char slug + `.md` = до ~92 chars в `ls -1`. Slug visually смещается в правый край; muscle-memory автодополнения слабее, чем у `<slug>.md`.
- **Migration cost при rebuilding ID-схемы**: A5 (Format versioning) предусматривает migration tool, но он не существует на момент этого ADR — см. § Implementation.
- **Retitle vs vim atomicity**: rename + frontmatter edit — две операции. Window partial state существует; recovery — не автоматическая (см. § Retitle lifecycle).

### Risks accepted

- **`serde_yaml` deprecated upstream** ([crates.io/crates/serde_yaml](https://crates.io/crates/serde_yaml)). David Tolnay архивировал крейт в марте 2024, помечен `no longer maintained`. zetto использует его в v0.x для записи frontmatter; **plan migration** на [`serde_yml`](https://crates.io/crates/serde_yml) или [`saphyr`](https://crates.io/crates/saphyr) при первом security advisory или MSRV-конфликте. Migration cost оценивается ~4–8 часов (одна точка вызова).
- **`std::fs::rename` на Windows** требует Rust toolchain с PR [rust-lang/rust#138133](https://github.com/rust-lang/rust/pull/138133) (стабилизирован в Rust 1.87+, май 2025), иначе на Windows <10 1607 и старых server-сборках получается `ERROR_INVALID_PARAMETER`. zetto pinит **MSRV ≥ 1.87** в `Cargo.toml` (`rust-version = "1.87"`).
- **`gray_matter` write-back fidelity**: lossy для пользовательских комментариев и порядка ключей в frontmatter. Если пользователь вручную добавляет комментарий, zetto его сотрёт при следующем retitle. Migration на YAML AST-парсер (`saphyr`, `yaml-rust2`) — 10–20 часов работы; пока не делается.
- **`@zetto/*` MCP-namespace**: out of scope (см. ADR-0001). Если zetto в будущем выпускает MCP-сервер, он живёт под `@kramar/zetto-mcp` или подобным.
- **Cross-machine ordering ULID**: approximately chronological modulo wall-clock skew. Skew между машинами одного пользователя обычно <1 секунда — несущественно для типового PKM-workflow.
- **CJK кандзи fallback**: в v1 кандзи дают путунхуа-romanization (через `deunicode`), нечитаемое для японского читателя. **Known limitation.** Для японско-тяжёлых vault-ов — рассмотреть `kakasi`/`lindera` в будущем (отдельный ADR, не блокер).

## Privacy and security considerations (применение CC-5)

- **ULID-timestamp в filename декодируется тривиально** (первые 48 бит). При публикации vault (open-source repo, public gist, передача файла коллеге) creation-times каждой заметки становятся видимыми. Это документированная характеристика формата, принятая trade-off.
- **Title через slug попадает в filename**, всплывающее в `git log --name-only`, `git diff`, shell history (`vim <filename>`, `zetto open <filename>`), swap-файлах vim/nvim, fzf/rg cache, ctags, LSP indexes, скриншотах `ls`/`tree`, remote git fork-копиях. Для sensitive title (NDA-материалы, медицинские/юридические заметки) пользователь несёт ответственность за выбор generic title или для использования `slug_locked: true` (см. § Empty-slug lifecycle).
- **Title в frontmatter сохраняется full-fidelity**: slug-deunicode-pass — это UX-affordance для filename, не privacy-механизм. Оригинальный title всегда доступен через любой текстовый search.
- **Encryption-at-rest** — ответственность пользователя (FS-level: FileVault/LUKS/dm-crypt/BitLocker). zetto хранит plain text. Любой механизм sync/share — отдельный ADR.
- **Note deletion**: `git rm <note>` + `git commit` удаляет файл из tip репозитория, но содержимое **остаётся в git history** и доступно через `git log -- <path>`, `git show <commit>:<path>`, любой существующий clone, fork, mirror. Для полного стирания нужен `git filter-repo` / BFG / переписывание истории. zetto не предоставляет deletion-команд; это явное user-decision.

## Alternatives considered

### 1. Alternative B — slug-only filename (`<slug>.md`)

ULID хранится только в frontmatter; filename = `<slug>.md`; коллизия slug — disambig через `-2`/`-3` суффикс. *Strength*: чистый `ls`, лучшая Obsidian-compat. *Weakness*: lookup `[[ULID]]` требует frontmatter scan (O(N) cold; persistent index обязателен с дня 1, что закрывает B1 в одну сторону); slug-collision logic нетривиальна; нет хронологической сортируемости. *Lost because*: closes B1 (graph index) преждевременно; стоимость persistent-index с самого начала не оправдана для personal-CLI с <1k заметок. См. полные trade-offs в [design](../research/2026-05-09-A1-note-id-scheme-design.md).

### 2. Alternative C — ULID-only filename (`<ULID>.md`)

ULID = filename; slug в frontmatter (для preview); никаких rename операций. *Strength*: простейшая реализация, нулевой rename-overhead, никогда не нарушает invariant. *Weakness*: «стена ID в `ls`» режет terminal-native UX; STRATEGY-метрика cross-session-usage и time-to-first-link страдают; D4 = strict-checker невозможен (Obsidian-vault имеет filename-as-title); ID-как-filename даёт меньше навигационных подсказок без TUI. *Lost because*: STRATEGY primary user — vim+tmux+git, daily в `ls`; cognitive cost wall-of-ID — не теоретическая. Roast B-4 показал, что в практике этот pain частично всё равно возникает (empty-slug = по умолчанию для свежих заметок), но это митигируется auto-retitle-flow в § Empty-slug lifecycle, не пересмотром выбора.

### 3. Status quo — нет решения

Не определять ID-схему явно; использовать filename = title (Obsidian-стиль) или random-имена. *Lost because*: STRATEGY-anti-pattern «фиксированная ID-схема» прямо требует противоположного; retitle ломает все backlinks; проект не стартует.

### Explicitly not considered

| Variant | Reason rejected |
|---|---|
| **UUID v4** (`550e8400-...`) | 36 chars, не сортируется по времени, нечитаем. ULID даёт всё то же + хронология + 26 chars. Прежний zetto использовал UUID — спайк, не контракт. |
| **Timestamp-only prefix** (`202605091230-on-fixed-ids.md`) | Community-feedback negative («visually noisy»); collision при одной мс; нет entropy для cross-device. |
| **UUID v7** (RFC 9562, 2024) | Time-prefixed, RFC-стандарт. Проигрывает ULID в чтении (36 vs 26 chars, hex vs Crockford). К 2027–2028 может стать предпочтительным default — см. § Open questions. |
| **Luhmann hierarchical** (`1a2b3c-on-fixed-ids.md`) | Требует ручного выбора (нарушает Discovery lean Q3 = auto-only); paper-era artifact. |
| **Three-letter human** (`zk1.md`, `zk2.md`) | Manual choice; vault-local uniqueness only; не масштабируется выше ~17k заметок. |
| **Content-hash** | Меняется при любом edit → не ID, а версия. |
| **Filename-as-title** (Obsidian-style) | Retitle ломает все backlinks (нарушает Discovery lean Q6 = ID константен); spaces в filename — против toolchain-interop. |
| **Slug + UUID-suffix** | UUID длиннее ULID, не сортируется; обратный порядок (suffix vs prefix) ухудшает sort. |

## Implementation

### Synchronous changes (этот коммит)

- **ADR-0002** — этот файл.
- **`ARCHITECTURE.md` §5 Decision Index** — запись для ADR-0002.
- **`docs/architecture/README.md` ADR Index** — та же запись.
- **`docs/architecture/decision-map.md`** — A1 переходит open → decided со ссылкой на ADR-0002; Suggested order Уровень 0 пересобирается (A1 удаляется из next-up; A2/A3/A4 разблокированы как Уровень 1; A5 как Уровень 2).
- **`ARCHITECTURE.md` §6 Open questions** — Q1 (ID schema) удаляется (закрыт этим ADR).

### MSRV

`Cargo.toml`: `rust-version = "1.87"`. Минимальный toolchain для корректной atomic-rename семантики на Windows и для современных features.

### Crate dependencies (когда начнётся реализация)

| Крейт | Версия | Назначение |
|---|---|---|
| `ulid` | `1.2.1` (pinned) | ULID generation + Crockford encoding |
| `slug` | `0.1.6` (pinned) | Title → ASCII slug |
| `gray_matter` | `0.2` (pinned major) | YAML frontmatter parsing (read) |
| `serde_yaml` | `0.9` (pinned, deprecated upstream — мигрировать к `serde_yml`/`saphyr` при первом security advisory) | YAML frontmatter serialization (write) |
| `atomic-write-file` | `0.2` (pinned major) | Atomic body writes |

`Cargo.lock` коммитится. `cargo update` — раз в квартал с `cargo audit` перед коммитом.

### Out-of-repo follow-ups

- **Migration tool `zetto migrate format-v1 format-v2`** пишется тогда, когда первая идея, требующая format-v2, проходит фазу Decide. До этого момента format-v1 неприкосновенен; любая такая идея откладывается. Это явный trigger вместо deferred-with-no-condition.
- **CI test-suite** для format-v1 invariants появляется вместе с первым кодом (зависит от других ADR — A2/A3).

### Format versioning anchor (применение CC-3)

ULID + slug filename layout фиксируется как **stable, versioned** компонент будущей спецификации `format-v1` (см. A5 в decision-map, статус — open; будет определён отдельным ADR-0006 или подобным). Stability ≠ frozen: layout стабилен до тех пор, пока не появится reasoned предложение format-v2. Migration tool — обязательный артефакт каждого major bump.

**ID-rendering в линке — implementation detail, не public ABI** (применение F-1). Третьи стороны (vim-плагины, скрипты, dataview-аналоги) могут полагаться на формат filename как на public ABI; на формат ULID-rendering внутри wikilink — нет (изменяется в A2 и любом будущем link-syntax-ADR без поднятия major-версии format-v1).

## Open questions

- **Multi-author author-attribution** — A3 (frontmatter convention) должен зарезервировать имя поля для author-attribution (применение F-5). Без этого multi-author scenario, если когда-то размораживается, будет восстанавливать attribution из git blame — хрупко.
- **D4 strict-checker variant excluded** — A1 неявно сужает D4 (Obsidian compat posture) до variants (a)/(b)/(c); strict-checker over arbitrary Obsidian-vault несовместим с фиксированным `<ULID>-<slug>.md` filename (применение F-6). Если D4 в итоге пойдёт в strict-checker — потребуется либо relax A1 (filename как рекомендация, не контракт), либо принять, что zetto-проверяемый vault — это zetto-vault, а не arbitrary Obsidian-vault.
- **CJK fallback policy** — текущий `slug` крейт через `deunicode` мапит кандзи на путунхуа. Для японско-тяжёлых vault-ов — open question, отдельный ADR при появлении пользователя.

## Reviews trail

- 2026-05-09 — Discovery ([2026-05-09-A1-note-id-scheme-discovery.md](../research/2026-05-09-A1-note-id-scheme-discovery.md))
- 2026-05-09 — Research digest ([2026-05-09-A1-ulid-slug-rust-research.md](../research/2026-05-09-A1-ulid-slug-rust-research.md))
- 2026-05-09 — Design ([2026-05-09-A1-note-id-scheme-design.md](../research/2026-05-09-A1-note-id-scheme-design.md))
- 2026-05-09 — Decision summary ([2026-05-09-A1-decision-summary.md](../research/2026-05-09-A1-decision-summary.md))
- 2026-05-09 — Roast (5 roles, severity: 4H/11M/5L) — [00-summary.md](../reviews/2026-05-09-roast-A1-note-id-scheme/00-summary.md)
- 2026-05-09 — Meta-review on roast — [99-meta-review.md](../reviews/2026-05-09-roast-A1-note-id-scheme/99-meta-review.md)
