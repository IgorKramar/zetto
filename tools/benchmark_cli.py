#!/usr/bin/env python3
"""Замеры CLI на новом синтетическом дереве; исходные заметки не затрагиваются."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import platform
import resource
import shutil
import statistics
import subprocess
import sys
import time

import corpus


def measure(binary, vault, arguments, env, count):
    values = []
    result = None
    for _ in range(count):
        started = time.perf_counter()
        process = subprocess.run(
            [str(binary), "--vault", str(vault), "--format=json", *arguments],
            env=env, capture_output=True, timeout=120, check=True,
        )
        values.append(round((time.perf_counter() - started) * 1000, 3))
        result = json.loads(process.stdout)
        assert result["error"] is None
    return {"ms": values, "p50_ms": statistics.median(values),
            "mode": result["freshness"]["mode"]}, result


def filesystem(path):
    if not shutil.which("findmnt"):
        return None
    result = subprocess.run(["findmnt", "-no", "FSTYPE", "--target", str(path)],
                            capture_output=True, text=True, check=False)
    return result.stdout.strip() if result.returncode == 0 else None


def benchmark(binary, destination, profiles, samples):
    destination.mkdir(mode=0o700, parents=True, exist_ok=False)
    env = dict(os.environ, XDG_CACHE_HOME=str(destination / "cache"))
    report = {"benchmark_version": 1, "workload": "4k-3to10links",
              "started_at": datetime.now(timezone.utc).isoformat(),
              "platform": platform.platform(), "filesystem": filesystem(destination),
              "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
              "binary_version": subprocess.check_output([str(binary), "--version"], text=True).strip(),
              "cache": "прогретый файловый кэш ОС; новый SQLite при первом rebuild",
              "measurements": {}}
    cases = {
        "list_stat": ["list", "--limit=1"],
        "list_cached": ["--cached", "list", "--limit=1"],
        "search_stat": ["search", "Кириллица", "--limit=1"],
        "search_cached": ["--cached", "search", "Кириллица", "--limit=1"],
        "list_fresh": ["--fresh", "list", "--limit=1"],
    }
    for profile in profiles:
        vault = destination / profile
        manifest = corpus.generate(vault, profile, performance=True)
        rebuilt, outcome = measure(binary, vault, ["index", "rebuild"], env, 1)
        data = {key: manifest[key] for key in
                ("generator_version", "notes", "resolved_edges", "median_body_bytes")}
        data.update(manifest_sha256=hashlib.sha256((vault / "manifest.json").read_bytes()).hexdigest(),
                    rebuild_ms=rebuilt["p50_ms"], rebuild_metrics=outcome["data"]["metrics"])
        assert outcome["data"]["files"] == manifest["notes"]
        for name, arguments in cases.items():
            data[name], _ = measure(binary, vault, arguments, env, samples)
        peak = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss
        data["max_child_peak_rss_kib_so_far"] = peak / 1024 if sys.platform == "darwin" else peak
        corpus.check(vault)
        if profile == "5000":
            capture = destination / "capture-5000"
            shutil.copytree(vault, capture)
            data["capture"], _ = measure(binary, capture, ["capture", "измерение сохранения"], env, samples)
        report["measurements"][profile] = data
        (destination / "measurements.json").write_text(
            json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        print(f"{profile}: измерения сохранены", file=sys.stderr, flush=True)
    return report


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("destination", type=Path, help="Новый каталог для данных и отчёта")
    parser.add_argument("--binary", type=Path, default=Path("target/release/zetto"))
    parser.add_argument("--profile", choices=["5000", "50000"], action="append")
    parser.add_argument("--samples", type=int, default=5)
    args = parser.parse_args()
    if args.samples < 1:
        parser.error("--samples должен быть положительным")
    binary = args.binary.resolve(strict=True)
    report = benchmark(binary, args.destination.resolve(),
                       list(dict.fromkeys(args.profile or ["5000", "50000"])), args.samples)
    print(json.dumps(report, ensure_ascii=False, indent=2))
