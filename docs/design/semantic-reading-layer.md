# Semantic Reading Layer 設計書

## 概要

AI生成文書は、内容が正しくても、長い・重複する・重要箇所が見えにくい、といった問題を持ちやすい。

Semantic Reading Layerは文書を書き換えず、文書を文の単位で見て、**いま読み手が問うていることに答えている箇所を視覚的に浮かび上がらせる**。

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

**「収穫が高い」を文書の側の属性として測るのはやめた**（2026-09-22）。
収穫は読み手が何を探しているかで決まるので、この層は問いを受け取って
それへの答えの強さを測る。

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

**段落に代えてJevの知覚したまとまりを単位にする形も、2026-09-24にやめた。**
いまの単位は文（散文のAtom 1つ）である（下の「Semantic Unit」）。

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

**データ行は1行ずつSemantic Unitになる**（文と同じ扱い。下の「Semantic Unit」）。
行ごとに問いへのスコアを聞き、答えている行だけが光る。

ヘッダ行を別の種別にしてあるのは、**ヘッダ行はUnitにならない**からである。列の
名前を挙げても中身を言ったことにはならない。見出しと同じ扱いになる。

区切り行（`| - | - |`）は独立したAtomにせず、ヘッダ行のAtomの末尾に含める。
含めないとそこがどのAtomにも属さず、行が沈んだときにその1行だけがNORMALで
光り残る。

セルまでは割らない。1行が問いに答えうる最小の単位で、セルは単独では読めない。

---

# Semantic Unit

**Semantic Unit = 散文のAtom 1つ**（文・リスト項目・引用・表のデータ行）。
見出し・コードブロック・表のヘッダ行はUnitにならない ＝ スコアを持たず、光らない。
境界はJevに聞かない。種別だけで決まるので、「Jevに判断させないもの」の側にある。

```text
Atom A    Atom B    Atom C      ← 散文なら、それぞれが Unit
  ↓         ↓         ↓
 Jev       Jev       Jev        ← いまの問いへの Noul を 1 つずつ
```

## 以前はJevに境界を聞いていた（2026-09-24まで）

Semantic Unitは「**Jevが現在の文書から知覚した意味的まとまり**」だった。
散文どうしの境界ごとに `SAME_UNIT` / `NEW_UNIT` をJevに聞いて束ね、光るUnitの中で
光らせる一文（核）をもう1度Choiceで選んでいた。Unitも境界の問いもDIM版の都合で
作ったもので、marksだけになってから測り直した
（[`unit-granularity.md`](../../examples/semantic/measurements/unit-granularity.md) /
[`core-question.md`](../../examples/semantic/measurements/core-question.md) /
[`unit-per-atom.md`](../../examples/semantic/measurements/unit-per-atom.md)）。

- 文ごとの方が正解ラベルの取りこぼしが少ない（核の問いを直した束ね方で8件、
  文ごとで0件）。束ね方に残った取りこぼしの大半は「1 Unitから光るのは1文だけ」の
  制限で、核の問いをどう直しても減らない
- 実物の文書で両者が食い違った印を人が判定すると、どちらか片方だけが光らせた印も
  全部「要る」だった。精度は同じ
- 費用は14%安く、ランごとの揺れも小さい（境界の揺れも核の揺れも無い）
- 弱点: 見出しの無い文書で、元は見出しだった行が光る率が上がる（7.3% → 12.8%）

**戻すと次のものが一緒に戻ってくる** — 1 Unit 1文の制限、光らせる一文を選ぶ
問い（その文面がいまの問いを見ていないと、numbersで光ったUnitでも「要点」の文が
光る）、問いをまたぐ境界のキャッシュと同時に走るプロセスの排他、つまみの既定の
測り直し。詳しくは判定器（`examples/semantic/jev-annotate.py`）の冒頭にある。

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

**いまは両者が一致している**（Unit = 散文のAtom 1つ）。プロトコルは複数のAtomを
持つUnitと、その中で光らせるAtom（`core_atoms`）も運べるが、判定器は使っていない
— 各Unitの `core_atoms` はそのAtom自身である。

表示位置についてはakapen側のRange Attribution基盤を利用する。

---

# 問いとスコア

**この層の機構は 1 つだけである。**

```text
問い = 定型（5 本）または 自由入力
  ↓
散文の Atom（= Unit）ごとに Noul を 1 ラウンド → score
  ↓
スコアの高い文から光らせる。量はつまみ
```

0〜100のimportance scoreは使用しない。Jevが返すのは、

> いま問われていることに、このUnitがどれだけ答えているか

という 0.0〜1.0 のNoulひとつである。**これは順位スコアではない** —
問いが変われば値も変わる。「重要度」という文書の側の属性を測っているのでは
なく、**読み手の問いとの距離**を測っている。

## 問いは定型と自由入力

定型は5本。文面の正本はakapen側の `assets/marks-questions.json` にあり、
判定器は文面を知らない汎用の器である（2か所に置くとずれる）。

```text
Essential   落とすと要点・結論・制約・未決を取り違える箇所
Settled     決定・合意・確定を述べている箇所
Unsettled   未決・保留・要確認を述べている箇所
Your call   読み手に判断・確認・選択を求めている箇所（id は decide）
Numbers     期限・金額・数量を含む箇所
Ask …       読み手が打った自由入力（型に差し込む）
```

文面の設計と実測は
[`marks-only-and-review-mode.md`](marks-only-and-review-mode.md) 0 節にある。
**文面を変えたら測り直すこと。**

## Jevへの問いは1段に保つ

**System One に寄せるのは、Jevにやらせる処理の側である。** 多段の推論や、
前の答えを次の問いの前提に差し込む連鎖はFastの仕事ではない。上の機構が
スコアの1ラウンドだけなのはそのためでもある（2026-09-24までは境界・スコア・核の
3ラウンドで、どれも1段だった）。

つまみ・UI・足切り・描画といった後処理はこの方針の対象外で、深さは別の理由で
決める。

## 核の一文 — もう聞かない（2026-09-24）

Unitが文1つになったので、光らせる一文は**スコアを付けたその文そのもの**である。
各Unitの `core_atoms` はそのAtomを明示する。

以前は、足切りを越えたUnitの散文Atomを選択肢にして「どれを光らせるか」をChoiceで
聞いていた。文面は2度変わった — 「1か所だけ読むならどこか」（導入文や節のラベルが
選ばれた）→ 損失ベースの「読み飛ばすと要点を失うのはどれか」（問いにかかわらず
固定で、numbersで光ったUnitでも「要点」の文を選んだ）→ いまの問いの文面を入れる
（取りこぼし14 → 8件）。残りは1 Unit 1文の制限で、そこでUnitごとやめた。

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

1つだけ。

```text
問いへのスコア          （この文は、いま問われていることにどれだけ答えているか）
```

2026-09-24までは「Atom間の意味境界」と「核の一文」もJevに聞いていた（上の
「Semantic Unit」「核の一文」）。

Jevには長い説明や要約を生成させない。

小さな意味判断を高速に、**1段で**行わせる。

例:

```text
ここは「まだ決まっていない事柄」を述べている？
```

---

# Jevに判断させないもの

ローカルコードで扱う。

```text
syntax parsing
Atom生成
Unitの組み立て（散文のAtom 1つ = Unit 1つ）
source position管理
ファイル変更検知
debounce
cache
rate limit
つまみ（上から何%）
足切り
表示状態への変換
renderer
```

---

# つまみ

つまみはユーザーが、

> この文書のどれだけを光らせるか

を指定する値。

```text
1% ～ 100%
```

1%刻みで変更可能とする。**％は文書の全Unit数 ＝ 散文のAtomの数に対してである**
（足切りを越えた数ではない。見出し・コードは数えない）。足切りを越えた数のほうを分母にすると、狭い問いで
「10本しか該当が無いのに20%で2本」になり、**つまみが該当の一部を隠す**。

## なぜ「％」で、「スコアの境目」ではないのか

実測（5文書×8問×2ラン）が2つのことを言っている。

- **「スコアの境目」は文書をまたがない。** 同じ「要点」でも0.5を超えるUnitは
  ある文書で171中15、別の文書で126中107
- **「上から N %」はまたぐ。** 上位20%のスコアは0.30〜0.82で、どの文書でも
  「上位20%」という指し方が成立する（この2つはJevが束ねたUnitで測った）

既定は**15%**（2026-09-24）。それまでの20%はJevが束ねたUnit（段落ほど）を分母に
していて、Unitを文にすると同じ20%で光る本数が1.2〜1.9倍になる。前の形の20%と
同じ本数になる値を6文書×5問の応答から数えると中央値15.3で、15%での本数の合計は
前の20%の98%（[`unit-per-atom.md`](../../examples/semantic/measurements/unit-per-atom.md)）。
ラン間の顔ぶれの一致は10〜20%のどれでもほぼ同じで、決め手にならなかった。

## 足切り

％だけだと、**狭い問い**（費用の話をしない文書に「費用の話」）で上からN本を
取ると雑音が光る。そこで全文書共通の定数 0.20 を足切りに置く。

**0本は正しい答えでありうる** —「この問いに答えている箇所が無い」という意味を
持つ。つまみの刻みで0本になるとその意味が混ざるので、％のほうは**最低1本**に
丸める。0本になるのは足切りを越えるUnitが無いときだけである。

足切りは**上限も作る**。つまみを100%まで上げても、該当の無いものは出てこない。

## つまみは残す

「スコアで並べて上からN本、閾値は要らない」という形もありうるが、**制御は
残す**。理由は3つ。

- 後で制御を外す決断をするとき、**制御があるほうがデバッグしやすい**。
  「この箇所が光る／光らない」の境目を動かして見られなければ、光り方の正否を
  判定器の側と表示の側に切り分けられない
- スコアは判定器が既に返している。保持するだけで、新しい問いは要らない
- 制御を外すのは「あったものを外す」決断で、「無かったものを足す」より安い

---

# マーカーの決め方

スコアと核から、現在のつまみでの表示状態をローカルに決定する。

```text
足切りを越え、かつ核を持つUnit  → 候補
候補をスコアの降順に並べる       （同点は文書順）
上から take = round(全Unit数 × つまみ%) 本、候補数で頭打ち
選ばれたUnitの核のAtomだけ       → MARKED
それ以外                         → NORMAL
```

いまの判定器では核がUnitのAtomそのものなので、選ばれた文がそのままMARKEDになる。

**判決を下さない。** 光るか、何も言わないか、だけである。

## 単調性

つまみを上げたとき、光っていたUnitが消えることはない。選ぶ順はスコアの降順で
固定なので、集合は**入れ子で広がる**。足切りはつまみの関数ではないので、この
性質を壊さない。

## キャップを掛けない

1本のリストの項目が全部光るのは「答えが全部光る」で正しい。**問いが既に
選んでいる**ので、量はつまみが受け持つ。

---

# 表示状態

3値:

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

**この層は返さない。** 読み手が自分でフォーカス（`f`）を押したときに、
クライアントが光っていない箇所へ掛ける描画である。

**フォーカスは「光った文と見出しのほかを沈め、琥珀の線を消す」**（2026-09-24、
読み手の決定）。光った文はふつうの明るさで残り、溝の目盛りと `]m` は沈めている間も
残る。Unitが文になって「光ったUnitの核でない文を沈めない」が空振りしていた既知の
劣化は、この形で整理し直した（詳しくは
[`marks-only-and-review-mode.md`](marks-only-and-review-mode.md) のフォーカスの節）。

DIMはverdict（外れると読み手が信頼を払う）で、MARKEDはoffer（外れても損を
しない）である。この層はofferしか出さない。

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

# つまみの操作ではJevを呼ばない

一度Semantic Annotationが得られれば、

```text
20 → 21 → 22
```

という操作は完全にローカルで行う。フォーカス（`f`）も同じで、投影が
変わるだけである。

```text
Jev analysis
     ↓
score / core_atoms
     ↓
マーカーの決め方
     ↓
つまみ 1〜100%
     ↓
View Decoration
```

## 遅延

解析は文書を開いた時点では走らせない。**問いを決めた最初の1打**
（`m` / `M` / `/`）が起点で、そこまで判定器を1度も呼ばない。素で読むだけの
文書に解析1回分を払わないためで、2度目以降は文書と問いが変わっていなければ
キャッシュに当たる。

**つまみは起点にならない。** 問いの無い解析はこの投影では使えないので、
問いを決めていない状態でつまみを回しても判定器は起きず、読み出しが理由を言う。

## 判定器は問いをまたいだ状態を持たない

問いを変えるとakapen側のキャッシュ（コマンド行・文書・問いで引く）は外れ、
判定器がもう一度起きる。いまの判定器が聞くのはその問いへのスコアだけなので、
問いをまたいで持ち越すものが無い。

2026-09-24までは境界の答えを問いをまたいでキャッシュし
（`~/.cache/akapen/semantic/boundaries/v1/`）、同時に起きたプロセスどうしは
その隣の`<digest>.lock`の`flock`で境界のラウンドを1本に絞っていた。判定器は
もうそこを読みも書きもしない。古い項目が残っていても害は無く、
`akapen --semantic-cache-clear` で消える。

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
Semantic Units（散文の Atom 1 つずつ）
       ↓
問い + Jev Noul
       ↓
score / core_atoms
       ↓
マーカーの決め方
       ↓
つまみ 1-100% と足切り
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
Semantic Unit（散文の Atom 1 つ）
問いへのスコア
つまみ 1〜100% と足切り
MARKED / NORMAL（DIMはフォーカスの描画）
akapen integration
cache（実装済み）
incremental reanalysis
```

含めない:

```text
importance 0〜100 score
Reading Tier の4段
Reading Budget と判決
redundancy / 前提の閉包
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

**2026-09-22に「含めない」側へ移したものがある** — Reading Tier・Reading
Budget・redundancy・前提の閉包。理由は
[`../gotchas/semantic-reading.md`](../gotchas/semantic-reading.md)。

---

# 最初のデモ

AI生成Markdownをakapenで開く。`m` で問いを選ぶ。

```text
Essential · 13 · 15%
```

問いに答えている文が上から15%だけ光る。

つまみを上げる。

```text
Essential · 26 · 30%
Essential · 44 · 50%
Essential · 61 · 100%
```

光る箇所が**入れ子で広がる**。一度光ったものは消えない。

問いを変える（`m` / `/`）。

```text
Unsettled · 12 · 15%
Ask: 費用の話 · 4 · 15%
```

**違う場所が光る。** 文書は1バイトも変わっていない。

`f` を押すと、光っていない箇所が沈む。

元文書は変化しない。

つまみの操作ではJevを呼ばない。

---

# 成功条件

問いに対して、

- gist
- conclusion
- decision
- requirement
- critical rationale

のうち**その問いが指すもの**が光り、指さないものが光らないこと。

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

> **現在の文書を見て、一文ずつ「いま問われていることへの答え」をぱっと知覚するSystem 1的センサー**

として使う。

Semantic Reading Layerはその判断を、人間のattentionへ変換する。

> **Document stays fixed. Attention moves.**
