"""`examples/lint/` の変換スクリプトのテスト — **実物の textlint も lint.py も呼ばない。**

    python3 -m pytest examples/lint -q

固定の JSON を入力にして、LSP の Diagnostic の形（0 始まりの行・UTF-16 の桁）と
`--unix-oneline` の形（1 始まり・文字単位・1 行に畳む）を確かめる。CLI の道は、
`TEXTLINT` / `NATURAL_JAPANESE_RUNNER` に固定の JSON を吐く偽物を差して通す。

ファイル名にハイフンが入っていて普通の import ができないので、
`examples/semantic/test_jev_annotate.py` と同じく SourceFileLoader で読む。
"""

import importlib.machinery
import importlib.util
import json
import os
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent


def load(name):
    loader = importlib.machinery.SourceFileLoader(name.replace("-", "_"), str(HERE / name))
    spec = importlib.util.spec_from_loader(loader.name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


textlint = load("textlint-diagnostics.py")
natural = load("natural-japanese-diagnostics.py")

#: 2 行目に「かも」、3 行目に 😀（UTF-16 で 2 単位）の後ろの「かも」。
SOURCE = "# 見出し\n効くかもしれない。\n😀かも\n"


def u16(text):
    return len(text.encode("utf-16-le")) // 2


def textlint_json(messages):
    return [{"filePath": "/x/doc.md", "messages": messages}]


def weak(start, end, **extra):
    message = {
        "ruleId": "ja-technical-writing/ja-no-weak-phrase",
        "message": '弱い表現: "かも" が使われています。',
        "range": [start, end],
        "severity": 2,
    }
    message.update(extra)
    return message


def test_textlint_range_becomes_a_zero_based_utf16_position():
    at = u16("# 見出し\n効く")
    out = textlint.to_diagnostics(textlint_json([weak(at, at + 2)]), SOURCE)
    (d,) = out["diagnostics"]
    assert d["range"] == {
        "start": {"line": 1, "character": 2},
        "end": {"line": 1, "character": 4},
    }
    assert d["code"] == "ja-no-weak-phrase"
    assert d["data"] == {"ruleId": "ja-technical-writing/ja-no-weak-phrase"}
    assert d["source"] == "textlint"
    assert d["severity"] == 1, "textlint の error は LSP の Error"


def test_an_astral_character_counts_two_utf16_units():
    at = u16("# 見出し\n効くかもしれない。\n😀")
    (d,) = textlint.to_diagnostics(textlint_json([weak(at, at + 2)]), SOURCE)["diagnostics"]
    assert d["range"]["start"] == {"line": 2, "character": 2}


def test_loc_is_used_when_range_is_missing():
    message = weak(0, 0)
    del message["range"]
    message["loc"] = {"start": {"line": 2, "column": 3}, "end": {"line": 2, "column": 5}}
    (d,) = textlint.to_diagnostics(textlint_json([message]), SOURCE)["diagnostics"]
    assert d["range"]["start"] == {"line": 1, "character": 2}
    assert d["range"]["end"] == {"line": 1, "character": 4}


def test_severity_maps_warning_and_info():
    at = u16("# 見出し\n効く")
    out = textlint.to_diagnostics(
        textlint_json([weak(at, at + 2, severity=1), weak(at, at + 2, severity=3)]), SOURCE
    )
    assert [d["severity"] for d in out["diagnostics"]] == [2, 3]


def test_unix_oneline_is_one_based_in_characters_and_folds_the_message():
    at = u16("# 見出し\n効くかもしれない。\n😀")
    message = weak(at, at + 2, message="文末が\"。\"で終わっていません。\n理由: 句点\n修正:  足す")
    out = textlint.to_oneline(textlint_json([message]), SOURCE, "doc.md")
    assert out == (
        'doc.md:3:2: 文末が"。"で終わっていません。 理由: 句点 修正: 足す'
        " (ja-technical-writing/ja-no-weak-phrase)\n"
    )


def test_not_an_array_is_an_error():
    for bad in ({}, "x", None):
        try:
            textlint.to_diagnostics(bad, SOURCE)
        except ValueError:
            continue
        raise AssertionError(f"{bad!r} が通ってしまった")


def fake(tmp_path, name, stdout, code=0):
    """固定の stdout を吐いて `code` で終わる偽物のコマンド。引数は args に残す。"""
    script = tmp_path / name
    (tmp_path / f"{name}.out").write_text(stdout, encoding="utf-8")
    script.write_text(
        f'#!/bin/sh\nprintf "%s\\n" "$@" > "{tmp_path}/{name}.args"\n'
        f'cat "{tmp_path}/{name}.out"\nexit {code}\n',
        encoding="utf-8",
    )
    script.chmod(0o755)
    return script


def run(script, args, env):
    return subprocess.run(
        [sys.executable, str(HERE / script), *args],
        capture_output=True,
        text=True,
        env={**os.environ, **env},
    )


def test_the_cli_passes_textlint_args_and_accepts_exit_1(tmp_path):
    doc = tmp_path / "doc.md"
    doc.write_text(SOURCE, encoding="utf-8")
    at = u16("# 見出し\n効く")
    bin_ = fake(tmp_path, "textlint", json.dumps(textlint_json([weak(at, at + 2)])), code=1)
    done = run("textlint-diagnostics.py", ["--config", "/cfg", str(doc)], {"TEXTLINT": str(bin_)})
    assert done.returncode == 0, done.stderr
    assert json.loads(done.stdout)["diagnostics"][0]["range"]["start"]["line"] == 1
    args = (tmp_path / "textlint.args").read_text(encoding="utf-8").split("\n")
    assert args[:5] == ["--format", "json", "--config", "/cfg", str(doc)]

    done = run("textlint-diagnostics.py", ["--unix-oneline", str(doc)], {"TEXTLINT": str(bin_)})
    assert done.stdout.startswith(f"{doc}:2:3: 弱い表現"), done.stdout
    assert "--unix-oneline" not in (tmp_path / "textlint.args").read_text(encoding="utf-8")


def test_the_cli_prints_nothing_when_textlint_prints_no_json(tmp_path):
    doc = tmp_path / "doc.md"
    doc.write_text(SOURCE, encoding="utf-8")
    bin_ = fake(tmp_path, "textlint", "Error: No rules found", code=1)
    done = run("textlint-diagnostics.py", [str(doc)], {"TEXTLINT": str(bin_)})
    assert done.returncode == 2
    assert done.stdout == "", "形の違う出力を 0 件の JSON に見せない"


# ---- natural-japanese -------------------------------------------------------

REPORT = {
    "file": "doc.md",
    "stats": {},
    "findings": [
        {"line": 2, "category": "forbidden_phrase", "excerpt": "かもしれない",
         "severity": "warn", "detail": "禁止語/LLM常套句ヒット: 「かもしれない」"},
        {"line": 3, "category": "low_burstiness", "excerpt": "burstiness=-0.3",
         "severity": "info", "detail": "burstiness が閾値未満"},
        {"line": 99, "category": "x", "excerpt": "", "severity": "critical", "detail": "行の外"},
    ],
}


def test_a_line_finding_spans_the_whole_line():
    out = natural.to_diagnostics(REPORT, SOURCE)["diagnostics"]
    assert len(out) == 2, "行の外は捨てる"
    assert out[0]["range"] == {
        "start": {"line": 1, "character": 0},
        "end": {"line": 1, "character": u16("効くかもしれない。")},
    }
    assert out[1]["range"]["end"] == {"line": 2, "character": 4}, "😀 は 2 単位"
    assert out[0]["code"] == "forbidden_phrase"
    assert out[0]["source"] == "natural-japanese"
    assert [d["severity"] for d in out] == [2, 3]


def test_the_message_is_the_detail_and_the_excerpt_only_fills_a_missing_one():
    out = natural.to_diagnostics(REPORT, SOURCE)["diagnostics"]
    assert out[0]["message"] == "禁止語/LLM常套句ヒット: 「かもしれない」"
    assert out[1]["message"] == "burstiness が閾値未満", "excerpt は足さない"
    assert natural.message_of({"category": "x", "excerpt": "抜粋"}) == "x: 抜粋"


def test_a_report_without_findings_is_an_error():
    for bad in ({}, [], {"findings": "x"}):
        try:
            natural.to_diagnostics(bad, SOURCE)
        except ValueError:
            continue
        raise AssertionError(f"{bad!r} が通ってしまった")


def test_the_natural_cli_runs_lint_py_with_json(tmp_path):
    doc = tmp_path / "doc.md"
    doc.write_text(SOURCE, encoding="utf-8")
    runner = fake(tmp_path, "runner", json.dumps(REPORT))
    env = {"NATURAL_JAPANESE_RUNNER": str(runner), "NATURAL_JAPANESE_LINT_PY": "/skill/lint.py"}
    done = run("natural-japanese-diagnostics.py", [str(doc)], env)
    assert done.returncode == 0, done.stderr
    assert len(json.loads(done.stdout)["diagnostics"]) == 2
    args = (tmp_path / "runner.args").read_text(encoding="utf-8").split("\n")
    assert args[:3] == ["/skill/lint.py", str(doc), "--json"]
    # `--lint-py` が環境変数に勝つ。
    done = run("natural-japanese-diagnostics.py", ["--lint-py", "/mine.py", str(doc)], env)
    assert (tmp_path / "runner.args").read_text(encoding="utf-8").startswith("/mine.py\n")


def test_the_natural_cli_needs_the_lint_py_location(tmp_path):
    doc = tmp_path / "doc.md"
    doc.write_text(SOURCE, encoding="utf-8")
    env = {k: v for k, v in os.environ.items() if k != "NATURAL_JAPANESE_LINT_PY"}
    done = subprocess.run(
        [sys.executable, str(HERE / "natural-japanese-diagnostics.py"), str(doc)],
        capture_output=True, text=True, env=env,
    )
    assert done.returncode == 2
    assert done.stdout == ""
    assert "NATURAL_JAPANESE_LINT_PY" in done.stderr
