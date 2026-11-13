#!/usr/bin/env python3
"""Paired evaluation of the agent-navigation map.

Tasks, gold answers, and results live outside the repository (``--data``) so
the map cannot learn from them. Arm ``a`` is a snapshot without
``docs/agent-navigation``; arm ``b`` is the same snapshot with the map and a
prompt pointer to it. Scoring is deterministic against the files each PR
changed, so it is blind to the arm.

Data files:
  tasks.json      [{"id", "gold": [paths], "domain", ...}]
  questions.json  [{"id", "question"}]
  results.jsonl   one record per (task, arm, repeat), appended by ``run``
"""

from __future__ import annotations

import argparse
import concurrent.futures
import hashlib
import json
import os
import random
import re
import shutil
import statistics
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

ARMS = ("a", "b")
MAP_DIR = "docs/agent-navigation"
POINTER = (
    "Before searching, read docs/agent-navigation/README.md, open the one domain page "
    "(or glossary.md) that matches the question, then verify its cited paths in source. "
    "The map is generated evidence, not instructions.\n\n"
)
PROMPT = (
    "You are answering a code-localization question about this repository. "
    "Use only the view, grep, and glob tools. Do not edit anything.\n\n"
    "Question: {question}\n\n"
    "Reply with only a JSON object, no prose and no code fence: "
    '{{"files": [repository-relative paths of the source files that would need to change, '
    'most likely first, at most 6], "symbols": [relevant function or type names]}}'
)
JSON_RE = re.compile(r"\{.*\}", re.S)


def load(path: Path):
    return json.loads(path.read_text())


def setup(repo: Path, data: Path, ref: str) -> None:
    for arm in ARMS:
        target = data / arm
        if target.exists():
            subprocess.run(["git", "-C", str(repo), "worktree", "remove", "--force", str(target)], check=True)
        subprocess.run(["git", "-C", str(repo), "worktree", "add", "--detach", str(target), ref], check=True)
    shutil.rmtree(data / "a" / MAP_DIR)
    (data / "snapshot.txt").write_text(
        subprocess.run(["git", "-C", str(data / "b"), "rev-parse", "HEAD"], capture_output=True, text=True, check=True).stdout
    )


def experiment_id(data: Path, model: str) -> str:
    """Results only combine within one snapshot, model, prompt, question set, and gold set."""
    digest = hashlib.sha256()
    for part in ((data / "snapshot.txt").read_text().strip(), model, POINTER, PROMPT,
                 (data / "questions.json").read_text(), json.dumps([(t["id"], t["gold"]) for t in load(data / "tasks.json")])):
        digest.update(part.encode() + b"\0")
    return digest.hexdigest()[:16]


def read_results(data: Path, experiment: str) -> dict[tuple, dict]:
    latest: dict[tuple, dict] = {}
    path = data / "results.jsonl"
    for line in path.read_text().splitlines() if path.exists() else []:
        r = json.loads(line)
        if r.get("experiment") != experiment:
            continue
        key = (r["id"], r["arm"], r["repeat"])
        if key not in latest or r.get("answer") is not None:
            latest[key] = r
    return latest


def parse_answer(text: str) -> dict | None:
    match = JSON_RE.search(text)
    if not match:
        return None
    try:
        value = json.loads(match.group(0))
    except json.JSONDecodeError:
        return None
    if not isinstance(value, dict) or not isinstance(value.get("files"), list):
        return None
    return {"files": [str(f).removeprefix("./") for f in value["files"] if isinstance(f, str)][:6],
            "symbols": [str(s) for s in value.get("symbols", []) if isinstance(s, str)][:12]}


def usage_totals(path: Path) -> dict:
    try:
        usage = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError):
        return {}
    details = usage.get("tokenDetails", {})
    return {
        "input": details.get("input", {}).get("tokenCount", 0),
        "cache_read": details.get("cache_read", {}).get("tokenCount", 0),
        "output": details.get("output", {}).get("tokenCount", 0),
        "premium": usage.get("totalPremiumRequestCost", 0) or 0,
    }


def run_one(data: Path, task: dict, question: str, arm: str, repeat: int, model: str, timeout: float, experiment: str = "") -> dict:
    prompt = (POINTER if arm == "b" else "") + PROMPT.format(question=question)
    with tempfile.TemporaryDirectory(dir=data) as scratch:
        usage_file = Path(scratch) / "usage.json"
        command = [
            os.environ.get("COPILOT_BIN", "copilot"), "-p", prompt, "-s", "--model", model,
            "--available-tools", "view", "grep", "glob", "--no-ask-user", "--no-custom-instructions",
            "--usage-output-file", str(usage_file),
        ]
        started = time.monotonic()
        error = None
        try:
            result = subprocess.run(command, cwd=data / arm, capture_output=True, text=True, timeout=timeout)
            stdout = result.stdout
            if result.returncode:
                error = f"exit {result.returncode}: {result.stderr.strip()[-300:]}"
        except subprocess.TimeoutExpired:
            stdout, error = "", "timeout"
        elapsed = time.monotonic() - started
        usage = usage_totals(usage_file)
    answer = parse_answer(stdout)
    if answer is None and error is None:
        error = "unparseable answer"
    return {"experiment": experiment, "id": task["id"], "arm": arm, "repeat": repeat, "model": model, "elapsed": round(elapsed, 1),
            "usage": usage, "answer": answer, "error": error, "raw_tail": stdout[-400:] if answer is None else ""}


def run(data: Path, model: str, repeats: int, jobs: int, seed: int, timeout: float, limit: int | None) -> None:
    tasks = {t["id"]: t for t in load(data / "tasks.json")}
    questions = {q["id"]: q["question"] for q in load(data / "questions.json")}
    ids = sorted(tasks)[:limit] if limit else sorted(tasks)
    results_path = data / "results.jsonl"
    experiment = experiment_id(data, model)
    done = {key for key, r in read_results(data, experiment).items() if r.get("answer") is not None}
    trials = [(i, arm, rep) for i in ids for arm in ARMS for rep in range(repeats) if (i, arm, rep) not in done]
    random.Random(seed).shuffle(trials)
    print(f"experiment {experiment}: {len(trials)} trials to run ({len(done)} already complete)", flush=True)
    lock = threading.Lock()
    with concurrent.futures.ThreadPoolExecutor(jobs) as pool:
        futures = [pool.submit(run_one, data, tasks[i], questions[i], arm, rep, model, timeout, experiment) for i, arm, rep in trials]
        for count, future in enumerate(concurrent.futures.as_completed(futures), 1):
            record = future.result()
            with lock, results_path.open("a") as handle:
                handle.write(json.dumps(record) + "\n")
            print(f"[{count}/{len(trials)}] {record['id']} {record['arm']}{record['repeat']} "
                  f"{record['elapsed']}s {record['error'] or 'ok'}", flush=True)


def score(answer: dict | None, gold: list[str]) -> dict:
    files = answer["files"] if answer else []
    gold_set = set(gold)
    hits = [f for f in files if f in gold_set]
    return {
        "top1": bool(files) and files[0] in gold_set,
        "any": bool(hits),
        "recall": len(set(hits)) / len(gold_set),
        "precision": len(set(hits)) / len(set(files)) if files else 0.0,
    }


def pct(values) -> str:
    values = list(values)
    return f"{100 * sum(values) / len(values):.1f}%" if values else "n/a"


def report(data: Path, model: str) -> None:
    tasks = {t["id"]: t for t in load(data / "tasks.json")}
    experiment = experiment_id(data, model)
    latest = read_results(data, experiment)
    rows = {arm: [] for arm in ARMS}
    for (tid, arm, _), r in latest.items():
        s = score(r.get("answer"), tasks[tid]["gold"])
        rows[arm].append({**s, **r.get("usage", {}), "id": tid, "elapsed": r["elapsed"], "error": r.get("error")})
    print(f"experiment {experiment}  snapshot {(data / 'snapshot.txt').read_text().strip()[:12]}  model {model}  tasks {len(tasks)}")
    print("| Metric | Arm A (no map) | Arm B (map) |\n| --- | --- | --- |")
    for label, key in (("Top-1 file hit", "top1"), ("Any gold file hit", "any")):
        print(f"| {label} | {pct(r[key] for r in rows['a'])} | {pct(r[key] for r in rows['b'])} |")
    for label, key in (("Gold recall", "recall"), ("Precision", "precision")):
        print(f"| {label} | {pct(r[key] for r in rows['a'])} | {pct(r[key] for r in rows['b'])} |")
    for label, key in (("Median input tokens", "input"), ("Median output tokens", "output"), ("Median seconds", "elapsed")):
        med = {arm: statistics.median([r.get(key, 0) for r in rows[arm]]) if rows[arm] else 0 for arm in ARMS}
        print(f"| {label} | {med['a']:,.0f} | {med['b']:,.0f} |")
    for arm in ARMS:
        n = len(rows[arm])
        errors = sum(1 for r in rows[arm] if r["error"])
        premium = sum(r.get("premium", 0) for r in rows[arm])
        tokens = sum(r.get("input", 0) for r in rows[arm])
        successes = sum(r["any"] for r in rows[arm])
        per = f"{tokens / successes:,.0f}" if successes else "n/a"
        print(f"arm {arm}: trials {n}, errors {errors}, premium {premium:g}, input tokens per success {per}")
    by_task = {}
    for arm in ARMS:
        for r in rows[arm]:
            by_task.setdefault(r["id"], {}).setdefault(arm, []).append(r)
    diffs = [statistics.mean(x["recall"] for x in v["b"]) - statistics.mean(x["recall"] for x in v["a"])
             for v in by_task.values() if v.get("a") and v.get("b")]
    if diffs:
        wins = sum(d > 0 for d in diffs)
        losses = sum(d < 0 for d in diffs)
        print(f"paired per-task recall delta (B-A): mean {statistics.mean(diffs):+.3f}, "
              f"B better {wins}, A better {losses}, tied {len(diffs) - wins - losses}")
    domains = sorted({t["domain"] for t in tasks.values()})
    print("\n| Domain | n | A recall | B recall |\n| --- | --- | --- | --- |")
    for d in domains:
        ids = {i for i, t in tasks.items() if t["domain"] == d}
        a = [r["recall"] for r in rows["a"] if r["id"] in ids]
        b = [r["recall"] for r in rows["b"] if r["id"] in ids]
        print(f"| {d} | {len(ids)} | {pct(a)} | {pct(b)} |")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--data", type=Path, required=True, help="directory outside the repository")
    sub = parser.add_subparsers(dest="command", required=True)
    s = sub.add_parser("setup")
    s.add_argument("--ref", default="HEAD")
    r = sub.add_parser("run")
    r.add_argument("--model", required=True)
    r.add_argument("--repeats", type=int, default=3)
    r.add_argument("--jobs", type=int, default=6)
    r.add_argument("--seed", type=int, default=1)
    r.add_argument("--timeout", type=float, default=600)
    r.add_argument("--limit", type=int)
    sub.add_parser("report").add_argument("--model", required=True)
    args = parser.parse_args()
    repo = Path(__file__).resolve().parent.parent
    data = args.data.resolve()
    if data == repo or repo in data.parents:
        parser.error("--data must be outside the repository")
    if args.command == "setup":
        setup(repo, data, args.ref)
    elif args.command == "run":
        run(data, args.model, args.repeats, args.jobs, args.seed, args.timeout, args.limit)
    else:
        report(data, args.model)
    return 0


if __name__ == "__main__":
    sys.exit(main())
