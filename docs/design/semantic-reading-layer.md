# Semantic Reading Layer 設計書

## 概要

AI生成文書は、内容が正しくても、長い・重複する・重要箇所が見えにくい、といった問題を持ちやすい。

Semantic Reading Layerは文書を書き換えず、文書内の意味的なまとまりと読む優先度を推定し、**Reading Budgetに応じて読むべき部分を視覚的に浮かび上がらせる**。

> Document stays fixed. Attention moves.

---

# スコープ

初期対象は単一のテキストファイル。

対象:

- Markdown
- README
- 設計書
- 仕様書
- メモ
- AI生成文書
- プレーンテキスト

対象外:

- 書籍
- 小説
- 複数ファイル横断
- リポジトリ全体
- RAG
- ナレッジベース

---

# 基本思想

Semantic Reading Layerは要約器ではない。

ユーザーが文章を拾い読みするときの、

> ここは読む  
> ここは飛ばしてもよさそう  
> ここは前と同じ話  
> ここを落とすと意味が変わる

という判断を支援する。

---

# 研究的背景

Skimming研究では、読者は文章全体を均等に高速で読むのではなく、読む場所を選択している。

Duggan & Payneのsatisficingモデルでは、読み進めることで得られるinformation gainが低下すると、読者は次の場所へ移動する。

Semantic Reading Layerでも、

> **限られたattentionを、意味的な収穫が高い部分へ配分する**

という考え方を採用する。

また、文書全体におけるstructural importanceや、既読内容とのredundancyも考慮する。

---

# Document Profileを作らない

以下のような事前Profileは生成しない。

```yaml
purpose:
document_type:
main_topics:
key_questions:
```

文書の目的や構造は執筆途中でも変化する。

Jevには、

> 文書について説明してから判断させる

のではなく、

> **現在の文書そのものを見て判断させる**

ことを基本とする。

---

# 全文をContextとして扱う

単一ファイルかつ現実的なサイズである限り、全文をJevのcontextへ渡すことを第一候補とする。

```text
Current Document
      +
Target
      ↓
     Jev
```

必要になった場合のみwindowing等を導入する。

Document Profileやsummary treeを最初から構築しない。

---

# ParagraphをSemantic Unitにしない

Markdown上の段落は意味上のまとまりとは限らない。

AI生成文書では、

- 不自然な改行
- 不自然な空行
- 一段落に複数の話題
- 一つの意味が複数段落に分裂

が起こり得る。

したがって、

```text
paragraph ≠ semantic unit
```

とする。

---

# Atom

文書をまず機械的な最小単位へ分割する。

これをAtomと呼ぶ。

例:

```text
sentence
list item
heading
code block
table
blockquote
```

Atomは意味単位ではない。

> **安全に位置を指定できる機械的単位**

である。

## 表は行へ割る

表だけは「1ブロック1Atom」の例外で、行まで割る。

```text
table       ← ヘッダ行（＋区切り行）。表の枕
table row   ← データ行1行
```

表全体を1つのAtomにすると、表示単位がAtomである以上「表まるごと光る / 表まるごと
沈む」しか選べない。marks モードが売りにしているのは一文の精度なので、そこまで
下りる。

**割るのは表示と核のためであって、まとまりを崩すためではない。** 表は1つの
Semantic Unitのままである。行に下りるのは核だけで、「この表から1行だけ読むなら
どれか」を聞く。

ヘッダ行を別の種別にしてあるのは、**ヘッダは核にならない**からである。列の名前を
挙げても中身を言ったことにはならない。見出しと同じ扱いになる。

区切り行（`| - | - |`）は独立したAtomにせず、ヘッダ行のAtomの末尾に含める。
含めないとそこがどのAtomにも属さず、行が沈んだときにその1行だけがNORMALで
光り残る。

セルまでは割らない。1行が「1か所だけ読むならどこか」の答えになる最小の単位で、
セルは単独では読めない。

---

# Semantic Unit

Atom間の境界をJevに判断させる。

```text
Atom A ──?── Atom B ──?── Atom C
          ↑             ↑
         Jev           Jev
```

判断例:

```text
SAME_UNIT
NEW_UNIT
```

Semantic Unitとは、

> **Jevが現在の文書から知覚した意味的まとまり**

である。

---

# 判断単位と表示単位

意味判断:

```text
Semantic Unit
```

表示:

```text
Atom
```

Semantic Unitに付与した意味情報を、そのUnitを構成するAtomへ投影する。

表示位置についてはakapen側のRange Attribution基盤を利用する。

---

# Reading Tier

0〜100のimportance scoreは使用しない。

Semantic Unitには意味のある粗い段階を付与する。

```text
ESSENTIAL
SUPPORTING
CONTEXT
DETAIL
```

## ESSENTIAL

落とすと文書の要点、結論、制約、未決の論点や宿題などを取り違える可能性が高い。

## SUPPORTING

ESSENTIALの理解・納得に役立つ。

## CONTEXT

背景や前提、理解補助。

## DETAIL

例、細部、追加説明。

---

# Redundancy

重複はReading Tierとは別軸にする（弱まるのは対のうちTierが低い方、同Tierなら長い方）。

```text
reading_tier = SUPPORTING
redundant_with = u3
```

のような状態を許す。

つまり、

> 意味上は重要だが、すでに得た情報なので追加information gainが低い

という状態を表現できる。

MVPで必要なrelationはまず、

```text
REDUNDANT_WITH
```

のみとする。

---

# Role

必要であれば、

```text
decision
requirement
conclusion
rationale
risk
evidence
example
background
transition
other
```

などのroleを保持できる。

ただしMVPで必須とはしない。

---

# Jevに判断させるもの

主に以下。

```text
Atom間の意味境界
Reading Tier
semantic redundancy
必要ならrole
```

Jevには長い説明や要約を生成させない。

小さな意味判断を高速に行わせる。

例:

```text
ここを飛ばすと要点を失う？
これは主要な主張を支えている？
これは主に背景説明？
これは前に出た内容と実質同じ？
```

---

# Jevに判断させないもの

ローカルコードで扱う。

```text
syntax parsing
Atom生成
source position管理
ファイル変更検知
debounce
cache
rate limit
Reading Budget
Reading Policy
表示状態への変換
renderer
```

---

# Reading Budget

Reading Budgetはユーザーが、

> この文書にどれだけ注意を使えるか

を指定する値。

```text
1% ～ 100%
```

1%刻みで変更可能とする。

例:

```text
READ 100%
READ 73%
READ 42%
READ 17%
READ 1%
```

Reading Tierは粗い意味分類だが、Budget操作は細かい。

したがって、

```text
43%
42%
41%
```

で表示が変わらなくても問題ない。

## Budgetの下限

Reading Policyは核とその前提をBudgetを見ずに残す。したがって文書ごとに、
**そこから下ではBudgetを下げても表示が変わらない値**がある。これが下限。

```text
READ 43% (floor)
```

下限は**その文書の測定値であって、閾値や設定ではない**。核と前提のバイト数を
全体で割って切り上げた整数で、定数はどこにも無い。下限が高い文書は、
核が多いか、核の前提の系譜が長い文書である。

クライアントはBudgetを下限より下へ回さない。目的は**数字と画面の量の一致**で、
READ 1 %と言いながら4割を出す表示を許さない。予算外で戻る見出しの行だけは
上乗せになる。

---

# Reading Policy

Reading Tierとrelationを元に、現在のBudgetでの表示状態をローカルに決定する。

基本:

```text
ESSENTIAL
→ 最後まで残りやすい

SUPPORTING
→ 次に残る

CONTEXT
→ Budget低下時にDIM

DETAIL
→ 早めにDIM

REDUNDANT
→ 対の負けた側が、元TierにかかわらずDIM候補
```

ただし**核は奪わない** — 冗長な対に負けても、ESSENTIALで核を持つUnitからは
核を取り上げない。核はBudgetを見ずに残す側にあるので、負けても沈まない。
Reader Personaを含めないと決めている以上、読み手によって重要度が逆転する対で
どちらか一方の核を切る規則は持てず、冗長の効き先は実効Tierまでに留まる。

**見出しは中身に付いてくる** — 節の中のUnitが1つでも残るなら、その節の
見出しの行も残す（入れ子は親へ連鎖する）。逆は成り立たず、節ごと沈むなら
見出しも沈む。これは新しい原理ではなく、境界規則2「見出しは直後の内容に付く」を
箇条書きが項目ごとに割れたあとまで運ぶための言い直しで、判定器が節の境界を
属性（`section_of`）として渡し、Policyは予算の取り合いのあとに見出しの行だけを
戻す。戻した行はBudgetに数えない（「Budgetの下限」の上乗せがこれ）。

同じTier内部では、

- redundancy
- length
- document position
- context preservation

などの決定論的なruleをまず利用する。

Jevに精密な順位スコアを出させない。

---

# 表示状態

MVP:

```text
MARKED
NORMAL
DIM
```

## MARKED

読む価値の高いAtom。

薄いsemantic markerを本文上に重ねる。

## NORMAL

通常表示。

## DIM

現在のBudgetでは優先度が低いAtom。

文書から削除はしない。

---

# Semantic Marker

重要部分をgutterの左線で示す方式は採用しない。

表示はAtomに対するrange decorationとする。

```text
DIM ───── NORMAL ───── MARKED
```

文字を追加せず、背景・明度・modifier等で静かに表現する。

---

# Budget変更ではJevを呼ばない

一度Semantic Annotationが得られれば、

```text
37 → 36 → 35
```

という操作は完全にローカルで行う。

```text
Jev analysis
     ↓
Reading Tier / Relations
     ↓
Reading Policy
     ↓
Budget 1〜100%
     ↓
View Decoration
```

## 遅延

解析は文書を開いた時点では走らせない。Reading Budgetキー（`-` `+` `<` `>`）の
**最初の1打**が起点で、そこまで判定器を1度も呼ばない。素で読むだけの文書に
解析1回分を払わないためで、2度目以降は文書が変わっていなければキャッシュに
当たる。

---

# 編集時

キー入力ごとにJevを呼ばない。

```text
typing
typing
typing
   ↓
 idle
   ↓
reanalysis
```

AI生成時はstreaming完了後に解析する。

変更箇所周辺のみ再解析し、大きな構造変更時のみ全文再解析する。

---

# Architecture

```text
Single Text File
       ↓
Syntax Parser
       ↓
Atoms
       ↓
Jev Boundary Detection
       ↓
Semantic Units
       ↓
Jev Semantic Judgement
       ↓
Reading Tier / Relations
       ↓
Reading Policy
       ↓
Reading Budget 1-100%
       ↓
Atom Decorations
       ↓
Client Renderer
```

---

# Clientとの境界

Semantic Engineは、

```text
どのsource rangeが
どのsemantic stateか
```

までをクライアントへ渡す。

具体的な、

```text
色
background
underline
dim
```

はclient側で決定する。

akapenでは別途定義する **Range Attribution / Selection基盤** を利用する。

---

# akapen

akapenをReference Clientとする。

Semantic Reading Layer側から必要なのは、

```text
Atom
↓
source byte/span
↓
akapen range decoration
```

というインターフェースのみ。

source/render mapping、range selection、mouse selection等はakapen側の責務とする。

---

# MVP

含める:

```text
単一テキストファイル
Atom
Semantic Unit
Jev boundary detection
Reading Tier
REDUNDANT_WITH
Reading Budget 1〜100%
MARKED / NORMAL / DIM
akapen integration
cache（実装済み）
incremental reanalysis
```

含めない:

```text
importance 0〜100 score
Document Profile
RAG
embedding DB
複数ファイル
書籍
自動書き換え
Reader Persona
range selection UI
Google Docs
agent auto-feedback
```

---

# 最初のデモ

AI生成Markdownをakapenで開く。

```text
READ 100%
```

全文を表示し、ESSENTIALな部分を薄くマーキングする。

Budgetを下げる。

```text
READ 70%
READ 50%
READ 30%
READ 10%
```

低優先度や重複部分が徐々にDIMになる。

元文書は変化しない。

Budget操作ではJevを呼ばない。

---

# 成功条件

低いReading Budgetでも、

- gist
- conclusion
- decision
- requirement
- critical rationale

を失いにくいこと。

ユーザーが、

> 要約された

ではなく、

> **元の文章のまま、読む場所が見える**

と感じること。

---

# 本機能の本質

Jevは、

> 文書を書き換えるAI

ではない。

> **現在の文書を見て、意味のまとまりと読む優先度をぱっと知覚するSystem 1的センサー**

として使う。

Semantic Reading Layerはその判断を、人間のattentionへ変換する。

> **Document stays fixed. Attention moves.**