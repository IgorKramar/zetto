# Architectural Decision Records

Каталог содержит accepted ADR-ы проекта zetto. Файлы пронумерованы (`NNNN-<slug>.md`) и **append-only** — принятые решения не редактируются (за исключением status-перевода в `Deprecated` или `Superseded by ADR-NNNN`). Изменения принятого ADR-а появляются как **новый ADR** со ссылкой на superseded.

## Index

| # | Date | Status | Decision |
|---|---|---|---|
| [0008](./0008-capture-publish-and-root.md) | 2026-10-29 | Accepted | Capture/publish, root и уточнение STRATEGY |
| [0007](./0007-shared-cli-tui-core.md) | 2026-10-29 | Accepted | Общее ядро CLI/TUI |
| [0006](./0006-safe-writes-and-recovery.md) | 2026-10-29 | Accepted | Конкурентная запись и recovery |
| [0005](./0005-format-layout-and-resolution.md) | 2026-10-29 | Accepted | Формат, размещение, resolver, YAML preservation |
| [0004](./0004-frontmatter-convention.md) | 2026-05-09 | Accepted | Frontmatter convention: required `id`/`title`; standard optional `tags`/`aliases`/`created`/`updated`; `x-*` for extensions; lenient schema with lint warns; hand-rolled write |
| [0003](./0003-link-representation.md) | 2026-05-09 | Accepted | Link representation: wikilink-primary `[[ULID]]` / `[[ULID|display]]`; canonical markdown read-compat; embeds/anchors/block-refs deferred to v2 with triggers |
| [0002](./0002-note-id-scheme-and-filename-layout.md) | 2026-05-09 | Accepted | Note ID scheme and filename layout: ULID in YAML frontmatter, `<ULID>-<slug>.md` filename |
| [0001](./0001-project-name-and-ecosystem-positioning.md) | 2026-05-09 | Accepted | Rename project from `zk` to `zetto` |

Дублируется в [`../README.md`](../README.md) ADR Index и в [`../../ARCHITECTURE.md`](../../ARCHITECTURE.md) §5 Decision Index — каждый ADR попадает в три места одновременно при принятии.

## Naming convention

`NNNN-<short-slug>.md`:
- **NNNN** — zero-padded sequence (4 цифры), никогда не переиспользуется. Следующий ADR-0004 получит свой номер даже если ADR-0003 будет superseded.
- **slug** — kebab-case, описывает содержание (3–6 слов).

## Lifecycle

- **Proposed** — черновик, ещё обсуждается. Через `/archforge:cycle` редко возникает (cycle проводит до Accepted), но можно зафиксировать через `/archforge:adr` с явным `Status: Proposed`.
- **Accepted** — принятый ADR; реализация может опираться на него как на контракт.
- **Deprecated** — решение отозвано (но не заменено). Указывается дата и причина.
- **Superseded by ADR-NNNN** — заменено новым решением. Перевод в этот статус — **редактирование статус-строки**, остальное тело ADR не трогается.

ADR-ы **никогда не удаляются**: история принятия решений — часть архитектурного следа.

## How to add an ADR

- `/archforge:cycle "<problem>"` — full deep cycle (Discover → Research → Design → Decide → Roast → Document → Meta-review). Для high-stakes/cross-cutting решений.
- `/archforge:adr "<decision>"` — shortcut для уже принятых решений; пишет ADR без roast/meta-review.

После accepted-state ADR должен быть отражён в трёх местах: этот index, `../README.md` ADR Index, `../../ARCHITECTURE.md` §5 Decision Index.

## See also

- [`../README.md`](../README.md) — навигация по `docs/architecture/` (decisions / diagrams / research / reviews).
- [`../../ARCHITECTURE.md`](../../ARCHITECTURE.md) §5 — top-level Decision Index.
- [`../decision-map.md`](../decision-map.md) — карта открытых решений с зависимостями; ADR-ы закрывают записи на этой карте.
- [`../research/`](../research/) — discovery, research digest, design alternatives, decision summaries (input для ADR-ов).
- [`../reviews/`](../reviews/) — roast и meta-review артефакты (output из cycle-а перед написанием ADR).
