#!/usr/bin/env python3
"""textlint の指摘を LSP の Diagnostic の JSON に直す — akapen の `--lint-cmd` 用。

    textlint-diagnostics.py [textlint の引数...] <file>
    textlint-diagnostics.py --unix-oneline [textlint の引数...] <file>

textlint を `--format json` で走らせ、1 ファイル分の指摘を

    {"diagnostics": [{"range": {"start": {"line": 71, "character": 25}, ...},
                      "message": "...", "source": "textlint",
                      "code": "ja-no-weak-phrase", "severity": 1}]}

の形で標準出力に書く（`line` は 0 始まり、`character` は UTF-16 のコード単位）。
位置は textlint の `range`（文書全体での UTF-16 の添字）から計算し、無ければ
`loc` を使う。`code` はルール id の最後の区切り（`ja-technical-writing/ja-no-weak-phrase`
なら `ja-no-weak-phrase`）で、元の id は `data.ruleId` に残す。

`--unix-oneline` を付けると、代わりに 1 件 1 行の

    <file>:<行>:<桁>: <メッセージ> (<ルール id>)

を書く（行・桁は 1 始まり、桁は文字単位）。micro エディタの linter プラグインが
`%f:%l:%c: %m` で読む形である。複数行のメッセージは 1 行に畳む。

textlint のコマンドは環境変数 `TEXTLINT`（既定 `textlint`）。設定ファイルは
textlint 自身が作業ディレクトリから上へ探す。textlint は指摘があると exit 1 で
終わるが、このスクリプトは JSON を読めたら exit 0 で終わる。読めなければ標準出力に
何も書かずに exit 2（akapen は「0 件」ではなく「形が違う」と出す）。

Python 標準ライブラリだけで動く。
"""

from __future__ import annotations

import bisect
import json
import os
import shlex
import subprocess
import sys

#: textlint の severity（1 warning / 2 error / 3 info）→ LSP（1 Error / 2 Warning / 3 Information）。
SEVERITY = {2: 1, 1: 2, 3: 3}


class Text:
    """文書の中の位置を、UTF-16 の添字・文字の添字・(行, 桁) の間で直す。"""

    def __init__(self, text: str):
        self.text = text
        # u16[i] は text[:i] の UTF-16 のコード単位数（len(text)+1 個）。
        self.u16 = [0]
        for ch in text:
            self.u16.append(self.u16[-1] + (2 if ord(ch) > 0xFFFF else 1))
        # 各行の頭の文字添字。
        self.line_starts = [0] + [i + 1 for i, ch in enumerate(text) if ch == "\n"]

    def char_of_u16(self, offset: int) -> int:
        """UTF-16 の添字 → 文字の添字（サロゲートの間なら手前の文字）。"""
        offset = max(0, min(offset, self.u16[-1]))
        return bisect.bisect_right(self.u16, offset) - 1

    def char_of_loc(self, line: int, column: int) -> int:
        """textlint の loc（行 1 始まり・桁は UTF-16 で 0 始まり）→ 文字の添字。"""
        index = min(max(line - 1, 0), len(self.line_starts) - 1)
        start = self.line_starts[index]
        return self.char_of_u16(self.u16[start] + max(column, 0))

    def position(self, char: int) -> tuple[int, int, int]:
        """文字の添字 → (0 始まりの行, 行頭からの UTF-16 の桁, 行頭からの文字数)。"""
        line = bisect.bisect_right(self.line_starts, char) - 1
        start = self.line_starts[line]
        return line, self.u16[char] - self.u16[start], char - start


def fold(message: str) -> str:
    """改行と連続する空白を 1 つの空白に畳む。"""
    return " ".join(message.split())


def span(message: dict, text: Text) -> tuple[int, int] | None:
    """1 件の指摘の (始まり, 終わり) を文字の添字で。取れなければ None。"""
    rng = message.get("range")
    if isinstance(rng, list) and len(rng) == 2 and all(isinstance(n, int) for n in rng):
        return text.char_of_u16(rng[0]), text.char_of_u16(rng[1])
    loc = message.get("loc")
    if isinstance(loc, dict):
        try:
            start = loc["start"]
            end = loc["end"]
            # textlint の loc.column は 1 始まり（v12 以降）。
            return (
                text.char_of_loc(start["line"], start["column"] - 1),
                text.char_of_loc(end["line"], end["column"] - 1),
            )
        except (KeyError, TypeError):
            pass
    line, column = message.get("line"), message.get("column")
    if isinstance(line, int) and isinstance(column, int):
        at = text.char_of_loc(line, column - 1)
        return at, at
    return None


def findings(results, text: Text):
    """textlint の JSON（ファイルごとの結果の配列）から (始まり, 終わり, 指摘) を並べる。"""
    if not isinstance(results, list):
        raise ValueError("textlint の出力が配列でない")
    for result in results[:1]:  # 1 ファイル分だけ（最後の引数の文書）
        for message in result.get("messages", []):
            if not isinstance(message, dict) or not isinstance(message.get("message"), str):
                continue
            at = span(message, text)
            if at is None:
                continue
            yield at[0], at[1], message


def to_diagnostics(results, source: str) -> dict:
    """textlint の JSON → `{"diagnostics": [...]}`。"""
    text = Text(source)
    out = []
    for start, end, message in findings(results, text):
        rule_id = str(message.get("ruleId") or "")
        s_line, s_char, _ = text.position(start)
        e_line, e_char, _ = text.position(max(start, end))
        diagnostic = {
            "range": {
                "start": {"line": s_line, "character": s_char},
                "end": {"line": e_line, "character": e_char},
            },
            "message": message["message"],
            "source": "textlint",
        }
        if rule_id:
            diagnostic["code"] = rule_id.rsplit("/", 1)[-1]
            diagnostic["data"] = {"ruleId": rule_id}
        severity = SEVERITY.get(message.get("severity", 0))
        if severity is not None:
            diagnostic["severity"] = severity
        out.append(diagnostic)
    return {"diagnostics": out}


def to_oneline(results, source: str, file: str) -> str:
    """textlint の JSON → `<file>:<行>:<桁>: <メッセージ> (<ルール id>)` を 1 件 1 行。"""
    text = Text(source)
    lines = []
    for start, _, message in findings(results, text):
        line, _, column = text.position(start)
        rule_id = message.get("ruleId")
        body = fold(message["message"])
        if rule_id:
            body = f"{body} ({rule_id})"
        lines.append(f"{file}:{line + 1}:{column + 1}: {body}")
    return "".join(line + "\n" for line in lines)


def main(argv: list[str]) -> int:
    oneline = "--unix-oneline" in argv
    args = [a for a in argv if a != "--unix-oneline"]
    if not args:
        print("usage: textlint-diagnostics.py [--unix-oneline] [textlint args...] <file>", file=sys.stderr)
        return 2
    file = args[-1]
    try:
        with open(file, encoding="utf-8", newline="") as handle:
            source = handle.read()
    except OSError as error:
        print(f"textlint-diagnostics: {error}", file=sys.stderr)
        return 2
    textlint = shlex.split(os.environ.get("TEXTLINT", "textlint"))
    try:
        done = subprocess.run(
            [*textlint, "--format", "json", *args],
            capture_output=True,
            text=True,
        )
    except OSError as error:
        print(f"textlint-diagnostics: {error}", file=sys.stderr)
        return 2
    try:
        results = json.loads(done.stdout)
        if oneline:
            body = to_oneline(results, source, file)
        else:
            body = json.dumps(to_diagnostics(results, source), ensure_ascii=False) + "\n"
    except (ValueError, AttributeError) as error:
        # textlint 自身のエラー（設定が無い・ルールが入っていない）は stderr にある。
        sys.stderr.write(done.stderr)
        print(f"textlint-diagnostics: textlint did not print JSON ({error})", file=sys.stderr)
        return 2
    sys.stdout.write(body)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
