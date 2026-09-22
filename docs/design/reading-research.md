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
| **箇条書きの中の予期しないハイライト**が具体的な不満として挙がっている | `docs/gotchas/semantic-reading.md`「箇条書きのラベルが MARKED になる」と**同じ現象**。向こうも解けていない |
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

> **2026-09-21 に測った。結果は
> `examples/semantic/measurements/headless-documents.md`**（対照 3 対 + web からエクスポート
> された実記事 3 本、各 4 ラン）。**数字はそちらを見ること。** 以下はそのときの
> 計画で、残してあるのは「何を測ろうとしたか」のためである。
>
> ここで書いた見立てのうち、**当たらなかったものが 1 つある**。
> 「見出しが無いと劣化する」を MARKED 比率で捕まえようとしたが、**比率は動か
> なかった**。壊れ方は比率ではなく**光る場所の入れ替わり**に出る。そして
> 「段落の先頭」の事前分布は、**設計書が置いてよいとした場所（核の Choice）で
> 測ると偶然とほとんど差が無い**。採らない側の材料が揃っている。

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

**比較は必ず同じラン数で。** `docs/gotchas/semantic-reading.md`「退行の判定」。

---

## 冗長な対のどちらを残すか — 「前を残す」は読みの原理か（2026-09-22）

### 問い

akapen の redundancy 規則は「これより前の箇所を言い直しているだけか」を Noul で
聞き、当たった Unit を `weakened()` で 1 段落とす。**前を残して後ろを弱める。**
Duggan & Payne の satisficing（読み進めて収穫が増えなくなったら次へ）を写した
もので、`policy.rs` のモジュールドキュメントと `jev.md`「redundancy の question
は方向を持つ」がその根拠を書いている。

実文書で観察された効き目は 1 つだけだった。**業務議事録の末尾の決定事項リスト
が、本文節の言い直しと判定されて沈み、長い本文節が残る**（8 ラン中 8 ランで
REDUNDANT。`examples/semantic/measurements/core-selection.md`）。判定は正しい。
それは本当に言い直しである。しかし議事録を開く人の多くは決定事項リストを読んで
本文を飛ばす。**いまの規則は、その読み手がいちばん欲しい部分を取り上げている。**
順読みの読み手には「前を残す」が正しいので、問いは「どの読み手のための規則か」
になる。

以下、4 つの角度から一次資料を確かめた。**確認済み**はこのセッションで書誌と
要旨（または本文）を一次資料か publisher の頁で確かめたもの、**未確認**は書誌
だけ、あるいは二次資料経由のもの。

### 1. 要約と本文、どちらが記憶に残るか — 要約が勝つ（確認済み）

Reder & Anderson は大学教科書の章と、その主旨を伝えるために作った要約を比べ、
**一貫して要約のほうが主旨の記憶に優る**ことを報告している。優位は 20 分後、
1 週間後、6〜12 か月後の保持でも維持され、関連する新しい材料を学ぶときの転移
でも要約が勝った。続報（1982）では原因を 2 つに分け、**細部が無いこと**と
**主旨を間隔をおいて読み返せること**が、独立に、どちらも中心的な考えの保持を
上げると結論している。

- Reder, L. M., & Anderson, J. R. (1980). A comparison of texts and their
  summaries: Memorial consequences. *Journal of Verbal Learning and Verbal
  Behavior*, 19(2), 121–134.
  <https://www.sciencedirect.com/science/article/abs/pii/S002253718090122X>
  （書誌は Crossref で、要旨は publisher の頁で確認。本文 PDF は取れなかった）
- Reder, L. M., & Anderson, J. R. (1982). Effects of spacing and embellishment
  on memory for the main points of a text. *Memory & Cognition*, 10(2), 97–102.
  <https://pubmed.ncbi.nlm.nih.gov/7087784/>

Kintsch & van Dijk のモデルでは、要約は**マクロ構造そのもの**である。理解の過程で
マクロ演算子（削除・一般化・構成）がテキストベースを gist に縮め、想起と要約は
その gist から生成される。書き手が末尾に置いた決定事項リストは、**書き手がすでに
マクロ演算を済ませた出力**であって、本文の「言い直し」ではなく本文の上位構造で
ある。

- Kintsch, W., & van Dijk, T. A. (1978). Toward a model of text comprehension
  and production. *Psychological Review*, 85(5), 363–394.
  <https://www.cl.cam.ac.uk/teaching/1516/R216/Towards.pdf>

**位置（冒頭か末尾か）はどうか。** Ausubel の advance organizer は、本文の前に
上位概念を置くと学習と保持が上がることを示した（金属学の 2,500 語の文章、
500 語の organizer 対 500 語の歴史的前置き。organizer 群が有意に上）。ただし
これは「先に読むと効く」であって、「先にあるほうを残せ」ではない。

- Ausubel, D. P. (1960). The use of advance organizers in the learning and
  retention of meaningful verbal material. *Journal of Educational Psychology*,
  51(5), 267–272. <https://psycnet.apa.org/record/1962-00294-001>
  （書誌は確認。実験の数字は二次資料経由）

Hartley & Trueman は要約の位置を直接比べた 5 実験の報告を出している。二次資料は
「**前に置いても後に置いても効果は同程度**」と要約しているが、**本文には当たれ
なかった**。位置が効かないなら、「前を残す」の根拠は読み手の記憶の側には無い。

- Hartley, J., & Trueman, M. (1982). The effects of summaries on the recall of
  information from prose: Five experimental studies. *Human Learning*, 1, 63–82.
  **未確認**（書誌は複数の引用で一致。位置の結論は二次資料のみ）

**akapen にどう効くか。** 冗長な対のうち、**短くて細部の無い側**（= 要約側）を
読ませたほうが主旨の記憶には効く、というのが文献の向きである。いまの規則は
逆側を残す。ただし文献は「要約を*読む*」実験であって「本文を DIM にする」実験
ではない。DIM にした本文は消えていないので、そのまま持ち込むのは飛躍がある。

### 2. 自動要約の冗長処理は位置で決めない — MMR（確認済み）

Carbonell & Goldstein の MMR は、候補 D_i の得点を
「λ·Sim1(D_i, Q) − (1−λ)·max_{D_j∈S} Sim2(D_i, D_j)」で定め、**既に選んだ集合 S
との類似が高い候補を、質問 Q への関連度で相殺して**貪欲に選ぶ。位置は式のどこ
にも無い。単一文書の要約は「文書を文に切り、MMR で並べ替え、上位を**元の文書
順に並べて**提示」している。位置は最後の表示順にだけ使われる。

論文は要約についてこう書いている。長い文書は「abstract, introduction,
conclusion, results といった節をまたいで**内在的な passage の冗長性**を含む」
ので MMR が効く、と。**akapen が議事録で見た対（本文節と末尾の決定事項リスト）は、
まさにこの「節をまたぐ冗長」である。** MMR ならこの対のどちらが残るかは、Q への
関連度で決まる。Q が無ければ（λ=1 の関連度のみ、あるいは中心性）文書全体との
類似が高い側が残り、それは多くの場合**要約側**である。

- Carbonell, J., & Goldstein, J. (1998). The use of MMR, diversity-based
  reranking for reordering documents and producing summaries. *SIGIR '98*,
  335–336. <https://dl.acm.org/doi/10.1145/290941.291025>
  （本文 PDF を確認。式と要約の節を読んだ）

**akapen にどう効くか。** akapen の「前を残す」は、Noul の文面に「これより
**前**の箇所を」と入れた結果として出てくる。方向を入れた理由は
`jev.md` に書いてある通り、**対称に聞くと結論が「重複」になる**からで、それは
正しい。だが「方向を入れる」ことと「前を残す」ことは別である。方向は**対を
見つける**ために要り、**どちらを残すか**は別の基準で決めてよい。MMR はその
別の基準が関連度であることを示している。「前を残す」は satisficing の逐次性を
写した**貪欲な順序の副産物**であって、読みの原理として文献に支持があるわけでは
ない。

ただし MMR にも反証がある。MMR は**質問（Q）がある**ことを前提にした枠組みで、
「一般的な読み手」の Q は文書の中心性で代用するしかない。それは Reading Tier
（ESSENTIAL か）と重なる。つまり **(b) の「関連度の高い方」は、実装上は「Tier の
高い方、同 Tier なら短い方」に落ちる**。

### 3. 読み手の目的で答えが変わる — 視点効果と目標指向読み（確認済み）

Pichert & Anderson は同じ物語を「泥棒」または「住宅購入者」の視点で読ませ、
**視点にとっての重要度が、その idea unit が学習されるか、1 週間後に想起される
かを独立に決める**ことを示した（重要度の主効果 F(2,202)=103.4）。続報では、
読了後に視点を切り替えるだけで、**最初の想起で出なかった情報が出てくる**。
符号化ではなく検索の側でも視点が効いている。

- Pichert, J. W., & Anderson, R. C. (1977). Taking different perspectives on a
  story. *Journal of Educational Psychology*, 69(4), 309–315.
  <https://files.eric.ed.gov/fulltext/ED134936.pdf>（Technical Report 版の
  本文を確認）
- Anderson, R. C., & Pichert, J. W. (1978). Recall of previously unrecallable
  information following a shift in perspective. *Journal of Verbal Learning and
  Verbal Behavior*, 17(1), 1–12.
  <https://www.sciencedirect.com/science/article/abs/pii/S0022537178904851>

Rothkopf & Billington は視線計測で、目標に関係する文は**注視が 2 倍以上**、
1 注視あたり 15 ms 長いこと、そして「付随的な文の速い走査」と「目標の遅い処理」
の 2 モードがあることを示した。McCrudden & Schraw はこれらを **relevance
instructions** として整理し（targeted segments / elaborative interrogation /
perspective / purpose の 4 種）、関連の指示が注意の配分と理解を変える 4 段階の
goal-focusing モデルを出している。

- Rothkopf, E. Z., & Billington, M. J. (1979). Goal-guided learning from text:
  Inferring a descriptive processing model from inspection times and eye
  movements. *Journal of Educational Psychology*, 71(3), 310–327.
  <https://pubmed.ncbi.nlm.nih.gov/521546/>
- McCrudden, M. T., & Schraw, G. (2007). Relevance and goal-focusing in text
  processing. *Educational Psychology Review*, 19(2), 113–139.
  <https://link.springer.com/article/10.1007/s10648-006-9010-7>（書誌と要旨を
  ERIC / publisher で確認）

**akapen にどう効くか。** 「読み手のモデルが無い」は設計上の選択である
（設計書「含めない: Reader Persona」）。文献はその選択の代償を正確に言っている。
**何が重要かは読み手の目的で変わり、目的が違えば逆になる。** 議事録の決定事項
リストは「決定を確認しに来た読み手」には ESSENTIAL で、「経緯を追う読み手」には
言い直しである。Reading Tier は 1 軸しかなく、どちらの読み手かを知らない。
**redundancy 規則の効き方が読み手で逆転する対は、この 1 軸では正しく処理できない。**
規則を弱めるにせよ勝者の決め方を変えるにせよ、それは「読み手のモデルが無い」
ことへの暫定処置であって、解決ではない。

### 4. Scim は同じ内容の複数箇所をどう扱ったか（確認済み・既存の節への補足）

Scim の本文を読み直した。**冗長対の処理は無い。** 「redundancy」「duplicate」に
相当する記述は本文に見つからなかった。あるのは次の 3 つで、どれも間接的に
「複数箇所」に触れている。

- 分布の後処理: 「まだハイライトを含まない段落の中の文を優先する」。これは
  同じ内容が 2 段落にあれば**両方に光る可能性を残す**設計で、akapen の
  「片方を沈める」とは逆
- 学習データの作り方: 全文の各文と **abstract との類似度**を測り、abstract に
  最も**似ていない**文を「どの facet でもない」負例にした（閾値 cos 0.25）。
  つまり **abstract（書き手の要約）に似ている文が重要側**という前提を置いて
  いる。これは Reder & Anderson と同じ向き — 要約に含まれる内容が主旨である
- 読み手の要望: 「abstract や introduction を、本文中の関連ハイライトへの
  **索引**として使いたい」（P2）。要約側を消すのではなく、要約側から本文へ
  飛びたい、という声

- Fok, R., et al. (2023). Scim: Intelligent Skimming Support for Scientific
  Papers. *IUI '23*. <https://arxiv.org/abs/2205.04561>（本文 PDF を確認）

**akapen にどう効くか。** Scim は要約側（abstract）を**重要度の基準**に使い、
それ自体を沈めるとは考えていない。akapen が末尾の決定事項リストを沈めるのは、
Scim の前提とは逆向きである。一方 Scim の分布規則は「両方光ってよい」を選んで
おり、それは冗長対を**解いていない**だけとも読める。

### 候補

**(a) 前を残す（いまのまま）**

- 支持: satisficing の逐次性と整合する。順読みの読み手には「もう読んだ」が
  事実。方向を入れた Noul は結論を誤判定しない（実測 0.71 → 0.36）
- 反証: 読み手の記憶の側に「前が残る」根拠が見当たらない。要約の位置は効果に
  差が無い（Hartley & Trueman、未確認）。MMR は位置を使わない。実文書での唯一の
  効き目が、多数派の読み手が欲しい部分を沈めている

**(b) 冗長な対では関連度の高い方 / 短い方を残す**

- 支持: Reder & Anderson（要約が主旨の記憶に優る、細部の無さが独立に効く）、
  MMR（勝者は関連度で決める、位置は使わない）、Scim（abstract に似る側が
  重要側）。実装上は `keep_order` が既に「同 Tier なら短い方」を持っていて、
  redundancy の勝者決めをそこへ寄せるだけで済む。**Noul の方向は変えない**
  （対を見つけるためには要る）
- 反証: 順読みの読み手には、先に読んだ本文が沈むのは奇妙。要約側が「言い直し」
  ではなく**独立した節**（決定事項が本文に無い項目を含む）のとき、対の判定
  そのものが怪しくなり、勝者を変えても救えない。「短い方」は要約と省略を区別
  しない

**(c) 読み手のモデルができるまで効き方を軽くする**

- 支持: Pichert & Anderson / McCrudden & Schraw — 何が重要かは読み手の目的で
  逆転する。1 軸の Tier では対のどちらを沈めても、片方の読み手を切る。Scim は
  両方光る設計を選んだ。`weakened()` の 1 段はすでに「軽い」ので、たとえば
  「redundant でも ESSENTIAL なら MARKED 候補に残す」だけで済む
- 反証: redundancy を入れた意味が薄れる。決定事項リストと本文節が両方 MARKED
  になれば、「これさえ見れば」の密度が上がる。分布の問題（既存の節「いちばん
  効く指摘は分布」）が悪化する

**(d) 位置ではなく「どちらが要約か」を聞く**

（追加の候補）Noul の方向を保ったまま、ラウンド 3 で REDUNDANT_WITH が付いた対に
ついて「この 2 箇所のうち、**他方の主旨を短くまとめているのはどちらか**」を
Choice で 1 問だけ聞く。Kintsch & van Dijk のマクロ構造に直接対応する問いで、
「短い方」より筋がよい。

- 支持: 要約側を残す根拠（1・2・4）をそのまま使える。対の数は少ない
  （実測で ESSENTIAL かつ REDUNDANT は 0〜1 件、SUPPORTING まで含めても数件）
  ので context window への影響はほぼ無い
- 反証: ラウンドが 1 つ増える（対が確定してからでないと聞けない）。Jev に
  「要約か」を聞くのが System One の範囲か、`jev.md` の線で確かめる必要が
  ある。「どちらでもない」（並列な 2 つの言い方）の逃げ道が要る

### 推し（決めない）

**(b) を、方向つきの Noul は変えずに勝者の決め方だけ変える形で。** 文献の向き
が揃っている（要約が記憶に効く、自動要約は位置で決めない、Scim も要約側を基準
にする）のと、実装が `keep_order` の既存の順序に寄せるだけで済むのが理由。
(d) はその上位互換だが、ラウンドが増えるので (b) で効き目を測ってから。

**(c) は (b) と両立する**（勝者を決めた上で、負けた側も ESSENTIAL なら落とさ
ない）。読み手のモデルを持たないと決めている以上、どこかで両方残す判断が要る。

**測る前に決めないこと。** 効き目が観察されたのは実文書 1 本の 1 対だけである。
(b) にして本文節が沈んだとき、順読みの読み手にどう見えるかは測っていない。
比較は `docs/gotchas/semantic-reading.md`「退行の判定」の手順で。

### 確認の内訳

| 状態 | 件数 | 内容 |
| --- | --- | --- |
| 確認済み（本文または publisher の要旨） | 9 | Reder & Anderson 1980 / 1982、Kintsch & van Dijk 1978、Carbonell & Goldstein 1998、Pichert & Anderson 1977、Anderson & Pichert 1978、Rothkopf & Billington 1979、McCrudden & Schraw 2007、Scim 2023 |
| 書誌のみ確認（内容は二次資料） | 1 | Ausubel 1960 |
| 未確認 | 1 | Hartley & Trueman 1982（位置の結論は二次資料のみ） |

---

## この文書の使い方

**実装の指示ではない。** 次に「見出しが無い文書をどうするか」「MARKED の
分布をどうするか」を考える人が、**同じ検索をやり直さずに済むため**にある。

採らなかったものには理由が書いてある。採る側に回すときは、その理由が
まだ成り立つかを先に確かめること。
