#!/usr/bin/env python3
"""natural-japanese の `lint.py --json` を LSP の Diagnostic の JSON に直す — `--lint-cmd` 用。

    natural-japanese-diagnostics.py [--lint-py <lint.py>] <file>

`lint.py`（`japanese` スキル。coji/natural-japanese 由来）の出力

    {"file": ..., "stats": ..., "findings": [{"line", "category", "excerpt", "severity", "detail"}]}

を `{"diagnostics": [...]}` に直して標準出力に書く。`lint.py` の指摘は行単位なので、
**その行全体**を範囲にする。`code` は `category`、`message` は `detail`（無ければ
`category: excerpt`）、`source` は `natural-japanese`。

`lint.py` の場所は `--lint-py` か環境変数 `NATURAL_JAPANESE_LINT_PY`。走らせ方は
環境変数 `NATURAL_JAPANESE_RUNNER`（既定: `uv` があれば `uv run`、無ければ
`python3`。`lint.py` は依存をスクリプトの先頭に書いているので `uv run` が要る）。

`lint.py` が JSON を出せなければ、標準出力に何も書かずに exit 2。

Python 標準ライブラリだけで動く。
"""

from __future__ import annotations

import json
import os
import shlex
import shutil
import subprocess
import sys

#: lint.py の severity → LSP（1 Error / 2 Warning / 3 Information）。
SEVERITY = {"critical": 1, "warn": 2, "info": 3}

def utf16_len(text: str) -> int:
    return sum(2 if ord(ch) > 0xFFFF else 1 for ch in text)


def message_of(finding: dict) -> str:
    """`detail` を理由の文にする。**`excerpt` は足さない** — 範囲が行全体なので
    本文は下線で見えていて、足すと一覧の 1 行が抜粋で埋まる。`detail` が無い
    ときだけ `category` と `excerpt` で埋める。"""
    detail = " ".join(str(finding.get("detail") or "").split())
    if detail:
        return detail
    excerpt = " ".join(str(finding.get("excerpt") or "").split())
    category = str(finding.get("category") or "")
    return f"{category}: {excerpt}" if excerpt else category


def to_diagnostics(report, source: str) -> dict:
    """lint.py の JSON → `{"diagnostics": [...]}`。行の外の指摘は捨てる。"""
    if not isinstance(report, dict) or not isinstance(report.get("findings"), list):
        raise ValueError("lint.py の出力に findings が無い")
    lines = source.split("\n")
    out = []
    for finding in report["findings"]:
        if not isinstance(finding, dict):
            continue
        line = finding.get("line")
        if not isinstance(line, int) or not 1 <= line <= len(lines):
            continue
        body = lines[line - 1].removesuffix("\r")
        diagnostic = {
            "range": {
                "start": {"line": line - 1, "character": 0},
                "end": {"line": line - 1, "character": utf16_len(body)},
            },
            "message": message_of(finding),
            "source": "natural-japanese",
        }
        if finding.get("category"):
            diagnostic["code"] = str(finding["category"])
        severity = SEVERITY.get(str(finding.get("severity", "")))
        if severity is not None:
            diagnostic["severity"] = severity
        out.append(diagnostic)
    return {"diagnostics": out}


def runner() -> list[str]:
    configured = os.environ.get("NATURAL_JAPANESE_RUNNER")
    if configured:
        return shlex.split(configured)
    return ["uv", "run"] if shutil.which("uv") else [sys.executable]


def main(argv: list[str]) -> int:
    lint_py = os.environ.get("NATURAL_JAPANESE_LINT_PY")
    args = list(argv)
    if "--lint-py" in args:
        at = args.index("--lint-py")
        if at + 1 >= len(args):
            print("natural-japanese-diagnostics: --lint-py needs a path", file=sys.stderr)
            return 2
        lint_py = args[at + 1]
        del args[at : at + 2]
    if len(args) != 1:
        print("usage: natural-japanese-diagnostics.py [--lint-py <lint.py>] <file>", file=sys.stderr)
        return 2
    if not lint_py:
        print(
            "natural-japanese-diagnostics: set --lint-py or NATURAL_JAPANESE_LINT_PY",
            file=sys.stderr,
        )
        return 2
    file = args[0]
    try:
        with open(file, encoding="utf-8", newline="") as handle:
            source = handle.read()
    except OSError as error:
        print(f"natural-japanese-diagnostics: {error}", file=sys.stderr)
        return 2
    try:
        done = subprocess.run(
            [*runner(), lint_py, file, "--json"],
            capture_output=True,
            text=True,
        )
    except OSError as error:
        print(f"natural-japanese-diagnostics: {error}", file=sys.stderr)
        return 2
    try:
        diagnostics = to_diagnostics(json.loads(done.stdout), source)
    except ValueError as error:
        sys.stderr.write(done.stderr)
        print(f"natural-japanese-diagnostics: lint.py did not print JSON ({error})", file=sys.stderr)
        return 2
    json.dump(diagnostics, sys.stdout, ensure_ascii=False)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
