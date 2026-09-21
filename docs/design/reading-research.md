# 読みの研究 — 「ここ読んで」は何が伝えているのか

設計書（`semantic-reading-layer.md`）は Duggan & Payne の satisficing を引いて
いる。**この文書はその周りを調べた結果の保管庫**である。実装の指示ではない。

調べた動機ははっきりしている。**見出しの拾い読みは Jev を呼ばなくても効く。**
`#` を見て段落の先頭を読む、あれである。ところが web からエクスポートした
Markdown は見出しが適切に打たれていないことがある。**単なるテキストが流し
込まれても、ある程度は「ここ読んで」が伝わるようにしたい。**

---

## 一言でいうと

**読み手は「ここ読んで」がどこにあるかを既に知っていて、書き手はそこに置く。**

見出し、段落の頭、文書の頭、語彙の切り替わり、主節と従属節。拾い読みは
**慣習への賭け**であり、その賭けはだいたい当たる。ニュース要約で LEAD-3
（先頭 3 文をそのまま要約とする）が学習済みモデルを上回ってしまうのは、
位置に魔法があるからではなく、**書き手が前に寄せて書くから**である。

だからユーザーの直感 —「見出し拾い読みは Jev なしでも効く」— は正しい。
そして**一般化する**。見出しは合図の一族の 1 つにすぎない。

## 合図の一族

| 合図 | 見出しの無い平文で何が担うか | 出典 | akapen の現状 |
| --- | --- | --- | --- |
| 構造（見出し） | `#`。粗いエクスポートでは欠ける | Lorch | `boundary_rule` の規則 1 / 2 |
| **位置** | **段落の先頭の Atom / 文書の先頭の段落** | Duggan & Payne 2011, LEAD-3 | **使っていない** |
| 語彙の切り替わり | 連続するブロック間の語彙の重なりの落ち込み | TextTiling (Hearst 1997) | 使っていない（下記の理由で採らない） |
| 核性 | 主節 / 従属節、「つまり」「要するに」「結論」 | Rhetorical Structure Theory | 使っていない |

**いちばん近い空白は「位置」である。** 段落の先頭という合図を akapen は
1 つも使っていない。見出しが無い文書でも、段落は空行で残る。

---

## 各論

### 1. 合図（signaling）は内容を足さずに構造を伝える

Lorch の整理では、**signal** とは「内容を足さずに、内容や構造の一部を
強調する書き方の装置」であり、タイトル・見出し・予告・概観・要約・
活字上の手がかり・番号・重要性の指示などが含まれる。

実験の結果は一貫している。合図があると**話題とその組織の記憶が良くなる**。
興味深いのは内訳で、**読み手はより多くの話題を思い出すが、1 つの話題に
ついて思い出す量は減る**。合図は「文書の話題構造の表象」を作り、その表象が
想起を導く、というモデルで説明されている。

これは akapen の立場とよく合う。**MARKED は内容を足さない**（設計書
「文字を追加せず、背景・明度・modifier 等で静かに表現する」）。akapen が
やっているのは、**書き手が置き損ねた signal を後から足すこと**である。

- Lorch, R. F. (1989). Text-signaling devices and their effects on reading and
  memory processes. *Educational Psychology Review*, 1(3), 209–234.
  <https://link.springer.com/article/10.1007/BF01320135>
- Lorch & Lorch. Effects of headings on text processing strategies /
  text recall and summarization（見出しの効果の一連の実験）
- Hyönä ら. Effects of topic headings on text processing: evidence from adult
  readers' eye fixation patterns（見出しが視線に与える影響）
  <https://www.sciencedirect.com/science/article/abs/pii/S0959475204000039>

### 2. 拾い読みは satisficing で、**段落の先頭に時間を使う**

設計書が引いている Duggan & Payne の続き。眼球運動の実測で、拾い読みの
読み手は

- **段落の**先頭に近い行ほど長く読む
- ページの上のほうを長く読む
- 文書の前のほうを長く読む
- 後ろの行ほど**そもそも注視されない**

拾い読みそのものの特徴は、**サッカードが長く、注視が短く、飛ばす語が多く、
総読書時間がページ全体に平均的にばらける**こと。

**ここが akapen にとっていちばん実用的な発見である。** 「段落の先頭」は
見出しが無くても残る合図で、しかも構文だけで取れる。Jev に聞く必要がない。

- Duggan, G. B., & Payne, S. J. (2011). Skim reading by satisficing:
  Evidence from eye tracking. *CHI '11*.
  <https://www.researchgate.net/publication/221518541_Skim_reading_by_satisficing_Evidence_from_eye_tracking>
- One page of text: Eye movements during regular and thorough reading,
  skimming, and spell checking. <https://pmc.ncbi.nlm.nih.gov/articles/PMC7198234/>

### 3. 核性（nuclearity）— 落としてよい部分は文法に出る

Rhetorical Structure Theory は文書を、節相当の単位（EDU）を葉とする木で
表す。多くの関係は **nucleus（核）と satellite（衛星）**に分かれ、

> 衛星は核なしでは意味を成さないことが多いが、**衛星を削った文書は
> ある程度理解できる**。

要約研究は長くこれを使ってきた。**核は要約に入りやすく、衛星は入りにくい。**

これは設計書の Reading Tier と**同じことを別の語彙で言っている**。
ESSENTIAL / SUPPORTING / CONTEXT / DETAIL の 4 段は、nuclearity の連続量を
4 つに丸めたものだと読める。**akapen が Jev に聞いているのは、要するに
「この Atom は核か衛星か」である。**

言い換えると、**Jev は RST パーサの代わりに使われている**。RST パーサを
自前で持たない代わりに、System One の判断を借りている。これは悪い取引では
ない — RST の自動解析は難しく、日本語ではなおさらである。

- Mann & Thompson (1988). Rhetorical Structure Theory.
  <https://en.wikipedia.org/wiki/rhetorical_structure_theory>
- Discourse-Aware Neural Extractive Text Summarization
  <https://arxiv.org/pdf/1910.14142>

### 4. 位置は強い。**ただしニュースの形をしている**

LEAD-3（先頭 3 文）はニュース要約で学習済みモデルを上回り続けている。
PacSum の分析では、**選ばれた文の 82.3% が先頭 3 文の中**だった。

**この数字をそのまま持ち込んではいけない。** CNN/DailyMail の性質である。
ブログのエクスポート、議事録、`CLAUDE.md` — どれも文書の頭に全部を
前置きしない。

**一般化するのは段落レベルのほう**（Duggan & Payne の「段落の先頭の行」）で
あって、文書レベルではない。**「文書の先頭 N 文は ESSENTIAL」は採らない。**
3 段落目から DIM が壊れる。

- Sentence Centrality Revisited for Unsupervised Summarization (PacSum)
  <https://arxiv.org/pdf/1906.03508>

---

## 採らなかったもの

### TextTiling — 教科書的な答えだが、この実装には合わない

Hearst の TextTiling は、見出しの無い平文を**語彙の結束性**だけで話題の
まとまりに切る古典である。連続するブロックの語彙の重なりを計算し、
その谷を境界とする。見出しが無い文書に構造を与える手としては教科書的で、
**この課題に対する「普通の答え」はこれ**である。

**それでも採らない。理由は 3 つ。**

1. **境界はユーザーの困りごとではない。** 見出しが無い文書にも**段落は
   残る**（空行）。akapen が困るのは境界ではなく、**見出しが無いときの
   Tier** である
2. **トークナイザが要る。** TextTiling はトークン列上のスライド窓で働く。
   日本語には形態素解析（MeCab 等）が要り、`crates/semantic-reading` が
   避けてきた依存を持ち込む
3. **Atom の作り方と噛み合わない。** akapen の Atom は Markdown の構文
   （pulldown-cmark → 文）から作る。語彙の窓はこの単位を無視する

**次に誰かが提案したときのために書いている。** 悪い案ではない。この実装に
合わないだけである。

- Hearst, M. A. (1997). TextTiling: Segmenting text into multi-paragraph
  subtopic passages. *Computational Linguistics*.
- Recent Trends in Linear Text Segmentation: a Survey
  <https://arxiv.org/pdf/2411.16613>

### 文書レベルの LEAD

上記のとおり、ニュースの性質である。段落レベルだけを採る。

---

## いちばん近い先行システム — Scim (IUI 2023)

論文を拾い読みするために、**重要な箇所をハイライトして注意を誘導する**
インタフェース。akapen と課題がほぼ同じで、**独立に同じ結論に着いている**。

彼らの設計目標を逐語で:

> highlights should be **simultaneously diverse, evenly-distributed, and
> important**

**「ミニマルに、これさえ見れば」＋「散らばっていてほしい」を 1 行で言って
いる。** akapen が 2026-09-21 に run キャップで実測から辿り着いたのと同じ
ところに、彼らは形成的調査から着いている。

### 数字と、そのまま効く教訓

| 彼らが見つけたこと | akapen での対応 |
| --- | --- |
| **ハイライトは段落 1 つにつき 1 つ程度**（設計目標 D5） | run キャップ（リスト 1 本につき核 1 つ）と同じ発想。**段落版はまだ無い** |
| 長くハイライトの無い区間があると、読み手は**落ち着かない**（そこに重要な情報がある気がする） | akapen は逆側（多すぎ）だけを問題にしてきた。**少なすぎ・偏りも問題**という視点が無かった |
| 重要度どおりに光らせることと、望ましい分布にすることは**両立しないことがある**（明示された tension） | **akapen の MARKED は重要度だけで決まる。分布の制約が無い** |
| 均等化は**後処理**で入れた —「まだハイライトを含まない段落の中の文を優先する」 | `policy::decorate` の後段に同じ形を置ける |
| 分類を外すと**信頼が落ちる**（設計目標 D6）。期待と食い違うと読み手は懐疑的になる | 「これさえ見れば」の位置にラベルが光る問題と同じ |
| **箇条書きの中の予期しないハイライト**が具体的な不満として挙がっている | `docs/gotchas.md`「箇条書きのラベルが MARKED になる」と**同じ現象**。向こうも解けていない |
| **AI のハイライトと、著者自身の強調（太字・箇条書き）の食い違い**が混乱を生む | 同上。**著者の強調を尊重するか上書きするかは、まだ決めていない** |
| 日誌調査で 105 件中 74 件（**70.4%**）が「拾い読みに役立った」 | 比較できる数字を akapen は持っていない |
| 密度は**全体と局所の両方で可変**にした。「粗い選択肢がほしい」という声 | Reading Budget がこれに当たる |

**いちばん効く指摘は「分布」である。** akapen の MARKED は重要度だけで
決まっていて、**文書の中でどう散らばるかを誰も見ていない**。run キャップは
リスト 1 本の中の分布を制約したが、それは偶然そうなっただけで、
**段落や文書のレベルの分布は制約していない**。

- Scim: Intelligent Skimming Support for Scientific Papers. IUI 2023.
  <https://arxiv.org/abs/2205.04561>

---

## akapen にとって、次に測れること

**見出しを剥がした文書で今どれだけ劣化するかは、Jev を呼ばずに測れる。**

5 つの公開文書から見出し記号（`^#+ `）を落とし、現行のパイプラインを通して、
**Tier の分布と MARKED の位置**を見出しありの版と比べる。これが「web から
エクスポートした Markdown」の模擬であり、**akapen が今どれだけ壊れるかの
実測**になる。API を呼ぶのは Tier を見るときだけで、構造の変化は無料で見える。

その数字が出て初めて、「段落の先頭に事前分布を置いたら戻るか」が問える。

**置くとしても、Tier の上書きではない。** 設計書「Jev に判断させないもの:
syntax parsing」と同じ線で、位置は**構造の側の情報**である。ラウンド 3 の
Choice の候補の並べ方、あるいは同点の場合の決め手として入れるのが筋で、
Tier そのものを位置で決めると、Jev に判断させている意味が無くなる。

**比較は必ず同じラン数で。** `docs/gotchas.md`「退行の判定」。

---

## この文書の使い方

**実装の指示ではない。** 次に「見出しが無い文書をどうするか」「MARKED の
分布をどうするか」を考える人が、**同じ検索をやり直さずに済むため**にある。

採らなかったものには理由が書いてある。採る側に回すときは、その理由が
まだ成り立つかを先に確かめること。
