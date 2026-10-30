#!/usr/bin/env python3
"""Реальные PTY-проверки v0.1/v0.2. Запуск: python3 tools/test_tui.py [binary]."""
import errno
import faulthandler
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

# Session leader остаётся жив для проверки termios: macOS отзывает slave при его выходе.
PTY_RUNNER = """
import signal,subprocess,sys,termios
before = termios.tcgetattr(0)
child = subprocess.Popen(sys.argv[1:])
for signum in (signal.SIGINT, signal.SIGWINCH, signal.SIGTERM):
    signal.signal(signum, lambda signum, frame: child.send_signal(signum))
code = child.wait()
assert termios.tcgetattr(0) == before, "termios не восстановлен"
sys.exit(code if code >= 0 else 128 - code)
"""


def attach_pty():
    os.setsid()
    fcntl.ioctl(0, termios.TIOCSCTTY, 0)


def drain_pty(master, timeout=0):
    os.set_blocking(master, False)
    deadline = time.monotonic() + 2
    while select.select([master], [], [], timeout)[0]:
        assert time.monotonic() < deadline, "PTY drain timeout"
        try:
            chunk = os.read(master, 65536)
        except BlockingIOError:
            break
        except OSError as error:
            if error.errno == errno.EIO:
                break
            raise
        # EOF может оставаться готовым для select, в том числе после выхода child.
        if not chunk:
            break
        yield chunk


def wait_pty_exit(process, drain):
    # macOS может ждать чтения PTY даже в proc_exit; wait до drain блокирует обоих.
    deadline = time.monotonic() + 7
    while process.poll() is None:
        if time.monotonic() >= deadline:
            raise subprocess.TimeoutExpired(process.args, 7)
        drain(0.05)
    drain(0)


def check_drain_eof():
    reader, writer = os.pipe()
    os.write(writer, b"payload")
    os.close(writer)
    try:
        assert list(drain_pty(reader)) == [b"payload"]
    finally:
        os.close(reader)
    master, slave = pty.openpty()
    os.close(slave)
    try:
        assert list(drain_pty(master)) == []
    finally:
        os.close(master)


def check_pty_exit():
    for script, expected in (("import sys; sys.stdout.buffer.write(b'x' * 131072); sys.stdout.flush()", 0), ("import tty; tty.setraw(0)", 1)):
        master, slave = pty.openpty()
        process = subprocess.Popen([sys.executable, "-c", PTY_RUNNER, sys.executable, "-c", script], stdin=slave, stdout=slave, stderr=slave, preexec_fn=attach_pty)
        output = bytearray()
        try:
            wait_pty_exit(process, lambda timeout: output.extend(b"".join(drain_pty(master, timeout))))
            assert process.returncode == expected, (process.returncode, output)
            if expected == 0:
                assert output == b"x" * 131072, len(output)
            else:
                assert "termios не восстановлен" in output.decode(), output
        finally:
            if process.poll() is None:
                process.kill()
            os.close(master)
            os.close(slave)
            process.wait(timeout=7)


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
            stdout = b"".join(drain_pty(master))
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
        self.output = bytearray()
        self.cursor_queries = 0
        self.process = subprocess.Popen([sys.executable, "-c", PTY_RUNNER, BINARY, "--vault", str(vault), "--color", "never", "tui"], stdin=self.slave, stdout=self.slave, stderr=self.slave, env=env, preexec_fn=attach_pty)
        self.wait("zetto / Library")

    def resize(self, columns, rows):
        fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, columns, 0, 0))
        if hasattr(self, "process"):
            os.kill(self.process.pid, signal.SIGWINCH)

    def send(self, data):
        os.write(self.master, data.encode() if isinstance(data, str) else data)

    def drain(self, timeout):
        for chunk in drain_pty(self.master, timeout):
            self.output.extend(chunk)
            queries = self.output.count(b"\x1b[6n")
            while self.cursor_queries < queries:
                os.write(self.master, b"\x1b[1;1R")
                self.cursor_queries += 1

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
                width = 2 if unicodedata.east_asian_width(token) in "WF" else 1
                if width == 2:
                    cells[row, column + 1] = ""  # Wide glyph заменяет обе прежние ячейки.
                column += width
        return "\n".join("".join(cells.get((r, c), " ") for c in range(180)) for r in range(60))

    def wait(self, marker):
        wanted = "".join(marker.split())
        self.until(lambda: wanted in "".join(self.visible().split()), marker)

    def wait_ready(self):
        self.until(lambda: not any(marker in self.visible() for marker in ("Сверка…", "Загрузка предпросмотра…")), "каталог и предпросмотр готовы")

    def picker_ready(self, query, target):
        lines = self.visible().splitlines()
        header = "".join(lines[1].split())
        return (f"Связь:/{query}[ВВОД]·опубликованнаязаметка" in header
                and lines[2].lstrip("│ ").startswith(">") and target in lines[2])

    def wait_saved(self, identity, path):
        wanted = "".join(f"Сохранено {identity} {path}".split())
        def confirmed():
            screen = self.visible()
            return (wanted in "".join(screen.split()) and "Связь:" not in screen
                    and "Сохранение…" not in screen)
        self.until(confirmed, "запись подтверждена после освобождения vault-lock")

    def paste(self, text):
        self.send(b"\x1b[200~" + text.encode() + b"\x1b[201~")

    def finish(self, code):
        wait_pty_exit(self.process, self.drain)
        assert self.process.returncode == code, (code, self.process.returncode, self.output.decode(errors="replace"))
        assert b"\x1b[?2004l" in self.output and b"\x1b[?1049l" in self.output and b"\x1b[?25h" in self.output, "paste/alternate/cursor не восстановлены"
        os.close(self.master)
        os.close(self.slave)

    def abort(self):
        if self.process.poll() is None:
            self.process.terminate()
        try:
            wait_pty_exit(self.process, self.drain)
        finally:
            os.close(self.master)
            os.close(self.slave)


def check_picker_ready():
    ui = Pty.__new__(Pty)
    old = "> Корень · root-id\n日本語 заметка · target-id"
    for header, candidates, expected in (
        ("Связь: /  · опубликованная заметка", old, False),
        ("Связь: / 日本語 [ВВОД] · Поиск…", old, False),
        ("Связь: / другое [ВВОД] · опубликованная заметка", "> 日本語 · target-id", False),
        ("Связь: / 日本語 [ВВОД] · опубликованная заметка", "> 日本語 · target-id", True),
    ):
        ui.output = bytearray(("\x1b[2J\x1b[2;1H" + header + "\x1b[3;1H" + candidates).encode())
        assert ui.picker_ready("日本語", "target-id") is expected, (header, ui.visible().splitlines()[:4])


def check_cached_startup(root, env):
    vault = root / "cached-startup"
    note = cli(vault, "init")
    cli(vault, "list")
    path = vault / note["path"]
    changed = path.read_text().replace("Корень", "Свежий старт")
    path.write_text(changed)
    ui = Pty(vault, env)
    try:
        # Событие произошло до установки watcher: cached должен смениться stat-результатом.
        ui.wait("Свежий старт")
        ui.send("q")
        ui.finish(0)
        assert path.read_text() == changed, "Startup-сверка изменила заметку"
    except BaseException:
        ui.abort()
        raise


def check_worker_ui(root, env):
    vault = root / "worker-vault"
    root_id = cli(vault, "init")["id"]
    first = cli(vault, "capture", "firsttoken full text", "--title", "Первый результат")
    second = cli(vault, "capture", "secondtoken full text", "--title", "日本語 заметка")
    for note in (first, second):
        cli(vault, "publish", note["id"], "--link", root_id)
    ui = Pty(vault, env)
    try:
        ui.send("/")
        ui.paste("firsttoken")
        ui.wait("firsttoken full text")
        # Быстрая замена создаёт несколько поколений запроса: принимает только последнее.
        ui.send(b"\x7f" * len("firsttoken"))
        ui.paste("secondtoken")
        ui.wait("secondtoken full text")
        assert "Первый результат" not in ui.visible(), ui.visible()
        ui.send("jknp")
        ui.wait("Нет совпадений")
        ui.send(b"\x1b")
        ui.until(lambda: "[ВВОД]" not in ui.visible(), "Esc закрыл поиск")
        # Навигация/resize работают одновременно с запросом фоновой full-сверки.
        ui.send("rj")
        ui.wait("> Первый результат")
        ui.resize(90, 26)
        ui.wait("> Первый результат")
        ui.wait(first["id"])
        ui.send("i")
        draft = cli(vault, "capture", "external worker body", "--title", "Watcher before")
        ui.wait("Watcher before")  # Без ручного r.
        ui.wait(draft["id"])
        ui.send("l")
        ui.wait("Связь:")
        ui.send("/")
        ui.paste("日本語")
        ui.send("\r")  # Enter при pending не выбирает старый первый элемент.
        ui.until(lambda: "[[" in (vault / draft["path"]).read_text() or ui.picker_ready("日本語", second["id"]), "актуальный Unicode fuzzy")
        if "Поисксвязи…дождитесьактуальногорезультата" in "".join(ui.visible().split()):
            assert "[[" not in (vault / draft["path"]).read_text(), "отвергнутый ранний Enter создал ссылку"
        if "[[" not in (vault / draft["path"]).read_text():
            ui.send("\r")
        ui.until(lambda: second["id"] in (vault / draft["path"]).read_text(), "fuzzy выбрал японский title")
        assert root_id not in (vault / draft["path"]).read_text(), "выбрана устаревшая fuzzy выдача"
        ui.wait_saved(draft["id"], draft["path"])
        # Отдельный CLI edit меняет title, затем CLI publish переносит выбранный файл.
        editor = root / "watch-editor.py"
        editor.write_text("import pathlib,sys\np=pathlib.Path(sys.argv[1])\np.write_text(p.read_text().replace('Watcher before', 'Watcher after'))\n")
        edit_env = dict(env, VISUAL=f"{sys.executable} {editor}", EDITOR=f"{sys.executable} {editor}")
        master, slave = pty.openpty()
        process = None
        try:
            process = subprocess.Popen([BINARY, "--vault", str(vault), "--format", "json", "edit", draft["id"]], stdin=slave, stdout=slave, stderr=subprocess.PIPE, env=edit_env)
            _, stderr = process.communicate(timeout=7)
            response = b"".join(drain_pty(master))
            assert process.returncode == 0, (response, stderr)
            assert json.loads(response)["saved"] is True
        finally:
            if process is not None and process.poll() is None:
                process.kill()
                process.communicate()
            os.close(master)
            os.close(slave)
        ui.wait("Watcher after")
        published = cli(vault, "publish", draft["id"])
        ui.wait("Нет заметок")
        ui.send("i/")
        ui.paste("Watcher after")
        ui.wait("Watcher after")
        ui.wait(draft["id"])
        ui.wait(published["path"])
        assert "inbox/" not in ui.visible().splitlines()[4], "внешний rename оставил старый preview path"
        ui.send(b"\x1b")
        ui.until(lambda: "[ВВОД]" not in ui.visible(), "Esc вернул библиотеку")
        ui.send("r")
        ui.wait("Файлы сверены")
        ui.wait_ready()
        # После завершения обоих запросов неизменные данные не рисуют кадры.
        ui.drain(0.1)
        idle_until = time.monotonic() + 5.7
        while time.monotonic() < idle_until:
            assert ui.process.poll() is None
            assert not select.select([ui.master], [], [], 0.1)[0], "idle reconciliation перерисовывает неизменный экран"
        ui.send("q")
        ui.finish(0)
    except BaseException:
        ui.abort()
        raise


def check_editor_worker_owns_tty(root, env):
    vault = root / "editor-worker-vault"
    cli(vault, "init")
    started = root / "editor-started"
    editor = root / "waiting-editor.py"
    editor.write_text("import pathlib,sys\np=pathlib.Path(sys.argv[1])\npathlib.Path(" + repr(str(started)) + ").write_text('ready')\nline=sys.stdin.readline()\np.write_text(p.read_text() + '\\n' + line)\nprint('EDITOR_INPUT_OK', flush=True)\n")
    edit_env = dict(env, VISUAL=f"{sys.executable} {editor}", EDITOR=f"{sys.executable} {editor}")
    ui = Pty(vault, edit_env)
    try:
        ui.send("e")
        ui.until(started.exists, "редактор получил TTY")
        ui.drain(0.1)
        before = len(ui.output)
        cli(vault, "capture", "CLI while editor waits", "--title", "Concurrent editor")
        ui.drain(0.3)
        assert len(ui.output) == before, "watcher перерисовал TUI поверх редактора"
        ui.send("EDITOR_STDIN_UNTOUCHED\n")
        ui.wait("EDITOR_STDIN_UNTOUCHED")
        ui.until(lambda: b"EDITOR_INPUT_OK" in ui.output, "stdin прочитан редактором")
        ui.wait("Сохранено")
        ui.send("q")
        ui.finish(0)
    except BaseException:
        ui.abort()
        raise


def main():
    check_drain_eof()
    check_pty_exit()
    check_picker_ready()
    with tempfile.TemporaryDirectory(prefix="zetto-pty-") as temp:
        root = Path(temp)
        vault = root / "vault"
        cli(vault, "init")
        editor = root / "editor.py"
        editor.write_text("#!/usr/bin/env python3\nimport pathlib,sys\np=pathlib.Path(sys.argv[1])\np.write_text(p.read_text().replace('title: \"\"', 'title: \"PTY note\"') + '\\nEDITOR_BODY\\n')\nprint('EDITOR_OK', flush=True)\n")
        editor.chmod(0o700)
        env = dict(os.environ, EDITOR=str(editor), VISUAL=str(editor), XDG_CONFIG_HOME=str(root / "config"), TERM="xterm-256color", NO_COLOR="1")
        check_cli_editor(root, env)
        check_cached_startup(root, env)
        check_worker_ui(root, env)
        check_editor_worker_owns_tty(root, env)
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
            ui.until(lambda: b"EDITOR_OK" in ui.output, "вывод внешнего редактора получен")
            ui.until(lambda: "EDITOR_BODY" in drafts[0].read_text(), "результат редактора сохранён ядром")
            assert "EDITOR_BODY" in cli(vault, "show", note_id)["body"]
            ui.wait("PTY note")
            ui.send("l")
            ui.wait("Связь:")
            ui.send("\r")
            ui.until(lambda: "[[" in drafts[0].read_text(), "связь сохранена")
            ui.wait_saved(note_id, drafts[0].relative_to(vault).as_posix())
            ui.send("p")
            ui.until(lambda: any(vault.glob(note_id + "-*.md")), "публикация сохранена")
            ui.wait_saved(note_id, next(vault.glob(note_id + "-*.md")).name)
            note = cli(vault, "show", note_id)
            assert note["stage"] == "published" and "[[" in note["body"]
            # CLI пишет при открытом UI; refresh сохраняет выбор и читает новые файлы.
            cli(vault, "capture", "external", "--title", "CLI concurrent")
            ui.send("r")
            ui.wait("Файлы сверены")
            ui.wait_ready()
            ui.drain(0.05)
            assert ui.process.poll() is None, "TUI завершился до проверки простоя"
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
    faulthandler.dump_traceback_later(60, repeat=True)
    main()
