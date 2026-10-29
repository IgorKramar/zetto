# `docs/architecture/`

The architectural trail of this project. Updated continuously alongside the code.

## Layout

| Directory | Contents |
|---|---|
| [`decisions/`](./decisions/) | **ADRs** — one file per architectural decision. Numbered, never deleted. Index below. |
| [`diagrams/`](./diagrams/) | **C4 diagrams** as Mermaid `.md` files (or sources alongside images). Context (L1), Container (L2), Component (L3) views. |
| [`research/`](./research/) | **Discovery and design notes** from `/archforge:discover` and `/archforge:design`. Snapshots of the problem space at the time of investigation. |
| [`reviews/`](./reviews/) | **Architectural reviews** from `/archforge:review`. Reviews of substantial changesets, kept for posterity. |

The root [`ARCHITECTURE.md`](../../ARCHITECTURE.md) is the living high-level document. ADRs and diagrams here are the artifacts that produced it.

## ADR index

<!--
Sorted by number, newest first. Entries are mirrored from ARCHITECTURE.md
section 5.
-->

| # | Date | Status | Decision |
|---|---|---|---|
| [0008](./decisions/0008-capture-publish-and-root.md) | 2026-10-29 | Accepted | Capture/publish и root |
| [0007](./decisions/0007-shared-cli-tui-core.md) | 2026-10-29 | Accepted | Общее ядро CLI/TUI |
| [0006](./decisions/0006-safe-writes-and-recovery.md) | 2026-10-29 | Accepted | Безопасная запись и recovery |
| [0005](./decisions/0005-format-layout-and-resolution.md) | 2026-10-29 | Accepted | Формат/размещение/resolver и YAML |
| [0004](./decisions/0004-frontmatter-convention.md) | 2026-05-09 | Accepted | Frontmatter convention: required `id`/`title`; standard optional `tags`/`aliases`/`created`/`updated`; `x-*` for extensions; lenient schema |
| [0003](./decisions/0003-link-representation.md) | 2026-05-09 | Accepted | Link representation: wikilink-primary `[[ULID]]` / `[[ULID|display]]`; markdown read-compat; embeds/anchors/block-refs deferred to v2 with triggers |
| [0002](./decisions/0002-note-id-scheme-and-filename-layout.md) | 2026-05-09 | Accepted | Note ID scheme and filename layout: ULID in YAML frontmatter, `<ULID>-<slug>.md` filename |
| [0001](./decisions/0001-project-name-and-ecosystem-positioning.md) | 2026-05-09 | Accepted | Rename project from `zk` to `zetto` |

## How to use

- New architectural decision → `/archforge:cycle "<problem>"` (full process) or `/archforge:adr "<decision>"` (shortcut).
- Need a diagram → `/archforge:c4 <level> "<subject>"`.
- Architectural review of a PR or directory → `/archforge:review [path]`.
- Just looking around → start with [`../../ARCHITECTURE.md`](../../ARCHITECTURE.md), then drill into ADRs.

ADRs are append-only. Decisions that no longer hold are not edited — they're superseded by a new ADR. This preserves the history of how the team's thinking evolved.
