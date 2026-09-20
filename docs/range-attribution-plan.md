# akapen Range Attribution / Selection 改修計画

## 目的

現在のakapenはsource lineを主要な位置単位として扱っている。

これを、

> **line-oriented attributionからrange-oriented attributionへ拡張する**

ことを目的とする。

主要な一次位置情報を、

```text
source line
```

から、

```text
source byte/span
```

へ高精度化する。

ただし既存の行単位レビューUXは維持する。

---

# なぜ必要か

現在のline attributionでは、

```text
line 42 = selected
```

までは扱える。

しかし、

```text
line 42

[この部分だけ重要][この部分は通常][ここは重複]
```

という同一行内の違いは表現できない。

source byte/spanを導入すると、

```text
start_byte: 1240
end_byte:   1294
```

という範囲で任意のsource部分を指定できる。

---

# Semantic Reading Layerとの関係

Semantic Reading Layerから見ると、本機能はインフラである。

```text
akapen Range Infrastructure
          ↑
          │
Semantic Reading Layer
```

Semantic Reading Layer専用の実装にはしない。

将来的には、

- 部分コメント
- マウス範囲選択
- source/render同期
- Atom selection
- semantic selection
- より精密なdiff表示

にも利用できる。

---

# 現状

vendored `tui-markdown` はすでにpulldown-cmarkの、

```rust
Parser::into_offset_iter()
```

を利用している。

つまりrenderer内部には、

```rust
Event + Range<usize>
```

がすでに存在する。

現在はこのbyte rangeからsource lineを求め、

```rust
Option<usize>
```

としてrendered spanへ紐付けている。

したがって新しくsource位置情報を取得する必要はない。

> **すでに存在するbyte-range情報を、途中で捨てずrendered outputまで運ぶ**

のが本改修の中心となる。

---

# SourceSpan

共通位置型を導入する。

```rust
struct SourceSpan {
    start_byte: usize,
    end_byte: usize,

    start_line: usize,
    start_col: usize,

    end_line: usize,
    end_col: usize,
}
```

正規の位置情報は、

```text
start_byte
end_byte
```

とする。

line / columnはUI・デバッグ・表示用の補助情報。

---

# Byte Offsetを一次情報にする理由

UTF-8のため、

```text
byte offset
character index
terminal cell
```

は一致しない。

そのため責務を分離する。

```text
source identity
→ byte offset

human display
→ line / column

terminal layout
→ cell position
```

内部のsource identityはbyte rangeで統一する。

---

# Render Attribution

rendered spanにもsource rangeを持たせる。

概念:

```rust
struct SourceTaggedSpan {
    text: String,
    style: Style,
    source: Option<Range<usize>>,
}
```

またはSourceSpanを直接保持する。

---

# Synthetic Span

Markdown renderingではsourceに存在しない文字列が生成される。

例:

- list prefix
- quote prefix
- table border
- padding
- paragraph separator

これらは、

```rust
source: None
```

とする。

これは現在のsource-line attributionでも採用している考え方を、そのままbyte rangeへ拡張する。

---

# Span Splitting

最大の重要ポイント。

Markdown parserの一つのeventに複数の意味範囲が含まれることがある。

例:

```text
Jevは高速だ。しかし文章生成には向かない。
```

parser上は1つのText Eventでも、

```text
Atom A
Jevは高速だ。

Atom B
しかし文章生成には向かない。
```

になる可能性がある。

その場合、

```text
RenderedSpan
source: 100..166
```

を、

```text
100..124
124..166
```

の境界で分割できる必要がある。

range decoration適用時には、

```text
rendered source span
∩
decoration source span
```

のintersectionを計算し、必要ならSpanを分割する。

---

# source view

source viewでは比較的単純。

source byte rangeから、

```text
line
column
terminal cell
```

へ変換してStyleを適用する。

同じsource line内でも、

```text
[MARKED][NORMAL][DIM]
```

を別々に表示できる。

---

# rendered Markdown view

こちらが本改修の中心的な技術課題。

Markdownでは、

```markdown
**重要な結論**
```

が、

```text
重要な結論
```

としてrenderされる。

sourceとrendered textは一対一ではない。

そのため、

```text
source byte/span
       ↓
rendered span
```

のmappingを保持する。

---

# Source-to-Render Mapping

vendored `tui-markdown` のsource-line attributionを、

```text
Option<source_line>
```

から、

```text
Option<source_byte_range>
```

へ拡張する。

理想的には各rendered spanが、

```rust
RenderedSpan {
    text,
    style,
    source_range,
}
```

を持つ。

その後wrapされてもsource attributionを保持する。

---

# Markdown構造別の精度

MVPですべてを完全可逆にする必要はない。

目標:

```text
plain text
→ byte精度

heading text
→ byte精度

emphasis / strong text
→ byte精度

link label
→ byte精度

list item text
→ byte精度

blockquote text
→ byte精度

table
→ row / cell程度でも可

code block
→ blockまたはline程度でも可

synthetic characters
→ source=None
```

目的はIDEレベルのcursor mappingではなく、range decorationとレビュー支援である。

---

# 既存UXは維持する

内部位置モデルをrange化しても、akapenの操作体系を即座に変更しない。

MVP:

```text
内部
→ source byte/span

ユーザー操作
→ 従来どおり行選択
```

これにより既存ユーザーへの影響を最小化する。

---

# Range Decoration

最初に利用する新機能。

任意のSourceSpanへStyleを適用できるAPIを用意する。

概念:

```rust
Decoration {
    range: SourceSpan,
    kind: DecorationKind,
}
```

例:

```text
SemanticMark
Dim
SearchMatch
CommentRange
DiffRange
```

Semantic Reading LayerはこのAPIだけを利用する。

---

# Style Composition

range decorationがsyntax highlight等を壊してはいけない。

優先順位の例:

```text
selection
    >
comment focus
    >
diff
    >
semantic decoration
    >
syntax highlight
```

可能ならStyleを全面的に置換せず、modifier/background等をpatchする。

---

# 将来: Range Selection

source range基盤が安定した後、ユーザーによる文字範囲選択を追加できる。

ただし既存の行選択を置き換えない。

```text
Line Selection
→ 通常レビュー

Range Selection
→ 特定表現への精密コメント
```

---

# マウス操作

将来候補。

マウスドラッグ時のみrange selectionを可能にする案がある。

```text
keyboard
→ line selection

mouse drag
→ range selection
```

これならキーボード中心の既存UXを維持できる。

ただしterminal native text selectionとの競合を考慮する必要がある。

MVPには含めない。

---

# 将来: Semantic Selection

Semantic Reading LayerでAtomが利用可能になれば、

```text
現在位置
↓
Atomを選択
```

というsemantic selectionも可能。

将来的な選択粒度:

```text
Line
↓
Atom
↓
Raw Span
```

ただしこれもMVPには含めない。

---

# 将来: Range Comment

現在の行コメントに加え、

```text
「非常に高速」
```

だけを選んで、

```text
ここは言い切りすぎ
```

というコメントを付けられる。

概念的には、

```rust
CommentTarget::Lines(...)
CommentTarget::Range(SourceSpan)
```

のように既存コメントモデルを拡張する。

---

# Architecture

```text
Source File
    ↓
SourceSpan Model
    ↓
Markdown Parser
    ↓
Event + Byte Range
    ↓
tui-markdown Renderer
    ↓
Rendered Span + Source Range
    ↓
Wrapping
    ↓
Rendered Row + Source Mapping
    ↓
Range Decoration Layer
    ↓
akapen View
```

将来的には、

```text
Range Selection
Range Comments
Semantic Selection
```

も同じ基盤を利用する。

---

# MVP

含める:

```text
SourceSpan型
byte-range attribution
rendered span attribution
wrap後もrange保持
range decoration
source view対応
rendered view対応
plain Markdown中心の正確なmapping
fixture / snapshot test
```

含めない:

```text
文字選択UI
マウスdrag
range comment
Atom snap
semantic selection
全Markdown構造の完全mapping
```

---

# 開発Phase

## Phase 1 — Range Attribution

現在の、

```text
source line attribution
```

を、

```text
source byte range attribution
```

へ拡張する。

既存表示は変えない。

---

## Phase 2 — Range Decoration

任意source spanへStyleを適用できるようにする。

fixtureで、

```text
[MARKED][NORMAL][DIM]
```

が同一行上に表示できることを確認する。

Semantic Reading Layerはここから利用可能。

---

## Phase 3 — Render Mapping強化

以下を重点的にテストする。

```text
emphasis
strong
link
list
blockquote
wrap
Japanese text
wide characters
```

table / codeは必要に応じて段階的に改善する。

---

## Phase 4 — Range Selection

必要になった時点で、

```text
mouse range selection
keyboard semantic selection
```

を検討する。

---

## Phase 5 — Range Comment

既存line commentと共存する形でrange commentを導入する。

---

# テスト項目

特に以下を重点的にテストする。

```text
ASCII
日本語
emoji
全角文字
Markdown emphasis
link
inline code
soft wrap
hard newline
list
blockquote
table
fenced code
```

byte offsetとterminal columnを混同しないことが重要。

---

# 設計原則

## Rangeは基盤、SelectionはUX

source spanを導入したからといって、UIまでrange中心にする必要はない。

```text
内部精度
≠
操作粒度
```

と考える。

---

## Line Selectionを壊さない

akapenの現在の軽いレビュー体験を維持する。

文字範囲選択は追加機能であり、必須操作にはしない。

---

## Semantic Reading Layerへ依存しない

Range Infrastructureはakapen本体の汎用機能。

Semantic Reading Layerがなくても価値を持つ設計にする。

---

# 成功条件

- 同一行内の任意範囲に別Styleを適用できる
- source view / rendered viewの両方でrangeが追跡できる
- 日本語やwide characterでも位置が崩れない
- Markdown記号が消えても本文範囲を追跡できる
- 既存の行選択UXを壊さない
- Semantic Reading Layerがrange decoration APIだけで実装できる

---

# 本改修の本質

これは文字選択機能の追加ではない。

> **akapen内部の「場所」の概念を、行からsource rangeへ精密化する基盤改修**

である。

まず内部モデルを高精度化する。

その上に、

```text
Semantic Reading Layer
Range Comments
Mouse Selection
Atom Selection
Precise Diff
```

を必要に応じて載せていく。