#!/usr/bin/env python3
"""Реальные PTY-проверки v0.1. Запуск: python3 tools/test_tui.py [binary]."""
import errno
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time
import unicodedata

BINARY = str(Path(sys.argv[1] if len(sys.argv) > 1 else "target/debug/zetto").resolve())


def cli(vault, *args):
    p = subprocess.run([BINARY, "--vault", str(vault), "--format", "json", *args], capture_output=True)
    assert p.returncode == 0, (p.returncode, p.stdout, p.stderr)
    return json.loads(p.stdout)["data"]


def check_cli_editor(root, env):
    vault = root / "edit-vault"
    cli(vault, "init")
    note = cli(vault, "capture", "before", "--title", "CLI editor")
    editor = root / "printing-editor.py"
    editor.write_text("import pathlib,sys\np=pathlib.Path(sys.argv[1])\np.write_text(p.read_text() + '\\nCLI_EDITOR_BODY\\n')\nprint('\\x1b[31mCLI_EDITOR_MARKER\\x1b[0m', flush=True)\n")
    env = dict(env, VISUAL=f"{sys.executable} {editor}", EDITOR=f"{sys.executable} {editor}")
    for output_format in ("json", "jsonl"):
        master, slave = pty.openpty()
        process = None
        try:
            # stdout — отдельный TTY; stderr — pipe, не тот же экран.
            process = subprocess.Popen([BINARY, "--vault", str(vault), "--format", output_format, "edit", note["id"]], stdin=slave, stdout=slave, stderr=subprocess.PIPE, env=env)
            _, stderr = process.communicate(timeout=7)
            stdout = bytearray()
            while select.select([master], [], [], 0)[0]:
                stdout.extend(os.read(master, 65536))
            assert process.returncode == 0, (output_format, process.returncode, stdout, stderr)
            assert b"CLI_EDITOR_MARKER" not in stdout and b"\x1b" not in stdout, (output_format, stdout)
            assert b"\x1b[31mCLI_EDITOR_MARKER\x1b[0m" in stderr, stderr
            if output_format == "json":
                response = json.loads(stdout)
            else:
                records = [json.loads(line) for line in stdout.splitlines()]
                assert records[-1]["type"] == "summary", records
                response = records[-1]["data"]
            assert response["status"] == "ok" and response["saved"] is True, response
            assert "CLI_EDITOR_BODY" in cli(vault, "show", note["id"])["body"]
        finally:
            if process is not None and process.poll() is None:
                process.kill()
                process.communicate()
            os.close(master)
            os.close(slave)


class Pty:
    def __init__(self, vault, env):
        self.master, self.slave = pty.openpty()
        self.resize(100, 28)
        self.before = termios.tcgetattr(self.slave)
        self.output = bytearray()
        self.cursor_queries = 0
        def attach():
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)
        self.process = subprocess.Popen([BINARY, "--vault", str(vault), "--color", "never", "tui"], stdin=self.slave, stdout=self.slave, stderr=self.slave, env=env, preexec_fn=attach)
        self.wait("zetto / Library")

    def resize(self, columns, rows):
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, columns, 0, 0))
        if hasattr(self, "process"):
            os.kill(self.process.pid, signal.SIGWINCH)

    def send(self, data):
        os.write(self.master, data.encode() if isinstance(data, str) else data)

    def drain(self, timeout):
        if select.select([self.master], [], [], timeout)[0]:
            try:
                self.output.extend(os.read(self.master, 65536))
                queries = self.output.count(b"\x1b[6n")
                while self.cursor_queries < queries:
                    os.write(self.master, b"\x1b[1;1R")
                    self.cursor_queries += 1
            except OSError as e:
                if e.errno != errno.EIO:
                    raise

    def until(self, predicate, label):
        deadline = time.monotonic() + 7
        while not predicate():
            assert time.monotonic() < deadline, (label, bytes(self.output[-6000:]).decode(errors="replace"))
            assert self.process.poll() is None, (label, self.process.returncode, self.output.decode(errors="replace"))
            self.drain(0.05)

    def visible(self):
        # ratatui пишет дельты: маркер может состоять из старых и новых ячеек.
        cells = {}
        row = column = 0
        tokens = re.finditer(r"\x1b\[[0-?]*[ -/]*[@-~]|.", self.output.decode(errors="replace"), re.S)
        for match in tokens:
            token = match.group()
            if token.startswith("\x1b["):
                command = token[-1]
                args = token[2:-1]
                if command in "Hf":
                    parts = [int(p or 1) for p in args.split(";")]
                    row, column = parts[0] - 1, (parts[1] if len(parts) > 1 else 1) - 1
                elif command == "J" and args == "2":
                    cells.clear()
                continue
            if token == "\r":
                column = 0
            elif token == "\n":
                row += 1
            elif not unicodedata.category(token).startswith("C"):
                if unicodedata.combining(token):
                    continue
                cells[row, column] = token
                column += 2 if unicodedata.east_asian_width(token) in "WF" else 1
        return "\n".join("".join(cells.get((r, c), " ") for c in range(180)) for r in range(60))

    def wait(self, marker):
        wanted = "".join(marker.split())
        self.until(lambda: wanted in "".join(self.visible().split()), marker)

    def paste(self, text):
        self.send(b"\x1b[200~" + text.encode() + b"\x1b[201~")

    def finish(self, code):
        self.process.wait(timeout=7)
        self.drain(0)
        assert self.process.returncode == code, (code, self.process.returncode, self.output.decode(errors="replace"))
        assert termios.tcgetattr(self.slave) == self.before, "termios не восстановлен"
        assert b"\x1b[?2004l" in self.output and b"\x1b[?1049l" in self.output and b"\x1b[?25h" in self.output, "paste/alternate/cursor не восстановлены"
        os.close(self.master)
        os.close(self.slave)

    def abort(self):
        if self.process.poll() is None:
            self.process.kill()
            self.process.wait()
        os.close(self.master)
        os.close(self.slave)


def main():
    with tempfile.TemporaryDirectory(prefix="zetto-pty-") as temp:
        root = Path(temp)
        vault = root / "vault"
        cli(vault, "init")
        editor = root / "editor.py"
        editor.write_text("#!/usr/bin/env python3\nimport pathlib,sys\np=pathlib.Path(sys.argv[1])\np.write_text(p.read_text().replace('title: \"\"', 'title: \"PTY note\"') + '\\nEDITOR_BODY\\n')\nprint('EDITOR_OK', flush=True)\n")
        editor.chmod(0o700)
        env = dict(os.environ, EDITOR=str(editor), VISUAL=str(editor), XDG_CONFIG_HOME=str(root / "config"), TERM="xterm-256color", NO_COLOR="1")
        check_cli_editor(root, env)
        ui = Pty(vault, env)
        try:
            ui.send("c")
            ui.wait("Заголовок")
            text = "jknp? e\u0301 👨‍👩‍👧‍👦\n第二行\n"
            ui.paste(text)
            ui.wait("第二行")
            ui.resize(65, 20)
            ui.send(b"\x13")
            ui.wait("Сохранено")
            drafts = list((vault / "inbox").glob("*.md"))
            assert len(drafts) == 1 and text in drafts[0].read_text()
            note_id = drafts[0].name[:26]
            ui.resize(100, 28)
            ui.send("e")
            ui.wait("EDITOR_OK")
            ui.until(lambda: "EDITOR_BODY" in drafts[0].read_text(), "результат редактора сохранён ядром")
            assert "EDITOR_BODY" in cli(vault, "show", note_id)["body"]
            ui.send("l")
            ui.wait("Связь:")
            ui.send("\r")
            ui.until(lambda: "[[" in drafts[0].read_text(), "связь сохранена")
            ui.send("p")
            ui.until(lambda: any(vault.glob(note_id + "-*.md")), "публикация сохранена")
            note = cli(vault, "show", note_id)
            assert note["stage"] == "published" and "[[" in note["body"]
            # CLI пишет при открытом UI; refresh сохраняет выбор и читает новые файлы.
            cli(vault, "capture", "external", "--title", "CLI concurrent")
            ui.send("r")
            ui.wait("Файлы сверены")
            while select.select([ui.master], [], [], 0.05)[0]:
                ui.drain(0)
            assert not select.select([ui.master], [], [], 0.2)[0], "idle TUI постоянно перерисовывается"
            ui.send("i")
            ui.wait("CLI concurrent")
            # Resize в форме, Esc default Continue, явный discard не создаёт заметку.
            ui.send("c")
            ui.wait("Заголовок")
            ui.paste("resize input")
            ui.wait("resize input")
            ui.resize(55, 10)
            ui.wait("Нужно")
            ui.resize(100, 28)
            ui.wait("resize input")
            ui.send(b"\x1b")
            ui.wait("Несохранённый ввод")
            ui.send("\r")
            ui.send(b"\x1b")
            ui.wait("Несохранённый ввод")
            ui.send("d")
            ui.send("q")
            ui.finish(0)
        except BaseException:
            ui.abort()
            raise
        # New сначала сохраняет draft; отказ публикации оставляет его выбранным в Inbox.
        ui = Pty(vault, env)
        try:
            ui.send("n")
            ui.wait("Заголовок")
            ui.send("\r")
            ui.paste("new rejected body")
            ui.wait("new rejected body")
            ui.send(b"\x13")
            ui.wait("Связь:")
            ui.send(b"\x1b")
            ui.wait("zetto/empty-title")
            ui.wait("zetto/link-before-publish")
            ui.wait("Inbox")
            draft = next(p for p in (vault / "inbox").glob("*.md") if "new rejected body" in p.read_text())
            ui.wait(draft.name[:26])
            ui.send("q")
            ui.finish(0)
        except BaseException:
            ui.abort()
            raise
        # Ошибка I/O при SIGINT сохраняет форму, снимает flag и позволяет повторить запись.
        ui = Pty(vault, env)
        try:
            ui.send("c")
            ui.wait("Заголовок")
            ui.paste("retry accepted")
            ui.wait("retry accepted")
            config = vault / ".zettorc"
            hidden = vault / ".zettorc.test-backup"
            config.rename(hidden)
            try:
                os.kill(ui.process.pid, signal.SIGINT)
                ui.wait("Текст НЕ подтверждён")
            finally:
                hidden.rename(config)
            ui.send("x")
            ui.wait("retry acceptedx")
            ui.send(b"\x13")
            ui.until(lambda: any("retry acceptedx" in p.read_text() for p in (vault / "inbox").glob("*.md")), "повтор capture после I/O")
            ui.send("q")
            ui.finish(0)
        except BaseException:
            ui.abort()
            raise
        # Ошибка редактора: терминал возобновлён, recovery указан, q работает.
        bad = dict(env, VISUAL="/bin/false", EDITOR="/bin/false")
        ui = Pty(vault, bad)
        try:
            ui.send("e")
            ui.wait("editor_failed")
            ui.wait("Восстановление:")
            ui.send("q")
            ui.finish(0)
        except BaseException:
            ui.abort()
            raise
        # Ctrl-C key и OS SIGINT сохраняют принятый многострочный ввод.
        for os_signal in (False, True):
            ui = Pty(vault, env)
            try:
                ui.send("c")
                ui.wait("Заголовок")
                body = "signal " + str(os_signal) + "\nСохранить 🌍"
                ui.paste(body)
                ui.wait("Сохранить")
                if os_signal:
                    os.kill(ui.process.pid, signal.SIGINT)
                else:
                    ui.send(b"\x03")
                ui.finish(130)
                tail = bytes(ui.output).rsplit(b"\x1b[?1049l", 1)[-1]
                assert b"inbox/" in tail and b"interrupted" in tail, "результат SIGINT потерял ID/путь capture"
                assert any(body in p.read_text() for p in (vault / "inbox").glob("*.md"))
            except BaseException:
                ui.abort()
                raise
        for args in (["tui"], ["--no-interactive", "tui"], ["--format", "json", "tui"]):
            p = subprocess.run([BINARY, "--vault", str(vault), *args], input=b"", capture_output=True)
            assert p.returncode == 2 and b"interaction_required" in p.stdout + p.stderr
            assert b"\x1b[?1049h" not in p.stdout
    print("PTY: CLI edit JSON/JSONL; capture → editor → link → publish; new/rejected publish; concurrent CLI/refresh; Unicode/resize; idle; IO retry; editor failure; q/Ctrl-C/SIGINT; termios — OK")


if __name__ == "__main__":
    main()
