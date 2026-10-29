# Корпуса контрактов

Автор: Igor Kramar. Ожидания соответствуют предлагаемым ADR-0005–0008.

- `yaml.json`: 32 независимых YAML-сценария. `expected` — полное дерево
  значений, `diagnostics` — результаты проверок полей, `rejected` — отказ
  на уровне YAML. Стенд проверяет все непровальные случаи после сериализации
  и стабильность повторной канонической записи. Это не frontmatter/Markdown parser.
- `small/`: 8 заметок (6 published, 2 inbox), 5 однозначных рёбер. Есть root,
  общие aliases, сирота, broken/ambiguous links, ссылки внутри кода,
  пустой draft. Каталог пригоден для будущего сквозного сценария; диагностические
  проблемы намеренны. В manifest edges только разрешённые рёбра.
- `invalid/`: 9 файлов: отсутствующие обязательные поля, синтаксис YAML,
  дубликат ключа, дубликат ID, mismatch имени, неверные optional-поля,
  overflow ULID и git-conflict. Ошибки не должны скрывать сам файл из списка.
  `diagnostics` в vault manifests перечисляет обязательные проблемы сценария,
  а не полный будущий lint-отчёт со всеми советами и root-info.
- `links.json`: входы/ожидаемые исходы для будущих тестов resolver.
  Проверяет контракт; исполняемого resolver на этапе 0 ещё нет.
- `markdown.json`: два примера с ожидаемыми UTF-8 byte ranges (LF/CRLF),
  frontmatter, inline/fenced/indented code. Это oracle будущего parser.

Команды от корня репозитория:

```sh
work-run -- cargo test --locked --manifest-path tools/format-probe/Cargo.toml
work-run -- python3 tools/corpus.py --self-check
work-run -- python3 tools/corpus.py tests/corpus/small --check
work-run -- python3 tools/corpus.py tests/corpus/invalid --check
work-run -- python3 tools/corpus.py .generated/5000 --profile 5000
work-run -- python3 tools/corpus.py .generated/50000 --profile 50000
work-run -- python3 tools/corpus.py .generated/50000 --check
```

Генерация только в новый каталог; существующий, даже пустой, не перезаписывается.
Повторная проверка использует `--check`. Для повторной генерации выбрать
другое новое имя. Генератор детерминированный, seed и случайные процессы не нужны.
Большие корпуса содержат N опубликованных заметок, root без исходящих,
остальные с одной ссылкой на предыдущую: N−1 рёбер. Каждая заметка содержит
кириллицу, CJK, combining mark и emoji. Corpus manifest фиксирует SHA-256
байтов каждого файла; он не заметка. Приложение игнорирует его при обходе.

Большие деревья локальны и исключены из Git. Для сравнимых измерений записать
профиль/версию генератора, агрегатный хеш manifest, железо, ФС, Rust и build
profile; отдельно измерять обход/stat, parse, rebuild, reconcile, search,
capture, startup, idle CPU/RAM. На этапе 0 проверяется состав, не скорость
ещё не существующего приложения. Плотность/цепочка корпуса не моделирует
все свойства реального vault, реальные данные проверяются дополнительно.

Извлечение ссылок в v0.1 должно отдельно проверять byte ranges от начала
полного UTF-8 файла, CRLF/frontmatter offset и исключение всех видов code.
Не переписывать эти ожидания в генератор production resolver: это oracle,
не альтернативная реализация.
