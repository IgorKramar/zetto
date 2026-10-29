# ADR-0004: Frontmatter convention

> Принятые уточнения от 2026-10-29:
> [ADR-0005](0005-format-layout-and-resolution.md) заменяет alias-resolver,
> validation pipeline, write strategy и обещание verbatim: сохраняются
> значения, не оформление; неверные optional-поля не скрывают заметку.
> [ADR-0006](0006-safe-writes-and-recovery.md) заменяет single-writer/mtime
> и протокол записи; [ADR-0008](0008-capture-publish-and-root.md) заменяет
> запрет пустого title в inbox и скрытое получение title из H1.
> `updated` меняется только управляемой записью по [format-v1](../../format-v1.md).
> Обязательные поля id/title и namespace расширений сохраняются;
> исторический текст ниже оставлен для прослеживаемости.

- **Date**: 2026-05-09
- **Status**: Accepted
- **Authors**: Igor Kramar
- **Cycle**: A3 (decision-map.md), deep
- **Predecessor**: ADR-0002 (`id:` field), ADR-0003 (`title:` подразумевается через render-fallback)
- **Decided**: A3 — индекс открытых архитектурных решений в [`decision-map.md`](../decision-map.md)
- **Affects (разблокирует или ограничивает)**: A4, A5, B1, B2, C2a, C5, D4

## Context

ADR-0002 зафиксировал `id:` (ULID, canonical в YAML frontmatter); ADR-0003 — что render-fallback читает `title:` из frontmatter target-заметки. ADR-0004 фиксирует **полную схему frontmatter zetto-заметки**: какие поля есть, какие обязательны, какие optional, как валидируются, что zetto делает при unknown полях, как пользователь добавляет custom-поля. Решение становится частью **format-v1 spec contract** (см. A5 в decision-map, статус — open; будет определён отдельным ADR).

**Идентификаторы в этом ADR**: A3, A4, A5, B1, C2a, D4 — индексы решений в [`decision-map.md`](../decision-map.md). Format-v1 — будущая публичная спецификация формата заметок (A5); этот ADR резервирует frontmatter-часть авансом. C2a — Methodology rule engine architecture (open); все lint-rule names здесь резервированы, semantics severity (`error`/`warn`/`info`) определяется в C2a.

**Discovery** ([2026-05-09-A3-frontmatter-convention-discovery.md](../research/2026-05-09-A3-frontmatter-convention-discovery.md)) зафиксировал 7 leans:
1. Required: `id` (ADR-0002), `title` (ADR-0003 render-fallback). Остальное optional.
2. Standard optional: `tags`, `aliases`, `created`, `updated`.
3. `tags:` syntax: YAML flat list primary; inline `#tag` recognized at read для derived view но не сохраняется в `tags:` array.
4. `aliases:` включён в v1 (D4=read-write anticipation).
5. `created` auto-set zetto при `zetto new`; `updated` config-knob default-auto on save.
6. Schema strictness: lenient + lint-warn для unknown.
7. Custom fields: `x-*` prefix convention (borrow from OpenAPI/JSON Schema, **без PKM-precedent**).

**Research digest** ([2026-05-09-A3-frontmatter-convention-research.md](../research/2026-05-09-A3-frontmatter-convention-research.md)) подтвердил: `pulldown-cmark` 0.13.3 + `gray_matter` 0.2 идиоматичный путь; Obsidian 1.12.7 регресс — wikilink resolver не использует `aliases:` (D4-ожидание не оправдается через Obsidian native — нужен zetto-side resolver); `gray_matter` round-trip lossy для comments/key-order — hand-rolled write предпочтительнее парсить-и-перепарсить; `updated` content-hash pattern из `obsidian-frontmatter-modified-date` плагина.

**Roast** ([reviews/2026-05-09-roast-A3-frontmatter-convention/](../reviews/2026-05-09-roast-A3-frontmatter-convention/00-summary.md)) пятью ролями обнаружил ~42 finding, 7 cross-cutting concerns. Решение по сути не оспорено. 14 текстовых правок применены ниже.

## Decision

YAML frontmatter с **двумя обязательными полями** (`id`, `title`) и **четырьмя стандартными опциональными** (`tags`, `aliases`, `created`, `updated`); custom-поля под namespace `x-*`; схема **lenient** (preserve unknown verbatim, lint warn для unprefixed unknown); **hand-rolled write** в фиксированном порядке полей с явными ограничениями (см. § Write strategy); **single-writer concurrency assumption**.

### Required fields

- **`id: <ULID>`** — string, regex `^[0-9A-HJKMNP-TV-Z]{26}$` (Crockford-base32 uppercase) + range-check на decoded timestamp (≤ 2^48-1 ms). Per ADR-0002. Lint rules: `zetto/missing-required-field` (severity error) + `zetto/invalid-id-format` (severity error).
- **`title: <string>`** — non-empty UTF-8 string. Per ADR-0003 render-fallback. Lint rule `zetto/empty-title` (severity warn).

### Standard optional fields

- **`tags: [<string>, ...]`** — flat YAML list. Tags = facets (STRATEGY anti-pattern «теги ≠ ссылки»). Recommendation (не enforce): kebab-case lowercase. **Inline `#tag` в body**: распознаётся при чтении и попадает в derived `zetto tags`-output (CLI-команда), но **не записывается обратно** в `tags:` array. Coercion `tags: tag1` (scalar) → `Vec<String>` через custom `deserialize_with` helper; на failure — lint error `zetto/invalid-tags-format`.
- **`aliases: [<string>, ...]`** — flat YAML list альтернативных titles. Используется **zetto-собственным alias-resolver-ом** (см. § Aliases-resolver — постоянная часть on-disk contract).
- **`created: <RFC 3339 timestamp>`** — auto-set zetto при `zetto new`. UTC. Format example: `2026-05-09T12:34:56Z`. User-edit-able (zetto preserves on subsequent saves).
- **`updated: <RFC 3339 timestamp>`** — **manual в v1, auto в v1.x после B1** (применение CC-1). До B1: `updated` поле manual — пользователь сам обновляет, либо config `update_timestamp: off` (default). После B1 (graph index): auto-managed через content-hash тела (без frontmatter), update iff hash changed. Pattern от `obsidian-frontmatter-modified-date`. Per-note opt-out через `x-skip-updated: true`.

### Aliases-resolver — постоянная часть on-disk contract (применение CC-4 / F-1)

zetto-side alias-resolver — **canonical zetto behaviour**, не временная заглушка под Obsidian-баг. Even если Obsidian починит свой resolver в 2027, zetto-side resolver остаётся (за 1–2 года накопит собственные contract-правила: case-insensitive, ULID-creation-time tiebreak при collision, кэш-инвалидация).

При резолве `[[X]]` после стандартных проходов (per ADR-0003 §Resolver):

1. ULID literal match — primary.
2. Filename-prefix match (через ULID-prefix-extract).
3. **Alias match** — scan `aliases:` всех заметок. До B1 (graph index, open) — synchronous scan; default disabled при vault > 200 заметок (новый lint info `zetto/alias-resolver-disabled-at-scale`). После B1 — index lookup O(1).
4. Filename match (legacy / imported content).

**Лимиты на collision**: если alias-match возвращает > 100 кандидатов — alias-rule disabled с lint info `zetto/alias-collision-saturation`; zetto предпочитает первый по ULID-creation-time tiebreak.

### Custom user/extension fields — `x-*` namespace

**Convention**: `x-<name>` prefix. **Без PKM precedent** — собственный design zetto.

**Rationale block** (применение CC-6 / F-2): рассмотрены альтернативы:
- `zetto:` prefix (vendor-specific) — слишком zetto-locked, не позволяет user-script-ам без префикса.
- `_` prefix (Python convention для private) — не несёт «extension»-семантики; конфликтует с YAML-internal-keys в некоторых tools.
- Free-form custom (как Obsidian/Logseq) — нет signal для линтера различить «typo пользователя» от «намеренное extension».
- **`x-*`** (chosen) — borrow from OpenAPI/JSON Schema; чёткий signal «это extension, не typo».

**Behavior**:
- Prefixed `x-*` (например `x-mood: happy`): preserved verbatim, никогда не lint-flag-ятся.
- Unprefixed unknown (например `mood: happy` без `x-`): preserved verbatim, **lint flag `zetto/unknown-frontmatter-field`** (severity warn в `recommended-luhmann` preset, off в `lenient` preset).

### Schema strictness — lenient (one mode in v1)

Schema strictness — это поведение парсера на нестандартные поля. zetto v1 поддерживает **один режим — lenient**. Strict-режим (reject unknown) не реализован в v1.

- **Required missing** → lint error + zetto refuses mutate-операции (write/link/retitle); read-only операции работают with stderr warning.
- **Required invalid format** (например, `id` не валидный ULID) → lint error + same refuse-mutate.
- **Standard optional invalid format** (например, `created` не RFC 3339) → lint warn (`zetto/non-rfc3339-timestamp`); zetto не блокирует операции.
- **Unknown без `x-*` prefix** → lint warn; preserved verbatim.
- **Unknown с `x-*` prefix** → silent preserve.

### Validation pipeline

1. **Parse** через `gray_matter` (per ADR-0002 §Crate dependencies) → YAML AST.
2. **Deserialize** в struct:
   ```rust
   #[derive(Deserialize)]
   struct Frontmatter {
       id: Ulid,
       title: String,
       #[serde(default)] tags: Vec<String>,
       #[serde(default)] aliases: Vec<String>,
       #[serde(default)] created: Option<DateTime<Utc>>,
       #[serde(default)] updated: Option<DateTime<Utc>>,
       // BTreeMap обеспечивает alphabetical iteration на write;
       // serde_yaml::Value сохраняет original YAML для verbatim render.
       #[serde(flatten)] extra: BTreeMap<String, serde_yaml::Value>,
   }
   ```
3. **Required-check**: `id` parsed (else lint error), `title` non-empty.
4. **Optional-check**: `created`/`updated` parse как RFC 3339 (else lint warn).
5. **Extra-bag split**: split keys по `x-` prefix → silent preserve vs lint-warn unknown.

### Write strategy — hand-rolled с явными ограничениями (применение CC-2 / B-2, H-2, H-6)

**Scope**: zetto-managed write для known fields в фиксированном порядке + flat preserve unknown scalars. Nested unknown YAML — best-effort через `serde_yaml::to_string`; **может потерять comments/anchors**.

**Шаблон**:
```
---
id: <ULID>
title: <quoted-title>
tags: [<tag1>, <tag2>]              # only if non-empty
aliases: ["<alias1>", "<alias2>"]    # only if non-empty
created: <timestamp>                 # only if set
updated: <timestamp>                 # only if set
<x-extensions in alphabetical order>
<unknown without prefix in alphabetical order, preserved verbatim>
---
```

**Known limitations** (документируются явно):
- **Comments в frontmatter теряются** на zetto-write. zetto не пишет комменты сам; user-edited комменты сохраняются только до первого retitle/save с изменением известных полей. Не «mitigation», а явное ограничение.
- **Nested unknown YAML** (например, `mood: {complex: nested}`): zetto re-emit-ит через `serde_yaml::to_string`; формат нормализуется, anchors теряются.
- **YAML quoting policy**: zetto quotes `title` и `aliases` (избегает edge cases с YAML-special-chars `:`, `#`, `[`, `]`, `&`, `*`, `?`); tags — bare если match `^[a-zA-Z][\w-]*$`, иначе quoted.

**Read→write идемпотентность invariant** (применение F-4): test-обязательный — для любой valid v1 заметки `read → write` идемпотентен **byte-for-byte для known fields**. Любое изменение в writer-output на known fields ломает invariant и должно сопровождаться явной migration policy.

### Concurrency and atomicity (применение CC-3 / B-3)

**Single-writer assumption**: zetto предполагает, что в каждый момент времени **только один процесс** правит файл. Concurrent writes от двух zetto-процессов или от zetto + vim параллельно — **undefined behavior**.

**Atomic write**: zetto использует `atomic-write-file` (per ADR-0002 §Crate dependencies) — write-to-temp-then-rename pattern. Это исключает corrupt files при partial write (Ctrl-C, OOM-kill, crash).

**CAS на mtime** (best-effort): перед write zetto читает текущий mtime файла; если он отличается от mtime прочитанного при load — abort с error message «file changed externally; re-run command». Это не CAS-в-строгом-смысле (race-window между check и rename остаётся), но снижает вероятность silent overwrite в типовом случае «vim открыт в другом терминале, zetto retitle тут».

### Lint rules (имена резервируются здесь, semantics в C2a)

`zetto/*` namespace **reserved for built-in rules**; future third-party rule namespacing определяется в C2a (применение F-6).

| Rule ID | Description | Default severity (предложение) |
|---|---|---|
| `zetto/missing-required-field` | `id` или `title` отсутствует | error |
| `zetto/invalid-id-format` | `id` не валидный ULID или out-of-range timestamp | error |
| `zetto/empty-title` | `title: ""` | warn |
| `zetto/non-rfc3339-timestamp` | `created` или `updated` не валидный RFC 3339 | warn |
| `zetto/invalid-tags-format` | `tags:` failed coercion в `Vec<String>` | error |
| `zetto/unknown-frontmatter-field` | поле без `x-*` prefix не в standard set | warn в `recommended-luhmann`, off в `lenient` |
| `zetto/tag-not-in-frontmatter` | inline `#tag` в body найден, но не в `tags:` | info (suggestion) |
| `zetto/non-canonical-tag-format` | `tags: tag1` (scalar) — coerced, suggest list | info |
| `zetto/empty-alias` | `aliases: ["", "  "]` | warn |
| `zetto/duplicate-frontmatter-key` | два одноимённых ключа в YAML | warn |
| `zetto/alias-collision-saturation` | alias-match > 100 кандидатов — rule disabled | info |
| `zetto/alias-resolver-disabled-at-scale` | vault > 200 заметок без B1 — alias-resolver выключен | info |
| `zetto/long-title` | `title` > 200 chars | info |

`recommended-luhmann` и `lenient` — два встроенных preset-а lint-rule severity (см. C2a; default — `recommended-luhmann`); пользователь переключает через `.zettorc` поле `lint.preset`.

### `zetto new` skeleton template (применение CC-5 / H-5)

`zetto new` создаёт skeleton-файл с обязательными полями:

```yaml
---
id: <auto-generated ULID>
title: <slug-derived placeholder OR empty + prompt>
created: <auto-set RFC 3339 timestamp>
---
# <H1 placeholder>

```

При first save после редактирования: если `title:` пуст → zetto auto-derive из первого H1 в body (если есть); иначе lint error `zetto/empty-title` блокирует mutate-операции с подсказкой `Add title via 'zetto retitle <ULID>' or edit frontmatter manually`.

### Forward-compat statement

A3 расширяет format-v1 spec contract **additively**: новые standard fields могут быть добавлены в minor format-v1.x bump (additive change, не breaking). Например, future `description:`, `status:`, `cssclasses:` — добавятся при появлении use case без format-v2 migration.

**Trigger conditions для defer-list** (применение CC-7 / F-3):
- `description:` — promote когда первая заметка vault использует search-friendly summary > 5 раз.
- `status:` — promote когда `x-status` появляется в ≥3 заметках реального vault.
- `type:` — promote когда `x-type` появляется в ≥3 заметках.
- `prev:` / `next:` (Folgezettel) — promote только при явном запросе пользователя (community split).
- `cssclasses:` — promote при D4=read-write Obsidian-vault use case.

Без срабатывания триггера — feature остаётся в defer-state. **Если триггеры не срабатывают за 18 месяцев** — переоткрыть Open question: продолжать ли defer или признать «v2 не наступит» и закрыть как non-goal до format-v2.

**Schema-version field**: `format: 1` — НЕ требуется (per ADR-0002 § Format versioning anchor). format-v1 — implicit; vault без `format:` field считается v1. format-v2 (если когда-то наступит) фиксирует `format: 2` обязательным.

**Batch-операции и `updated:`** (применение F-5): batch-операции, инициированные zetto-tool (миграции, mass-rename, reformat tags), имеют **explicit `updated:` mode** через флаг (`--update-timestamp=skip|set-now|preserve`); не наследуют content-hash логику auto-update. Default — `--update-timestamp=preserve` для миграционных операций (изменение format-v1 → v1.x не должно «обновлять» все заметки в один день).

## Consequences

### Easier

- **Минимально-возможный обязательный контракт** (`id`+`title` only) — низкий порог входа для свеже-созданных заметок.
- **C2a получает minimum surface для rules** — `recommended-luhmann` пишет against `tags`, `aliases`, `created`, `updated`.
- **D4 anticipation**: `aliases:` в v1 готов; zetto-side resolver работает независимо от Obsidian-status.
- **`x-*` namespace** — расширяемость для plugins/scripts без PKM-конвенции; lint-signal различает namespace from typo.
- **Hand-rolled write** изолирует write path от vendor risk на YAML-парсер-экосистеме (forward F-7).
- **Additive minor format-v1.x bump policy** — новые поля добавимы без migration tool.

### Harder

- **Comments в frontmatter теряются** после zetto-write — known limitation, документируется.
- **`aliases:` в Obsidian-current-version (1.12.7) не работает** для wikilink-резолва — zetto-side resolver обязателен; D4=read-write через Obsidian native не работает out-of-the-box.
- **13 lint rules** в v1 — дополнительная сложность для C2a (но C2a их implementation так или иначе должен поддерживать).
- **`zetto/*` namespace reserved** — third-party plugin host (deferred per decision-map) потребует параллельный namespace; решается в C2a.
- **Hand-rolled write** требует ~30–50 unit tests + quoting state machine; growing с каждым новым standard field.
- **`updated:` manual в v1** — пользователь сам обновляет; auto-management ждёт B1.

### Risks accepted

- **`x-*` namespace без PKM precedent**: каждый новый пользователь и сторонний инструмент сначала спросит «что это». Митигация — rationale block в ADR + section в `docs/architecture/format-v1.md` (когда A5 закроется).
- **Defer-list (description/status/type/prev/next/cssclasses) → forever-deferred risk**: trigger conditions смягчают, но не устраняют; explicit «defer = non-goal до format-v2» — fallback при не-срабатывании 18-месячных триггеров.
- **`gray_matter` deprecation/replacement risk** на горизонте 2 лет (per F-7); hand-rolled write hedges write path; read-path остаётся exposed.
- **Obsidian 1.12.7 alias-resolver регресс** может не быть починен — zetto-side resolver работает permanent, не временная мера.
- **`updated:` content-hash pattern** ломается на batch-операциях (rename, reformat, migration) — митигировано через `--update-timestamp` флаг; default `preserve` для миграций.
- **`serde_yaml` deprecated upstream** (наследие из ADR-0002) — план миграции на `serde_yml`/`saphyr` при первом security advisory.

## Alternatives considered

Подробное сравнение и trade-off matrix — в [2026-05-09-A3-frontmatter-convention-design.md](../research/2026-05-09-A3-frontmatter-convention-design.md). Здесь — выжимка.

### 1. Lean-set canonical schema (выбрана)

YAML frontmatter с required `id`+`title`, standard optional `tags`/`aliases`/`created`/`updated`, custom `x-*`, lenient + lint-warn, hand-rolled write в фиксированном порядке. Соответствует 7 leans Discovery + research insights.

*Сильная сторона*: minimal required + рабочий standard set; D4 anticipation через `aliases:` без upfront-cost полной D4-реализации; C2a получает minimum surface для rules; forward-compat additive.

*Слабая сторона*: comments-loss known limitation; 13 lint rules для maintain; `x-*` без PKM precedent требует rationale защищать.

### 2. Maximalist (richer standard schema)

A + дополнительные standard fields: `description`, `status`, `type`, `prev`/`next`, `cssclasses`. Богатый PKM-вокабуляр из коробки.

*Сильная сторона*: Folgezettel-sequence, lifecycle-states, Obsidian-cssclasses out-of-the-box.

*Слабая сторона*: больше lint-rules для maintain; больше cognitive cost при создании заметки; каждое поле — потенциальная migration-точка через format-v2; Folgezettel controversial. *Lost because*: extra fields добавляют schema-surface без явного use case в pre-alpha; **каждый из них может быть введён additive в format-v1 minor-bump** позже, когда use case появится. Vorab фиксировать — over-design.

### 3. Minimalist (only `id`+`title` required; everything else custom)

Required: `id`, `title`. Standard optional: нет. Любое дополнительное поле — custom. Schema super-lenient.

*Сильная сторона*: минимально строгий контракт; max user-customization; meta-cost zero (никаких lint-rules для standard fields).

*Слабая сторона*: **никакого PKM-функционала из коробки** — `tags`/`aliases`/`created`/`updated` каждый пользователь придумывает сам. **D4 incompatible** — Obsidian ожидает `tags`/`aliases`. Lint-engine (C2a) не имеет surface для writing rules — zetto становится «editor with constraints», не «PKM with methodology». *Lost because*: ни одна цель STRATEGY не достигается без минимального standard set; C2a превращается в no-op; D4 закрыт.

### 4. Status quo — отложить A3 до C2a / B1

Не определять frontmatter schema сейчас; ждать C2a (rule engine) или B1 (graph index) или появления первого реального кода.

*Сильная сторона*: zero effort немедленно; решение откладывается в момент, когда есть больше empirical data.

*Слабая сторона*: A3 блокирует A5 (format-v1 spec нужно знать frontmatter-часть), B1 (что индексировать), B2 (что парсить за пределами markdown body), C2a (rules against чего), D4 (Obsidian-compat нужно знать формат полей). *Lost because*: блокировка пяти других решений не оправдана ожидаемым ROI от отложения.

### Explicitly not considered

| Variant | Reason rejected |
|---|---|
| **TOML frontmatter** | ADR-0002 зафиксировал YAML; D4-compat (Obsidian/Logseq/Foam все ожидают `---`-delimited YAML); `gray_matter` setup. |
| **No frontmatter at all** | Required `id` и `title` — нужен какой-то transport. |
| **Inline `key:: value` Logseq-style** | Non-YAML-compliant; противоречит «plain markdown» STRATEGY anti-pattern. |
| **Strict schema (reject unknown)** | Kills extension-friendliness; никто из community PKM не делает; противоречит lean Q6 (lenient default). |
| **Properties в body/inline (`Properties: ...` heading-section)** | Sample mdBook pattern; ломает markdown-rendering для Obsidian/Logseq. |
| **Hugo-style `params:` namespacing** | Adds nesting cost без явного benefit; PKM convention — flat. |
| **Free-form custom без `x-*` namespace** (как Obsidian) | Лишает signal для линтера различить «typo» vs «extension». |
| **Folgezettel `prev:`/`next:` в v1** | Часть Альтернативы 2; controversial; добавимое позже как additive minor format-v1.x bump. |

## Privacy and security considerations

Этот ADR расширяет приватность-периметр ADR-0002 §Privacy и ADR-0003 §Privacy на frontmatter schema:

- **`title:` в frontmatter** — sensitive title leak channel при export/share одной заметки (см. ADR-0003 §Privacy CC-2). A3 фиксирует `title:` как **обязательное** — не позволяет «no title для приватных заметок» как escape hatch. Future export-mode определит redaction policy.
- **`created:` timestamp** — раскрывает время создания при share. Дублирует ULID-decoded time (per ADR-0002 §Privacy); явное `created:` поле — additional privacy surface, но low because ULID уже это раскрывает.
- **`aliases:` field** — может leak alternative human-readable forms title (code-name, nickname, pseudonym). При share — те же caveats как `title:`.
- **`tags:` field** — могут содержать sensitive labels (medical, legal, financial). При share/grep tag-pane — sensitive surface. zetto не sanitize.
- **Custom `x-*` fields** — пользователь несёт ответственность; zetto preserves verbatim, no sanitize. Включая случайно вставленные секреты/credentials. Recommendation: не использовать `x-*` для секретов; encryption-at-rest (FS-level FileVault/LUKS) — обязательно для sensitive vault.
- **`updated:` content-hash** (после B1) — hash тела ≈ 64 hex chars. **НЕ кэшировать в frontmatter** (дополнительная metadata surface при share); cache в external state (sqlite в B1).
- **Aliases-resolver scan** — implicit cross-note read access; в multi-author scenario (deferred) станет ACL question.
- **Lint-сообщения цитируют user-token** (`zetto/empty-title` показывает path; `zetto/non-ulid-wikilink-target` цитирует токен). Implementation note: lint-output умеет режим `--quiet-targets`.

## Open questions

- **A4** (notes directory layout): не зависит от A3, но layout определит пути в filename-globs.
- **A5** (format versioning policy): A3 фиксирует additive minor-bump policy; full format-v1 spec — в A5.
- **B1** (graph index): `aliases:` resolver требует index для O(1); до B1 — disabled при vault > 200; `updated:` auto-management ждёт B1.
- **C2a** (rule engine): 13 lint rules имена резервированы здесь; semantics + engine architecture в C2a (open).
- **D4** (Obsidian compat posture): A3 совместима с D4 ∈ {own format, read-only, read-write через aliases via zetto-side resolver}. Strict-checker исключён.
- **Multi-author author-attribution** — отдельный future ADR; A3 не резервирует имя поля.
- **format-v1.x additive bumps** (description, status, type, prev/next, cssclasses) — отложены до конкретного use case с явными trigger conditions.
- **18-month trigger sunset**: если defer-list триггеры не срабатывают за 18 месяцев — переоткрыть «defer = non-goal до format-v2».
- **Encryption-at-rest для sensitive vaults** — out-of-scope этого ADR; политика в future ADR.

## Reviews trail

- 2026-05-09 — Discovery ([2026-05-09-A3-frontmatter-convention-discovery.md](../research/2026-05-09-A3-frontmatter-convention-discovery.md))
- 2026-05-09 — Research digest ([2026-05-09-A3-frontmatter-convention-research.md](../research/2026-05-09-A3-frontmatter-convention-research.md))
- 2026-05-09 — Design ([2026-05-09-A3-frontmatter-convention-design.md](../research/2026-05-09-A3-frontmatter-convention-design.md))
- 2026-05-09 — Decision summary ([2026-05-09-A3-decision-summary.md](../research/2026-05-09-A3-decision-summary.md))
- 2026-05-09 — Roast (5 roles, severity: 5H/9M/7L) — [00-summary.md](../reviews/2026-05-09-roast-A3-frontmatter-convention/00-summary.md)
- 2026-05-09 — Meta-review on roast — [99-meta-review.md](../reviews/2026-05-09-roast-A3-frontmatter-convention/99-meta-review.md)
