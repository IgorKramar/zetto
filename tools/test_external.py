#!/usr/bin/env python3
"""Реальные параллельные CLI, занятая SQLite и смена git-ветки в временном vault."""
import hashlib
import json
import os
from pathlib import Path
import re
import sqlite3
import subprocess
import sys
import tempfile
import time

BINARY = str(Path(sys.argv[1] if len(sys.argv) > 1 else "target/debug/zetto").resolve())


def cli(vault, env, *args, expected=0):
    process = subprocess.run([BINARY, "--vault", str(vault), "--format=json", *args],
                             env=env, capture_output=True, timeout=10)
    assert process.returncode == expected, (args, process.returncode, process.stdout, process.stderr)
    return json.loads(process.stdout)


def parallel(vault, env):
    cli(vault, env, "init")
    cli(vault, env, "index", "rebuild")
    bodies = ["Первый процесс 🌍", "Второй процесс 🌍"]
    children = [subprocess.Popen([BINARY, "--vault", str(vault), "--format=json", "capture", "--stdin"],
                                env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                for _ in bodies]
    try:
        time.sleep(0.05)
        assert all(child.poll() is None for child in children), "Оба процесса должны ждать stdin"
        for child, body in zip(children, bodies):
            child.stdin.write(body.encode())
            child.stdin.close()
            child.stdin = None
        saved = []
        for child, body in zip(children, bodies):
            stdout, stderr = child.communicate(timeout=10)
            result = json.loads(stdout)
            if child.returncode == 3:
                assert result["error"]["code"] == "lock_busy" and not result["saved"]
                result = cli(vault, env, "capture", body)
            else:
                assert child.returncode == 0, (stdout, stderr)
            assert result["saved"]
            identity = result["data"]["id"]
            assert cli(vault, env, "show", identity)["data"]["body"] == body
            saved.append(identity)
        assert len(set(saved)) == 2
        root = cli(vault, env, "list")["data"]["items"][0]["id"]
        for identity in saved:
            cli(vault, env, "link", identity, root)
        before = cli(vault, env, "links", root)["data"]["items"]
        cache = Path(env["XDG_CACHE_HOME"]) / "zetto" / hashlib.sha256(os.fsencode(vault.resolve())).hexdigest() / "index.sqlite3"
        inode = cache.stat().st_ino
        with sqlite3.connect(cache) as database:
            database.execute("BEGIN IMMEDIATE")
            blocked = cli(vault, env, "index", "rebuild", expected=4)
            assert blocked["error"]["code"] == "index_unavailable"
            assert cache.stat().st_ino == inode, "Rebuild не должен удалять занятую базу"
            database.rollback()
        cli(vault, env, "index", "rebuild")
        assert cli(vault, env, "links", root)["data"]["items"] == before
        assert cache.stat().st_ino == inode
        assert len(cli(vault, env, "list", "--stage=all")["data"]["items"]) == 3
    finally:
        for child in children:
            if child.poll() is None:
                child.kill()
                child.communicate()


def branch_switch(vault, env):
    root = cli(vault, env, "init")["data"]
    target = cli(vault, env, "new", "Исходное тело", "--title", "Первая", "--link", root["id"])["data"]
    source = cli(vault, env, "capture", "[[ветка два]]", "--title", "Источник")["data"]
    hooks = vault.parent / "empty-hooks"
    hooks.mkdir()
    git_env = dict(env, GIT_AUTHOR_NAME="Igor Kramar", GIT_AUTHOR_EMAIL="i@ikramar.ru",
                   GIT_COMMITTER_NAME="Igor Kramar", GIT_COMMITTER_EMAIL="i@ikramar.ru")

    def git(*args):
        subprocess.run(["git", "-C", str(vault), "-c", f"core.hooksPath={hooks}",
                        "-c", "core.attributesFile=/dev/null", "-c", "commit.gpgsign=false", *args],
                       env=git_env, capture_output=True, check=True, timeout=10)

    def commit(message):
        assert not re.search(r"claude|codex|anthropic|openai", message, re.I)
        git("commit", "-qm", message)

    git("init", "-q", "--initial-branch=one")
    git("add", "--", ".zettorc", root["path"], target["path"], source["path"])
    commit("Исходное дерево")
    assert cli(vault, env, "links", source["id"])["data"]["items"][0]["reason"] == "broken"
    git("checkout", "-qb", "two")
    new_path = target["id"] + "-vtoraya.md"
    (vault / new_path).write_text(
        f'---\nid: {target["id"]}\ntitle: Вторая\naliases: ["ветка два"]\n---\n'
        f'Другая версия\n[[{root["id"]}]]\n', encoding="utf-8")
    (vault / target["path"]).unlink()
    git("add", "--", target["path"], new_path)
    commit("Другая ветка")
    assert cli(vault, env, "show", target["id"])["data"]["path"] == new_path
    assert cli(vault, env, "links", source["id"])["data"]["items"][0]["resolved_id"] == target["id"]
    assert len(cli(vault, env, "search", "Другая")["data"]["items"]) == 1
    git("checkout", "-q", "one")
    assert cli(vault, env, "show", target["id"])["data"]["path"] == target["path"]
    assert cli(vault, env, "links", source["id"])["data"]["items"][0]["reason"] == "broken"
    assert cli(vault, env, "search", "Другая")["data"]["items"] == []


if __name__ == "__main__":
    with tempfile.TemporaryDirectory(prefix="zetto-external-") as directory:
        path = Path(directory).resolve()
        env = dict(os.environ, XDG_CACHE_HOME=str(path / "cache"))
        parallel(path / "parallel", env)
        branch_switch(path / "branches", env)
    print("CLI: два процесса, сохранённые тексты, busy/rebuild; git: rename, aliases, FTS, обратное переключение — OK")
