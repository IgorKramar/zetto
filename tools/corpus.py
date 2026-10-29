#!/usr/bin/env python3
"""Детерминированные корпуса этапа 0. Только новый каталог, без перезаписи."""

import argparse
import hashlib
import json
from pathlib import Path
import tempfile

ALPHABET = "0123456789ABCDEFGHJKMNPQRSTVWXYZ"


def note_id(number):
    value = (1469918176385 << 80) + number
    return "".join(ALPHABET[(value >> shift) & 31] for shift in range(125, -1, -5))


def generate(destination, profile):
    destination.mkdir(parents=True, exist_ok=False)
    (destination / "inbox").mkdir()
    (destination / "assets").mkdir()
    count = {"small": 8, "invalid": 9, "5000": 5000, "50000": 50000}[profile]
    records = []
    for number in range(count):
        identity = note_id(number)
        title = "Корень" if number == 0 else f"Заметка {number}"
        slug = "koren" if number == 0 else f"zametka-{number}"
        draft = profile == "small" and number >= 6
        path = f"{identity}-{slug}.md"
        if draft:
            path = "inbox/" + path
        header = f'id: {identity}\ntitle: {json.dumps(title, ensure_ascii=False)}\n'
        body = f"# {title}\n\nКириллица, café, 東京, e\u0301, 🦀.\n"
        targets = [] if number == 0 else [note_id(number - 1)]
        diagnostics = []
        if profile == "small":
            if number in (1, 2):
                header += 'aliases: ["Общее"]\ntags: [knowledge]\n'
            if number == 3:
                body += f"\n`[[{note_id(0)}]]`\n\n```md\n[[{note_id(1)}]]\n```\n"
            if number in (4, 6):
                targets = []
            if number == 4:
                diagnostics.append("zetto/link-before-publish")
            if number == 5:
                body += "\n[[Общее]]\n[[Нет такой заметки]]\n"
                diagnostics += ["zetto/ambiguous-link", "zetto/no-broken-link"]
            if number == 6:
                header = f'id: {identity}\ntitle: ""\n'
                path = f"inbox/{identity}.md"
                diagnostics.append("zetto/empty-title")
        if profile == "invalid":
            targets = []
            if number == 0:
                header = "title: Missing ID\n"
                diagnostics = ["zetto/missing-required-field"]
            elif number == 1:
                header = f"id: {identity}\n"
                diagnostics = ["zetto/missing-required-field"]
            elif number == 2:
                header = f"id: {identity}\ntitle: [unclosed\n"
                diagnostics = ["zetto/invalid-yaml"]
            elif number == 3:
                header = f"id: {identity}\ntitle: One\ntitle: Two\n"
                diagnostics = ["zetto/duplicate-frontmatter-key"]
            elif number in (4, 5):
                header = f'id: {note_id(4)}\ntitle: "Дубликат"\n'
                diagnostics = ["zetto/duplicate-id"]
                if number == 5:
                    diagnostics.append("zetto/id-filename-mismatch")
            elif number == 6:
                header += "tags: {bad: type}\ncreated: yesterday\nx-keep: [1, null, true]\n"
                diagnostics = ["zetto/invalid-tags-format", "zetto/non-rfc3339-timestamp"]
            elif number == 7:
                header = "id: ZZZZZZZZZZZZZZZZZZZZZZZZZZ\ntitle: Overflow\n"
                diagnostics = ["zetto/invalid-id-format", "zetto/id-filename-mismatch"]
            else:
                body += "\n<<<<<<< HEAD\nA\n=======\nB\n>>>>>>> branch\n"
                diagnostics = ["zetto/merge-conflict"]
        body += "".join(f"\n[[{target}]]\n" for target in targets)
        content = f"---\n{header}---\n{body}".encode()
        (destination / path).write_bytes(content)
        records.append({"path": path, "sha256": hashlib.sha256(content).hexdigest(),
                        "edges": targets, "diagnostics": diagnostics})
    config = f'config_version = 1\nformat_version = 1\nroot_id = "{note_id(0)}"\n'
    (destination / ".zettorc").write_text(config, encoding="utf-8")
    manifest = {"generator_version": 1, "profile": profile, "notes": count,
                "resolved_edges": sum(len(r["edges"]) for r in records), "files": records}
    (destination / "manifest.json").write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    return manifest


def check(destination):
    manifest = json.loads((destination / "manifest.json").read_text(encoding="utf-8"))
    actual = set(destination.glob("*.md")) | set((destination / "inbox").glob("*.md"))
    expected = {destination / record["path"] for record in manifest["files"]}
    assert actual == expected and len(actual) == manifest["notes"], "состав корпуса изменился"
    for record in manifest["files"]:
        relative = Path(record["path"])
        assert not relative.is_absolute() and ".." not in relative.parts, "опасный путь"
        path = destination / relative
        assert not path.is_symlink(), "символическая ссылка вместо заметки"
        assert hashlib.sha256(path.read_bytes()).hexdigest() == record["sha256"], path
    assert sum(len(r["edges"]) for r in manifest["files"]) == manifest["resolved_edges"]
    return manifest


def self_check():
    with tempfile.TemporaryDirectory() as directory:
        path = Path(directory) / "vault"
        generated = generate(path, "small")
        assert check(path) == generated
        fixtures = Path(__file__).resolve().parents[1] / "tests" / "corpus"
        for profile in ["small", "invalid"]:
            fresh = Path(directory) / profile
            generate(fresh, profile)
            for name in ["manifest.json", ".zettorc"]:
                assert (fresh / name).read_bytes() == (fixtures / profile / name).read_bytes()
        try:
            generate(path, "small")
        except FileExistsError:
            pass
        else:
            raise AssertionError("перезапись существующего каталога разрешена")
        (path / generated["files"][0]["path"]).write_text("changed", encoding="utf-8")
        try:
            check(path)
        except AssertionError:
            pass
        else:
            raise AssertionError("порча корпуса не обнаружена")
        for case in json.loads((fixtures / "markdown.json").read_text(encoding="utf-8")):
            content = case["text"].encode()
            for link in case["links"]:
                assert content[link["start"]:link["end"]].decode() == link["raw"]


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("destination", nargs="?", type=Path)
    parser.add_argument("--profile", choices=["small", "invalid", "5000", "50000"], default="small")
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--self-check", action="store_true")
    args = parser.parse_args()
    if args.self_check:
        self_check()
        print("Самопроверка: OK")
    elif args.destination is None:
        parser.error("нужен новый каталог назначения")
    else:
        result = check(args.destination) if args.check else generate(args.destination, args.profile)
        print(f'{result["profile"]}: {result["notes"]} заметок, {result["resolved_edges"]} рёбер')
