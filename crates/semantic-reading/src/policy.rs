//! Reading Policy — Tier と relation から、現在の Budget での表示状態を決める。
//!
//! ここは完全にローカルな決定論的計算である。`37% -> 36% -> 35%` という
//! 操作で Jev を呼ばないのがこの層の存在意義で、入力が同じなら出力も常に
//! 同じになる（乱数も時刻もスコアも使わない）。
//!
//! # この層は attention の配分を決めている
//!
//! 設計書（`docs/design/semantic-reading-layer.md`）の「研究的背景」が、
//! この層が何をする層なのかを決めている。
//!
//! > Duggan & Payne の satisficing モデルでは、読み進めることで得られる
//! > information gain が低下すると、読者は次の場所へ移動する。
//!
//! > **限られた attention を、意味的な収穫が高い部分へ配分する**
//!
//! つまりここで決めているのは **attention を文書のどこへ配るか**であって、
//! Unit の重要度ランキングではない。`keep_order` は確かに Unit を一列に
//! 並べるが、それは配分を決めるための手順である。この module から外へ出るのは
//! 「その Budget で残った集合」だけで、Unit ごとの順位もスコアも出さない
//! （設計書「0〜100 の importance score は使用しない」）。
//!
//! 下の並び替え鍵は、この枠組みではこう読める。
//!
//! - **Tier** は、そこを読んで得られる収穫の粗い見積り。粗い 4 段に留めるのは
//!   設計書の選択で、理由は [`crate::ReadingTier`] にある。
//! - **redundancy** は「その収穫をもう得てしまっているか」。satisficing は
//!   読み**進める**過程のモデルなので、重複は文書が単体で持つ性質ではなく
//!   **既読との相対**で決まる。設計書も「**既読内容との** redundancy」と
//!   書いている。`REDUNDANT_WITH` が対称な「重複している」ではなく方向を持つ
//!   関係なのはそのためで、判定器側でもこの方向を落とすと、結論のように文書中で
//!   何度も触れられる箇所を重複と見なしてしまう
//!   （実測は `examples/semantic/README.md`「redundancy は方向を指定する」）。
//! - **バイト長** は、その Unit が食う attention の量そのもの。同じ Tier なら
//!   短い方が attention あたりの収穫が大きい、というのが下の「短い方を先に
//!   残す」の言い換えである。
//!
//! 打ち切りを prefix にして best-fit にしないことも、ここから説明できる。
//! 設計書は Budget について「43% 42% 41% で表示が変わらなくても問題ない」と
//! 書いていて、**attention を余さず詰めること自体は目的ではない**。
//! 詰め方を捨てて代わりに得ているものは、下の単調性である。
//!
//! # 決め方
//!
//! 1. **Unit を「残したい順」に一列に並べる**（`keep_order` の並び替え鍵）。
//!    この順序は Budget に依存しない。
//! 2. **一段目 — 核を先に、単独で確保する。** 核を持つ Unit
//!    （= MARKED になりうるもの。ESSENTIAL かつ `core_atoms != Some([])`。
//!    **冗長かどうかは見ない** — 下の「核は奪わない」）を、
//!    **Budget を見ずに**残す。そのとき
//!    **その Unit が前提にしている Unit（`PRESUPPOSES` の閉包）も一緒に
//!    残す**（下の「context preservation」）。
//! 3. **二段目 — 残りの予算を、残りの Unit が奪い合う。** `keep_order` の
//!    先頭から順に、attention が尽きるまで残す。attention の量は Unit が
//!    占める source のバイト数で測り、`budget` はその何 % までを残すかを
//!    表す。入らない Unit が現れた時点で打ち切る（後ろの小さい Unit を
//!    拾い直さない）。**二段目の Unit は自分の前提を連れてこない。**
//! 4. **節に何か残ったなら、その節の見出しを戻す**（下の「見出しは中身に
//!    付いてくる」）。予算を見ない後段で、戻すのは見出しの行だけである。
//! 5. **残った Unit の意味情報を Atom へ投影する**。判断単位は Unit、
//!    表示単位は Atom。
//!
//! 二段にしている理由は下の「台帳の単位」にある。**予算の意味は変えて
//! いない** — 読み手が「これだけ時間がある」と言う入力のままで、変えたのは
//! その下で何を数えるかである。
//!
//! 打ち切りを「入らないものを飛ばして詰める」best-fit にしないのは、
//! Budget を 1 下げた瞬間に別の Unit が復活しうるからである。先頭からの
//! 素直な prefix にしておけば、Budget が下がったとき残る集合は必ず縮み、
//! **一度 DIM になった range が再び DIM でなくなることはない**（単調性）。
//!
//! # 並び替え鍵（同一 Tier 内の決定論的 rule）
//!
//! 鍵は次の 5 つ組で、小さいほど残りやすい。
//!
//! ```text
//! (実効 Tier, 対に負けたか, バイト長, 先頭バイト位置, Unit の並び順)
//! ```
//!
//! - **実効 Tier**: `ESSENTIAL < SUPPORTING < CONTEXT < DETAIL`。
//!   ただし冗長な対に**負けた** Unit は 1 段だけ弱い Tier として扱う
//!   （[`crate::ReadingTier::weakened`]）。これが「REDUNDANT は元 Tier に
//!   かかわらず優先的に DIM 候補」の実装。Tier を無視して重複を一律
//!   最下位へ落とす案も取れるが、それでは redundancy が Tier を上書きして
//!   しまい「別軸」でなくなるので採らなかった。負けた ESSENTIAL は
//!   負けた SUPPORTING より長く残る。
//! - **対に負けたか**: 同じ実効 Tier なら、負けていない方を先に残す。
//!   これで「負けた側は同 Tier の勝った側より先に DIM になる」。
//! - **バイト長**: 短い方を先に残す。同じ attention でより多くの意味単位が
//!   残り、文書全体の骨格が見えやすくなる。
//! - **先頭バイト位置 / Unit の並び順**: 文書順。最後の同点崩しであり、
//!   これで順序は必ず全順序になる（安定でない並び替えでも結果が揺れない）。
//!
//! # 負けは位置で決めない — 冗長な対のどちらが弱まるか
//!
//! `REDUNDANT_WITH` は持ち主から前の Unit へ向く。**方向は変えていない** —
//! 方向は**対を見つけるため**に要る（判定器側で落とすと、結論のように文書中
//! で何度も触れられる箇所を重複と見なす。上の「redundancy」）。変えたのは
//! **勝者の決め方**だけである。
//!
//! > 対のうち **Tier が低い方、同 Tier なら長い方**が負けて 1 段弱まる。
//!
//! 2026-09-22 まで、弱まるのは必ず `REDUNDANT_WITH` の**持ち主**、つまり
//! 対の**後ろ**にある方だった。位置で負けを決める根拠は文献に無い
//! （`docs/design/reading-research.md`「冗長な対のどちらを残すか」）。
//!
//! - Reder & Anderson — **要約が主旨の記憶に優る**。細部の無さが独立に効く
//! - MMR（Carbonell & Goldstein）— 冗長性を引いたうえで**勝者は関連度で
//!   決める**。位置は使わない
//! - Scim — 学習データが **abstract（書き手の要約）に似ている側を重要側**と
//!   置いている。要約側を沈めるのは、この前提と逆向きである
//!
//! 「短い方」は要約と省略を区別しないが、**この層が持っている値のうち
//! 「どちらが要約か」にいちばん近いのは長さである**。1 軸しかない Tier を
//! 第 1 キーに置いたうえで、同 Tier の中を長さで決める。
//!
//! ## 引き分けは前を勝たせる
//!
//! 同 Tier・同長のときは先頭バイト位置で前を勝たせ、負けを後ろにする。
//! `REDUNDANT_WITH` は必ず後ろから前へ向くので、**引き分けは 2026-09-22 より
//! 前の規則（持ち主が負ける）にそのまま退化する** — 変える理由の無い場合に
//! 変わらない。satisficing の逐次性（「もう読んだ」のは前である）とも
//! 整合する。位置がさらに同じなら Unit の並び順で崩し、全順序を保つ。
//!
//! ## 1 つの Unit が複数の対に入るとき
//!
//! **1 つでも負ければ負け**で、弱まるのは**それでも 1 段だけ**である
//! （[`losers`] は `bool` を返し、数えない）。`A -> B` と `C -> B` があって
//! B が片方で勝ち片方で負けるなら、B は 1 段弱まる。勝った対があっても
//! 救われない — 冗長な対の勝者は「その内容の代表」であり、どこかで代表の座を
//! 譲った Unit は代表ではない。「全部の対で負けたときだけ」にすると、
//! **無関係な第 3 の対があるかどうかで弱まるかが変わる**。
//!
//! ## 両方向から集める
//!
//! 負けが相手側（前の Unit）に来ることがあるので、
//! [`crate::SemanticUnit::redundant_with`] を持ち主から見るだけでは足りない。
//! [`losers`] は全 Unit の `REDUNDANT_WITH` を走査して**対の両端に印を
//! 付ける**。
//!
//! # 核は奪わない
//!
//! **冗長と判定されても、ESSENTIAL で核を持つなら一段目に入り MARKED に
//! なる**（[`bears_a_core`] は `is_redundant()` を見ない）。
//!
//! 設計書は Reader Persona を「含めない」と決めている。文献はその選択の代償を
//! 正確に言っていて、**何が重要かは読み手の目的で変わり、目的が違えば逆に
//! なる**（Pichert & Anderson、McCrudden & Schraw）。議事録の末尾の決定事項
//! リストは「決定を確認しに来た読み手」には ESSENTIAL で、「経緯を追う
//! 読み手」には言い直しである。Reading Tier は 1 軸しかなく、どちらの読み手
//! かを知らない。**読み手のモデルを持たないと決めている以上、読み手によって
//! 重要度が逆転する対で、どちらか一方の核を切る規則は持てない。**
//! Scim も分布の後処理で「同じ内容が 2 段落にあれば両方に光る可能性を残す」
//! を選んでいる。
//!
//! 冗長が消えたわけではない。**効き先が核から実効 Tier だけに狭まった** —
//! 負けた側は同 Tier の中で先に沈むが、核を持つ ESSENTIAL なら一段目に
//! いるので沈まない。つまり**この 2 つは重なると (b) が見えなくなる**:
//! 対の両側が ESSENTIAL かつ核持ちなら、どちらを弱めても表示は変わらない。
//! 負けの決め方が表示に効くのは、負けた側が二段目にいるとき
//! （ESSENTIAL 以外、または `core_atoms == Some([])`）である。
//!
//! **代償は分布である。** 決定事項リストと本文節が両方 MARKED になれば、
//! 「これさえ見れば」の密度が上がり、一段目が太って [`floor`] が上がる
//! （実測は `examples/semantic/measurements/redundancy-loser.md`）。
//!
//! # context preservation — 前提を一緒に生き残らせる
//!
//! 設計書は同一 Tier 内の rule として
//! `redundancy / length / document position / context preservation` の 4 つを
//! 挙げている。上の並び替え鍵が使っているのは最初の 3 つで、
//! **4 つ目はここ（打ち切り）にある。**
//!
//! 最初の 3 つはどれも **Unit 単体の属性**から決まる静的な値である
//! （重複しているか・何バイトか・文書のどこにあるか）。だから鍵は Unit を
//! 1 つ見れば計算できる。4 つ目だけが違い、
//! **残った Unit を順に読んだとき文脈が繋がるか**という、選択の結果に
//! 依存する性質を指している。satisficing は「読み進めて information gain が
//! 落ちたら次へ移る」という**逐次的**なモデルなので、4 つ目はこの層で
//! 逐次性を担う唯一の項目である。
//!
//! ## 何をもって「文脈が繋がる」とするか
//!
//! 設計書はこの語の中身を定義していない（出てくるのは上の rule の列挙
//! 1 箇所だけである）。**中身は実測で決めた**（2026-09-21、
//! `examples/semantic/measurements/context-preservation.md`）。この module は
//! 設計書に無い定義を自分で発明したのではなく、**測って選ばれた形を実装して
//! いる**。設計書側は列挙のままで、定義はここと計測にある。
//!
//! 決まった形は 2 つに分かれる。
//!
//! - **依存の判定は判定器（Jev）がする。** 「この部分は、前のどこかを
//!   読んでいないと意味が取れないか」（Noul）→「どれか」（Choice）→
//!   「ほかにもあるか」（Noul）。直接の前提は最大 2 で、**閾値はどこにも
//!   置かない**。答えは [`crate::Relation::Presupposes`] として運ばれてくる
//! - **閉包と予算への反映はこの層がする。** それが下である
//!
//! 第 1 版は依存を「自分より前の全 Unit と対の Noul」で聞き、閾値で切った。
//! 0.4 では閉包が文書の 17〜58 %（中央値）に膨らみ、0.5 では実文書で手に
//! 見つけた穴の片方を取り逃がした。**原因は閾値ではなく primitive の
//! 取り違えで**、役を入れ替えても「はい」になる対が 0.4 で 7.6〜32 %
//! あった — それは依存ではなく関連である。Choice は候補を突き合わせて 1 つ
//! 返すので扇が構造的に 1 になり、**閉包は 0.5 の大きさのまま 0.4 の再現率**
//! になった（b1 10.3 % / design 2.5 %、穴は 2 件とも捕捉）。
//!
//! ## 予算に数える。表示はしない
//!
//! **前提は「読み手に知らせる」ものではなく「一緒に生き残らせる」もの**
//! である。ステータス行に「どこかに穴がある」と出す案は却下してある
//! （読み手にできることが無い）。
//!
//! 反映の仕方は上の「決め方」2 のとおり、**一段目で核と一緒に払う**。
//! 閉包は根まで辿る（前提の前提も払う）。
//!
//! 数えないという選択肢は無い。**数えないと「30 % と言って 45 % 出る」**
//! ことになり、`budget` が指す量が文書によって変わる。数えたうえで、
//! 核のぶんだけは**取り合いの前に**確保する、というのが二段の形である。
//!
//! # 台帳の単位 — 予算は Unit で数え、マーカーは Atom に置いている
//!
//! 台帳が一段だった頃、**読む量を減らすと「最低限これを読め」が消えた**。
//!
//! ```text
//! READ 20%  ->  MARKED の Atom 3 個
//! READ  1%  ->  MARKED の Atom 1 個
//! ```
//!
//! ラベルの意味と逆である。原因は**単位のずれ**だった。**予算は Unit で
//! 数え、マーカーは Atom に置いている。** Unit が予算に入らなければ丸ごと
//! DIM になり、その中の核の一文も一緒に沈む。1 % では段落は買えないが、
//! **一文なら 1 % の中で誤差**である。
//!
//! そして下の「表示状態の割り当て」は「**MARKED は Budget に依存しない**」
//! と書いていた。一段の台帳では、その主張は「生き残った Unit については
//! 真、落ちた Unit については偽」で、**どこにもそう書いていなかった**。
//!
//! ## 直し方 — 核を取り合いに出さない
//!
//! 一段目で核とその閉包を確保し、二段目で残りを取り合う。**「これさえ
//! 見れば」なら、READ 1 % は核（とその前提）だけが出るべきである。**
//!
//! これは「閉包を払うのは MARKED の分だけ」を**含む**。一段だった頃は
//! 残るすべての Unit が自分の閉包を払っていて、実データでは SUPPORTING の
//! Unit が自分の前提を引き上げていた。二段にすると、二段目の Unit は自分の
//! 前提を連れてこない。
//!
//! ## 逆転はもう起きない
//!
//! 一段だった頃は、**長い系譜を持つ ESSENTIAL が丸ごと払えずに落ち、
//! 系譜の無い格下が代わりに繰り上がっていた**。実測では 26 Unit の小さい
//! 文書で READ 30 % のとき MARKED 5 つのうち 2 つが落ちた。
//!
//! 二段にして、この形は消えた（実測 5 / 5）。**落ちた MARKED を拾い直して
//! 直したのではない** — 拾い直しは best-fit であり、下の単調性を壊す。
//! 核を**そもそも取り合いに出さない**ので、落ちなくなった。
//!
//! ## 一段目が予算を超えることはある — そこが Budget の下限
//!
//! 一段目は Budget を見ないので、**核とその閉包が `budget` を超えることが
//! ある**。実測では一段目だけで記事の 28.3 %、設計書の 42.5 %、業務議事録の
//! 42〜55 % を占めた。READ 1 % と言ってもその量が出る。
//!
//! これは「30 % と言って 45 % 出る」を避けるという上の話と衝突して見えるが、
//! 衝突していない。あちらは**二段目の取り合いの中で前提を隠れて払う**こと
//! （予算の意味が文書ごとに変わる）を避けている。こちらは
//! **「最低限これを読め」は予算より先にある**という宣言で、量は
//! 一段目の大きさとして数えられる。
//!
//! 数えた値が [`floor`] である — 一段目のバイト数 ÷ 全体を切り上げた
//! 整数で、**その文書で `READ` の数字が表示量と一致するいちばん小さい
//! Budget**（一致は残る Unit について。予算外で戻る見出しの行は上乗せ）。下限より下では残る集合が動かず数字だけが嘘になるので、
//! クライアント（akapen）は Budget をそこより下へ回さず、下限にいることを
//! ステータス行に出す。**下限は測定値であって閾値ではない** — 定数も設定も
//! 無く、`decorate` と同じ `first_tier` で数える。下限が高い文書は核が多いか、
//! 核の前提の系譜が長い文書である。
//!
//! 下限は Tier の判定と一緒に揺れる。実測では業務議事録の 4 ランで
//! 43 / 56 / 48 / 50 %（幅 13 ポイント）、Tier を固定して辺だけ揺らした
//! 32 fixture では幅 0〜4 ポイントだった。
//!
//! ## 単調性は何に支えられているか
//!
//! 二段にしても単調性は保たれる。支えているのは次の 3 点だけで、
//! **どれも辺の構造をまったく使っていない**。
//!
//! 1. **一段目が Budget に依存しない**（だから定数である）
//! 2. `keep_order` が Budget に依存しない
//! 3. 打ち切りが `break` である（入らないものを飛ばして先を試さない）
//!
//! **冗長な対の負けを位置から Tier と長さへ変えても、この 3 点は動かない。**
//! [`losers`] が読むのは `relations` と生の Tier・バイト長・先頭位置だけで、
//! **Budget を引数にすら取っていない**。鍵が Budget 非依存であるかぎり、
//! 負けの決め方は単調性の証明のどこにも現れない。
//!
//! 一段目で残る集合も、そこで使う額も Budget の関数ではない。つまり
//! 二段目は**固定の下駄 `S_core` を履いて**始まる。二段目の各 rank で払う
//! 額は「それまでに何が kept になったか」だけで決まり、それは rank だけの
//! 関数である（Budget を見ていない）。つまり
//! **累積額の列 `S_core <= S_core + c_0 <= …` は Budget に依存しない固定列**
//! で、Budget が決めるのは「その列のどこで初めて越えるか」だけである。
//! 越える位置は Budget について単調なので、残る集合は入れ子になる。
//!
//! だから**前向きの辺や循環があっても単調性は壊れない**（閉包は訪問済み
//! 集合で辿るので必ず止まる）。壊れるのは `break` を `continue` にした
//! ときだけで、そのとき「Budget b で i が入らず j が入り、b+1 で i が入って
//! j が落ちる」が起きる。
//!
//! **副産物として、MARKED が Budget の関数でなくなった。** 核は必ず
//! 一段目にいるので、READ 1 % と READ 100 % で MARKED の集合は完全に
//! 一致する（`the_marked_set_never_moves_with_the_budget`）。
//!
//! ## 核が 1 つも無い文書だけ、先頭を強制で残す
//!
//! 「読む場所がゼロの表示には意味がない」ので先頭 1 つは Budget 1 % でも
//! 必ず残す。ただしこれが要るのは**一段目が空のときだけ**で、核があるなら
//! 一段目がすでに空でない。
//!
//! **この強制は前提を連れてこない**（二段目の規則のまま）。`context
//! preservation` は「光っている一文が読める」ための rule なので、光る Unit
//! が無い文書では引き上げる先も無い。
//!
//! ## 並び替え鍵には入れない
//!
//! 前提を鍵に混ぜる案は採らない。鍵は「どちらを先に残すか」しか言えず、
//! **「一緒に引き上げる」を表現できない**。さらに、前提の強さを鍵に足すと
//! DETAIL の前提が ESSENTIAL より先に来て、**Tier が第 1 キーである意味が
//! 消える**。context preservation は順序の rule ではなく、
//! **払い方の rule** である。
//!
//! ## `REDUNDANT_WITH` のほうは、今回も相対にしていない
//!
//! 負けは [`losers`] が静的に決めるのであって、
//! `REDUNDANT_WITH` の相手がその Budget で残っているかは見ていない。
//! u0 との対に負けて弱められた Unit は、u0 自身が DIM になる Budget でも
//! 弱められたままである。ここでの「既読」は「文書の中で前にある」であって
//! 「読者が実際に辿る経路の上で前にある」ではない。**これは未解決のまま
//! 残っている**（`docs/gotchas/open-questions.md`）。今回入れたのは
//! `PRESUPPOSES` の側だけである。
//!
//! # 見出しは中身に付いてくる — 境界規則 2 の言い直し
//!
//! > **節の中の Unit が 1 つでも残るなら、その節の見出しも残す。**
//!
//! **新しい原理ではない。** 判定器の境界規則 2「見出しは直後の内容に付く」
//! （`examples/semantic/jev-annotate.py` の `boundary_rule`）が、
//! 「見出しだけの Unit は単独では Tier を判定しづらい」という理由ですでに
//! 言っていることである。規則 4 で箇条書きを項目ごとに割ってから、見出しの
//! Unit は「見出し ＋ せいぜい最初の項目」になり、**2 つ目以降の項目は別
//! Unit として浮いた** — 境界だけでは規則 2 の意図が守れなくなった。ここに
//! あるのは、その意図を「中身が複数 Unit になった場合」について言い直した
//! ものである。**似た規則が 2 つあると読まないこと。**
//!
//! 業務議事録（22.7 KB・13 節）を READ 30 % で表示したときの報告は 2 件で、
//! 中身は 17 atom 中 8 / 13 atom 中 2 が残っていた — それが何なのかを言う
//! ラベルだけが無い。**同じ文書を既存の応答 6 ランで数え直すと 4〜5 件**に
//! なる（応答が `PRESUPPOSES` の前のもので、残る集合が違うため）。以下の
//! 数字はすべて後者の数え方である。
//!
//! ## 構造だけで決まる。Jev には聞かない
//!
//! 設計書「Jev に判断させないもの」に `syntax parsing` がある。どの Unit が
//! どの節に属するかは構文から決まるので、判定器が
//! [`crate::SemanticUnit::section_of`] として構造のまま渡してくる。この層が
//! するのは「同じ節か」を属性で見ることだけで、**`#` の数も節の範囲も
//! 知らない**。
//!
//! ## 置き場所 — `PRESUPPOSES` にしなかった理由
//!
//! 「見出しは中身の前提である」と読めば仕組みは既にある — 判定器が
//! [`crate::Relation::Presupposes`] を構造的に立てれば、閉包がそのまま
//! 見出しを引き上げる。**採らなかった理由は 2 つある。**
//!
//! - **出自が混ざる。** あの配列に並ぶのは Jev が判定したものだけで、そこへ
//!   構造由来の辺を足すと、JSON を読む人が「どれが判定されたのか」を
//!   区別できなくなる
//! - **どちらの段で払うのかを決めねばならない。** 台帳は二段で、一段目は
//!   Budget を見ず、二段目は自分の前提を連れてこない。見出しを前提にすると
//!   **核を持つ節の見出しだけが無料で戻り、核の無い節の見出しは戻らない** —
//!   「中身が残るなら見出しも残す」とは別の rule になる。二段目にも前提を
//!   連れてこさせる形に戻せば、今度は台帳が一段だった頃の
//!   「長い系譜が払えずに落ちる」が見出しで再発する
//!
//! 逆に、**この層が Atom の並びから節を組み立てる案**も採らない。それには
//! 見出しの深さが要る — `### 費用` で `## 決定事項` の節が終わるのかどうかは
//! `#` の数を見ないと決まらず、それは crate に markdown を教えることである。
//! 属性で受け取れば、深さを知らずに入れ子が伝わる（`section_of` は親の節を
//! 指すので、戻した見出しから辿り直すだけで根まで上がる）。
//!
//! ## 戻すのは Atom — 見出しの行だけ
//!
//! 規則 2 は見出しを直後の内容に付けるので、**見出しの Unit には実質的な
//! 第 1 文が同居していることがある**。Unit ごと戻すと、それも一緒に戻って
//! くる。戻したいのは「これが何の節か」というラベルだけなので、
//! [`AtomKind::Heading`] の Atom だけを NORMAL にする。
//!
//! **これは下の「DIM は一律」の唯一の例外である。** そちらが禁じているのは
//! 「残った Unit の中で、どこを読むかによって沈み方を変える」ことで、行の
//! 途中で説明のつかない切り替わりが起きるのを避けるための rule である。
//! ここで戻るのは**見出しの行そのもの**なので、画面には「節の名前は見える
//! が中身は沈んでいる」という、読者に説明できる形しか出ない。核の選択とは
//! 別の話である。
//!
//! ## NORMAL であって MARKED ではない
//!
//! 戻した見出しは NORMAL にする。MARKED は「読む価値が高い」であって、この
//! 見出しは予算の取り合いに負けている。ラベルだけが光る状態は
//! `docs/gotchas/semantic-reading.md`「箇条書きのラベルが MARKED になる」で
//! 読み物として意味を成さないと書いたものそのものである。
//!
//! ## 予算には数えない
//!
//! 前提（`PRESUPPOSES`）は数えるのに、こちらは数えない。**大きさが 1 桁
//! 違う。** 前提の閉包は実測で文書の 2.5〜10.3 % に達し、数えないと
//! 「30 % と言って 45 % 出る」になった。戻る見出しは実測で Atom バイトの
//! **0.68〜3.26 %**（4 文書 18 ラン、READ 1 / 5 / 30 %。上限の 3.26 % は
//! 1.6 KB の `demo.md` で、大きい文書ほど小さい）。
//!
//! 数えると、戻した見出しのぶんだけ二段目の取り合いが変わり、
//! **「見出しが戻ったせいで本文が沈む」**が起きる。それは規則 2 の意図の
//! 逆である。
//!
//! ## 単調性
//!
//! 見えている Unit は `kept` を `section_of` で閉じたものになる。`kept` が
//! Budget について入れ子なら、閉包も入れ子である（閉包は単調な写像で、
//! **辺の構造を使っていない**）。Atom で見ても同じで、戻るのは見出しの
//! Atom だけなので、Budget を上げて DIM へ戻る Atom は無い
//! （`heading_restoration_stays_monotone`）。
//!
//! ## 空っぽの見出しは直さない
//!
//! 節が丸ごと沈むとき、見出しも沈んだままである。実測では 4 文書 18 ラン ×
//! READ 1 / 5 / 30 / 100 % のすべてで、部分木に何も残らない節の見出しは
//! **1 件残らず DIM のまま**だった。**直すのは逆向きだけ**である。
//!
//! 部分木で見るのが要点で、**子の節に中身が残っていれば親の見出しは戻る**。
//! `### 費用` が残れば `## 決定事項` が戻り、それが `#` を戻す。READ 1 % で
//! 文書の背骨だけが立つのはこの連鎖である（`examples/semantic/README.md`
//! では見出し 25 本のうち 2 本 → 6〜8 本）。背骨は骨格そのものなので、
//! これは正しい見え方だと判断した。**低予算の見え方は実際に変わる** —
//! 前後の数字は `docs/gotchas/semantic-reading.md`
//! 「中身が残っているのに見出しが沈む」にある。
//!
//! # 表示状態の割り当て
//!
//! ```text
//! 残った Unit で ESSENTIAL かつ核を持つ（冗長かどうかは見ない）
//!     その Unit の核（core_atoms）        -> MARKED
//!     同じ Unit の残り                    -> NORMAL
//! 残ったそれ以外                          -> NORMAL
//! 残らなかったもの                        -> DIM（Unit 全体に一律）
//!     ただし、節に中身が残っている見出しの行 -> NORMAL（上の「見出しは
//!                                            中身に付いてくる」）
//! どの Unit にも属さない Atom             -> NORMAL
//! ```
//!
//! [`crate::SemanticUnit::core_atoms`] は 3 値で、`None`（絞り込み無し）なら
//! Unit 全体が MARKED、`Some([])`（**核を持たない**）ならその Unit は MARKED に
//! ならない。後者は ESSENTIAL のまま NORMAL になる — **DIM ではない**。沈めるか
//! どうかは Tier と Budget が決めることで、核の選に漏れたことは「読まなくて
//! よい」を意味しないからである。
//!
//! ## run キャップ — リスト 1 本につき核は 1 つ
//!
//! `Some([])` が要るのは判定器の側の事情である。箇条書きを項目ごとに割ると
//! （`examples/semantic/jev-annotate.py` の境界規則 4）、1 本のリストの中で
//! ESSENTIAL な Unit がいくつも立ち、**そのすべてが光る**。実測では業務
//! `CLAUDE.md` の MARKED が 3.3〜3.7 % から 10.8〜11.2 % へ増えた。
//!
//! そこで判定器は、規則 4 でつながった Unit の並び（= 1 本のリスト）ごとに
//! 核を 1 つだけ選び、選に漏れた Unit へ `Some([])` を返す。**この層は
//! 変わらない** — run を知らないし、知る必要もない。ここが読むのは
//! [`crate::SemanticUnit::is_core`] だけである。
//!
//! MARKED は Budget に依存しない。Budget 100% で全文を見せつつ ESSENTIAL に
//! 薄い marker を重ねる、という設計書の最初のデモがそのままこの規則である。
//!
//! **これは上の「一段目」が支えている。** 核を持つ Unit は Budget を見ずに
//! 残るので、`kept` は必ず真になり、`marks` は Budget の関数ではない。
//! 台帳が一段だった頃、この一文は生き残った Unit にしか当てはまっていな
//! かった（上の「台帳の単位」）。
//!
//! ## MARKED だけ、投影を選択的にする
//!
//! 設計書は判断単位と表示単位について
//!
//! > Semantic Unit に付与した意味情報を、その Unit を構成する Atom へ投影する
//!
//! とだけ書いていて、**投影が一律コピーだとは書いていない**。当初の実装が
//! Unit の Tier を構成 Atom 全部へそのまま配ったのはこちらの解釈であり、
//! ここで狭めているのはその解釈である（設計書の変更ではない）。設計書が
//! MARKED / DIM を **Atom** に対して定義していることとは、むしろこの形の方が
//! 整合する。
//!
//! 動機は実測である。45.6 KB の実業務文書で、Unit 一律の投影だと MARKED が
//! Atom バイトの 46.7 %（Budget 100 / 80 / 60 %）を占めた。境界の構造ルール
//! 「どちらも list_item → SAME」が箇条書き 1 つを丸ごと 1 Unit にするため、
//! ESSENTIAL な項目は段落ごと光る。**半分が光っていれば、光っていない方が
//! 目立つ。** 核だけを MARKED にすると、同じ文書・同じ Budget で 5.0 % に
//! 落ちた（実測表は `examples/semantic/measurements/core-selection.md`）。
//!
//! **DIM は一律のままにする。** Unit が落ちたなら、その Unit は丸ごと沈むのが
//! 正しい。ここを選択的にすると「なぜこの行の一部だけが沈んでいるのか」が
//! 読者に説明できなくなる。核の選択は「残った中のどこを読むか」であって、
//! 「何を落とすか」ではない。
//!
//! **例外は 1 つだけで、上の「見出しは中身に付いてくる」である。** あちらが
//! 戻すのは見出しの行そのもので、説明も 1 行で付く（節の名前は見えるが中身は
//! 沈んでいる）。核の選択のように、行の途中で理由の言えない切り替わりを
//! 作ることはしない。
//!
//! 核を選ぶのは判定器（Jev）で、この層は [`crate::SemanticUnit::core_atoms`]
//! を読むだけである。**核の選択は Budget に依存しない**ので、上の単調性は
//! そのまま成り立つ。

use std::collections::HashMap;
use std::ops::Range;

use crate::atom::AtomKind;
use crate::display::DisplayState;
use crate::document::SemanticDocument;
use crate::unit::ReadingTier;

/// Reading Budget の下限。0 を渡しても 1 として扱う。
pub const MIN_BUDGET: u8 = 1;
/// Reading Budget の上限。
pub const MAX_BUDGET: u8 = 100;

/// 現在の Budget での Atom ごとの表示状態を返す。
///
/// `budget` は「この文書にどれだけ attention を使えるか」を 1〜100 % で
/// 表す。範囲外の値は [`MIN_BUDGET`]〜[`MAX_BUDGET`] に丸める。
///
/// 戻り値は `doc.atoms` と同じ並び順・同じ個数で、各要素は
/// `(source のバイト範囲, 表示状態)`。クライアントはこれをそのまま range
/// decoration に渡せばよく、色や modifier の決定はクライアント側で行う。
///
/// Budget を変えても Jev は呼ばれない。この関数は純粋関数である。
///
/// # 単調性
///
/// `budget` を下げたとき、DIM になった範囲が再び DIM でなくなることはなく、
/// 上げたとき MARKED / NORMAL だった範囲が DIM になることもない。
pub fn decorate(doc: &SemanticDocument, budget: u8) -> Vec<(Range<usize>, DisplayState)> {
    let budget = budget.clamp(MIN_BUDGET, MAX_BUDGET);

    let by_id = index_by_id(doc);
    let order = keep_order(doc, &by_id);
    let total: usize = doc.units.iter().map(|unit| cost(doc, unit)).sum();

    // ---- 一段目 — 核を先に、単独で確保する ----------------------------
    //
    // **ここは Budget を見ない。** 核（= MARKED になりうる Unit）と、その
    // 前提の閉包を、予算の取り合いの前に確保する。取り合いに混ぜると
    // 「READ を下げたら『最低限これを読め』が消える」が起きる（下の
    // 「台帳の単位」）。この集合は Budget の関数ではないので、`spent` の
    // 初期値も Budget に依存しない定数である。**同じ集合を [`floor`] も
    // 読む** — 計算は `first_tier` の 1 箇所にある。
    let mut kept = vec![false; doc.units.len()];
    let (mut spent, any_core) = first_tier(doc, &order, &by_id, &mut kept);

    // ---- 二段目 — 残りの予算を Unit が奪い合う ------------------------
    //
    // `keep_order` の prefix 打ち切りのまま。**ここの Unit は自分の前提を
    // 連れてこない** — 前提を一緒に生き残らせるのは核のためのものなので、
    // 一段目で済んでいる。
    //
    // 核が 1 つも無い文書でだけ、先頭の 1 つを Budget 1 % でも必ず残す
    // （読む場所がゼロの表示には意味がないため）。核があるなら一段目が
    // すでに空でないので、この保険は要らない。
    let mut rank = 0usize;
    for &unit_index in &order {
        // 一段目で確保済みのものは、もう払ってある。
        if kept[unit_index] {
            continue;
        }
        let due = cost(doc, &doc.units[unit_index]);
        let fits = ((spent + due) as u128) * 100 <= (budget as u128) * (total as u128);
        if fits || (!any_core && rank == 0) {
            kept[unit_index] = true;
            spent += due;
        } else {
            break;
        }
        rank += 1;
    }

    // ---- 見出しの復帰 — 節に何か残ったなら、その節の見出しも残す --------
    //
    // **ここは予算を見ない。** 取り合いが終わったあとに、沈んだ見出しの
    // 行だけを戻す（下の「見出しは中身に付いてくる」）。
    let restored = restore_section_heads(doc, &by_id, &kept);

    // Unit の判断を Atom へ投影する。1 つの Atom を複数の Unit が指している
    // 場合は attention の強い方を採る（[`DisplayState::stronger`]）。
    let mut states = vec![None; doc.atoms.len()];
    for (unit_index, unit) in doc.units.iter().enumerate() {
        // MARKED になりうる Unit か。なる場合だけ、Unit の中で核と残りを
        // 分ける（NORMAL と DIM は Unit 全体に一律で掛かる）。
        //
        // **一段目がこの Unit を必ず残しているので、Budget は効かない。**
        // 「MARKED は Budget に依存しない」はここで文字どおり成り立つ。
        let marks = bears_a_core(unit);
        debug_assert!(!marks || kept[unit_index], "核を持つ Unit は一段目で残る");
        for &atom in &unit.atoms {
            let state = if kept[unit_index] {
                if marks && unit.is_core(atom) {
                    DisplayState::Marked
                } else {
                    DisplayState::Normal
                }
            } else if restored[unit_index] && is_heading(doc, atom) {
                // 落ちた節の見出し。**行だけ**が戻り、同じ Unit に同居して
                // いる本文は沈んだままである（下の「戻すのは Atom」）。
                DisplayState::Normal
            } else {
                DisplayState::Dim
            };
            if let Some(slot) = states.get_mut(atom.0) {
                *slot = Some(slot.map_or(state, |current: DisplayState| current.stronger(state)));
            }
        }
    }

    doc.atoms
        .iter()
        .zip(states)
        // どの Unit にも属さない Atom は未判断であって低優先度ではない。
        .map(|(atom, state)| (atom.range.clone(), state.unwrap_or(DisplayState::Normal)))
        .collect()
}

/// Reading Budget の**下限** — その文書で `READ` の数字が表示量と一致する
/// いちばん小さい Budget。
///
/// 一段目（核とその `PRESUPPOSES` 閉包）は Budget を見ずに残るので、
/// その大きさより下では **Budget をいくら下げても残る集合は変わらない**。
/// 表示は「READ 1 %」と言いながら一段目の分（実測で文書の 3〜5 割）を
/// 出すことになり、数字が嘘になる。この関数はその境目を返す。
///
/// 値は **一段目のバイト数 ÷ 全体を切り上げた整数**で、[`decorate`] と同じ
/// `cost` / `total` で数える。核が 1 つも無い文書では一段目が空で、代わりに
/// 先頭 1 Unit が強制で残る（[`decorate`] の保険）ので、その Unit の
/// 大きさがこれに相当する。Unit が無い、または全体が 0 バイトの文書では
/// [`MIN_BUDGET`]。
///
/// **これは測定値であって閾値ではない。** 定数も設定も無く、文書
/// （とその注釈）だけから決まる。下限が高い文書は核が多いか、核の前提の
/// 系譜が長い文書である。
///
/// # 成り立つこと
///
/// - `budget < floor` なら、残る集合は `decorate(doc, MIN_BUDGET)` と同じ
///   （一段目そのもの）。そこでは表示量が `budget` % を**超えている**
/// - `budget >= floor` なら、残る Unit の表示量は `budget` % 以下（数字は
///   嘘をつかない）。**見出しの復帰（`restore_section_heads`）は予算の外**
///   なので、画面にはそのぶんだけ上乗せされる — 実測で設計書 +1.4 ポイント、
///   業務議事録 +0.1 ポイント。下限は一段目のバイト数で数えるので、
///   `section_of` の有無で下限そのものは動かない
/// - `floor` は Budget の関数ではないので、[`decorate`] の単調性はそのまま
///
/// `decorate(doc, floor)` は一段目に加えて、**切り上げの端数（全体の 1 %
/// 未満）に収まる Unit** を二段目で拾うことがある。だから
/// `decorate(doc, floor)` と `decorate(doc, floor - 1)` が同じとは限らない
/// — 同じであることが多いが、それは文書の性質で、この関数の約束ではない。
pub fn floor(doc: &SemanticDocument) -> u8 {
    let by_id = index_by_id(doc);
    let order = keep_order(doc, &by_id);
    let total: usize = doc.units.iter().map(|unit| cost(doc, unit)).sum();
    if total == 0 {
        return MIN_BUDGET;
    }
    let mut kept = vec![false; doc.units.len()];
    let (mut spent, any_core) = first_tier(doc, &order, &by_id, &mut kept);
    if !any_core {
        // 一段目が空なら、先頭の 1 つが Budget を見ずに残る（`decorate` の
        // 保険と同じ Unit — `keep_order` の先頭）。
        if let Some(&first) = order.first() {
            spent = cost(doc, &doc.units[first]);
        }
    }
    // 切り上げ。spent <= total なので 100 を超えない。
    let percent = ((spent as u128) * 100).div_ceil(total as u128);
    u8::try_from(percent)
        .unwrap_or(MAX_BUDGET)
        .clamp(MIN_BUDGET, MAX_BUDGET)
}

/// Unit の attention コスト（構成 Atom のバイト長の合計）。
///
/// 範囲外の添字は validate で弾かれるが、ここでも黙って無視して panic しない。
fn cost(doc: &SemanticDocument, unit: &crate::unit::SemanticUnit) -> usize {
    length_of(doc, unit)
}

/// **一段目。** 核を持つ Unit（[`bears_a_core`]）とその前提の閉包を `kept`
/// に立て、使った額と「核が 1 つでもあったか」を返す。**Budget を見ない。**
///
/// [`decorate`] と [`floor`] の両方がここを読む。**2 箇所で別々に書かない**
/// こと — ずれると「下限で止めたのに表示量が下限を超える」が出る
/// （`bears_a_core` と同じ理由）。
fn first_tier(
    doc: &SemanticDocument,
    order: &[usize],
    by_id: &HashMap<&str, usize>,
    kept: &mut [bool],
) -> (usize, bool) {
    let mut spent = 0usize;
    let mut any_core = false;
    let mut bill = Vec::new();
    for &unit_index in order {
        if !bears_a_core(&doc.units[unit_index]) {
            continue;
        }
        any_core = true;
        // この Unit と、その前提の閉包。重複は無い（`prerequisites` は
        // 訪問済み集合で辿り、seed 自身を含めない）。
        bill.clear();
        bill.push(unit_index);
        prerequisites(doc, by_id, unit_index, &mut bill);
        for &index in &bill {
            if !kept[index] {
                kept[index] = true;
                spent += cost(doc, &doc.units[index]);
            }
        }
    }
    (spent, any_core)
}

/// この Unit は**一段目**に入るか — すなわち MARKED になりうるか。
///
/// 表示状態の割り当てと同じ条件である（ESSENTIAL / 核を持つ）。
/// **2 箇所で別々に書かない**こと — ずれると「一段目で確保したのに MARKED に
/// ならない Unit」や、その逆が出る。
///
/// **`is_redundant()` は見ない。** 冗長と判定された Unit でも、ESSENTIAL で
/// 核を持つなら核は取り上げない（モジュールドキュメントの「核は奪わない」）。
/// 冗長が効くのは [`keep_order`] の実効 Tier だけで、そこでも効くのは
/// 対の**負けた側**である。
fn bears_a_core(unit: &crate::unit::SemanticUnit) -> bool {
    unit.reading_tier == ReadingTier::Essential && unit.has_core()
}

/// この Atom は見出しの行か。
///
/// 見出しかどうかは [`crate::atomize`] が既に決めていて、答えは
/// [`AtomKind::Heading`] として Atom に乗っている。**この層が読むのは
/// そこだけ**で、`#` の数も節の範囲も見ない（それは
/// [`crate::SemanticUnit::section_of`] が運んでくる）。
fn is_heading(doc: &SemanticDocument, atom: crate::atom::AtomIndex) -> bool {
    doc.atom(atom)
        .is_some_and(|atom| atom.kind == AtomKind::Heading)
}

/// 残った Unit から `section_of` を辿り、**戻す見出し Unit** に印を付ける。
///
/// 返り値は Unit ごとの真偽で、`kept` が真のものは常に偽である
/// （すでに残っているものを「戻す」必要は無い）。
///
/// # 入れ子は勝手に伝わる
///
/// 戻した見出し Unit を frontier へ積み直すので、`### 費用` が戻れば
/// その親の `## 決定事項` も戻り、さらにその親も戻る。**節の入れ子を
/// この層が知る必要は無い** — 見出し Unit 自身の `section_of` が親を
/// 指しているだけで連鎖する。
///
/// # 循環でも止まる
///
/// 節の見出しは自分より前にあるので辺は後ろ向きにしか立たないが、
/// **ここはそれに依存していない**。印の付いた添字を二度積まないので、
/// 前向きの辺が混ざっても循環があっても必ず止まる。知らない id は
/// 黙って飛ばす（弾くのは [`SemanticDocument::validate`] の仕事）。
fn restore_section_heads(
    doc: &SemanticDocument,
    by_id: &HashMap<&str, usize>,
    kept: &[bool],
) -> Vec<bool> {
    let mut restored = vec![false; doc.units.len()];
    let mut frontier: Vec<usize> = (0..doc.units.len()).filter(|&index| kept[index]).collect();
    while let Some(current) = frontier.pop() {
        let Some(unit) = doc.units.get(current) else {
            continue;
        };
        let Some(head) = unit.section_of.as_ref() else {
            continue;
        };
        let Some(&index) = by_id.get(head.as_str()) else {
            continue;
        };
        // すでに残っているか、もう戻したものは辿り直さない。
        if kept.get(index).copied().unwrap_or(false) || restored[index] {
            continue;
        }
        restored[index] = true;
        frontier.push(index);
    }
    restored
}

/// 冗長な対の**負けた側**に印を付ける。Budget には依存しない。
///
/// `REDUNDANT_WITH` は持ち主から前の Unit へ向くが、**弱まるのは持ち主とは
/// 限らない**。対のうち Tier が低い方、同 Tier なら長い方が負ける
/// （モジュールドキュメントの「負けは位置で決めない」）。だから
/// **relation を両方向から集める必要がある** — 持ち主の `redundant_with()`
/// だけを見ると、負けが相手側に来た対を取り逃がす。
///
/// 比べるのは `(Tier, 長さ, 先頭位置, 添字)` で、**どれも生の値**である。
/// 実効 Tier（弱めた後）で比べると自分自身を参照することになる。
///
/// # 1 つの Unit が複数の対に入るとき
///
/// **1 つでも負ければ負け**で、弱まるのは**それでも 1 段だけ**である
/// （`bool` であって数えない）。勝った対があっても救われない — 冗長な対の
/// 勝者は「その内容の代表」であって、どこかで代表の座を譲った Unit は
/// 代表ではない。「全部の対で負けたときだけ」にすると、無関係な第 3 の対が
/// あるかどうかで弱まるかが変わる。
///
/// 知らない id は黙って飛ばす（弾くのは
/// [`crate::SemanticDocument::validate`] の仕事）。
fn losers(doc: &SemanticDocument, by_id: &HashMap<&str, usize>) -> Vec<bool> {
    // 比較鍵。小さいほど強い（= 勝つ）。
    let rank = |index: usize| {
        let unit = &doc.units[index];
        (unit.reading_tier, length_of(doc, unit), start_of(doc, unit), index)
    };
    let mut loser = vec![false; doc.units.len()];
    for (owner, unit) in doc.units.iter().enumerate() {
        // **`redundant_with()` ではない。** あれは最初の 1 つしか返さないので、
        // 2 つ目以降の対を取り逃がす。
        for target in unit.redundancies() {
            let Some(&other) = by_id.get(target.as_str()) else {
                continue;
            };
            if other == owner {
                continue;
            }
            loser[if rank(owner) < rank(other) { other } else { owner }] = true;
        }
    }
    loser
}

/// Unit の構成 Atom のバイト長の合計（並び替え鍵の「長さ」）。
fn length_of(doc: &SemanticDocument, unit: &crate::unit::SemanticUnit) -> usize {
    unit.atoms
        .iter()
        .filter_map(|&atom| doc.atom(atom))
        .map(|atom| atom.len())
        .sum()
}

/// Unit の先頭バイト位置（並び替え鍵の「文書順」）。
fn start_of(doc: &SemanticDocument, unit: &crate::unit::SemanticUnit) -> usize {
    unit.atoms
        .iter()
        .filter_map(|&atom| doc.atom(atom))
        .map(|atom| atom.range.start)
        .min()
        .unwrap_or(usize::MAX)
}

/// Unit を「残したい順」に並べた添字列を返す。Budget には依存しない。
///
/// 並び替え鍵はモジュールドキュメントのとおり。`decorate` の単調性は
/// 「この順序が Budget に依存しないこと」と「prefix で打ち切ること」の
/// 2 点だけに支えられている。**[`losers`] も Budget を見ない**ので、
/// 負けの決め方を変えてもこの 2 点は動かない。
fn keep_order(doc: &SemanticDocument, by_id: &HashMap<&str, usize>) -> Vec<usize> {
    let loser = losers(doc, by_id);
    let mut order: Vec<usize> = (0..doc.units.len()).collect();
    order.sort_by_cached_key(|&index| {
        let unit = &doc.units[index];
        // 弱まるのは**負けた側**だけである。対の勝者は、冗長でない Unit と
        // 区別が付かない（`REDUNDANT_WITH` の持ち主であっても）。
        let lost = loser[index];
        let effective_tier = if lost {
            unit.reading_tier.weakened()
        } else {
            unit.reading_tier
        };
        (
            effective_tier,
            lost,
            length_of(doc, unit),
            start_of(doc, unit),
            index,
        )
    });
    order
}

/// `id` から Unit の添字を引く表。
///
/// [`crate::SemanticDocument::unit`] は線形探索なので、閉包を Unit の数だけ
/// 辿ると 3 乗になる。`decorate` 1 回につき 1 度だけ作る。
fn index_by_id(doc: &SemanticDocument) -> HashMap<&str, usize> {
    // id の重複は validate が弾くが、ここでは黙って先勝ちにする
    // （`decorate` は壊れた入力でも panic しない、という約束のほう）。
    let mut by_id = HashMap::with_capacity(doc.units.len());
    for (index, unit) in doc.units.iter().enumerate() {
        by_id.entry(unit.id.as_str()).or_insert(index);
    }
    by_id
}

/// `seed` が推移的に前提にしている Unit の添字を `out` へ追記する。
///
/// **`seed` 自身は含めない。** 呼び手が先に `out` へ入れている前提で、
/// 重複も入れない（`out` に既にある添字は辿り直さない）。
///
/// # 循環でも止まる
///
/// 判定器は「自分より前の Unit」しか選択肢にしないので辺は後ろ向きにしか
/// 立たないが、**ここはそれに依存していない**。すでに見た添字を二度辿ら
/// ないので、前向きの辺が混ざっても循環があっても必ず止まる。参照先が
/// 実在することは [`crate::SemanticDocument::validate`] が保証するが、
/// 知らない id はここでも黙って飛ばす（panic しない）。
fn prerequisites(
    doc: &SemanticDocument,
    by_id: &HashMap<&str, usize>,
    seed: usize,
    out: &mut Vec<usize>,
) {
    let mut frontier = vec![seed];
    while let Some(current) = frontier.pop() {
        let Some(unit) = doc.units.get(current) else {
            continue;
        };
        for target in unit.presupposes() {
            let Some(&index) = by_id.get(target.as_str()) else {
                continue;
            };
            // seed 自身へ戻る辺（循環）も、すでに積んだ添字も辿らない。
            if index == seed || out.contains(&index) {
                continue;
            }
            out.push(index);
            frontier.push(index);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atom::{Atom, AtomIndex, AtomKind};
    use crate::unit::{Relation, SemanticUnit};

    /// 長さの等しい Atom を 1 つずつ持つ Unit を並べた文書を作る。
    fn doc(tiers: &[(ReadingTier, bool)]) -> SemanticDocument {
        let atoms = (0..tiers.len())
            .map(|i| Atom::new(i * 10..i * 10 + 10, AtomKind::Sentence))
            .collect();
        let units = tiers
            .iter()
            .enumerate()
            .map(|(i, &(tier, redundant))| {
                let mut unit = SemanticUnit::new(format!("u{i}"), [AtomIndex(i)], tier);
                if redundant {
                    // 参照先は 0 番固定（0 番自身が redundant な文書は作らない）。
                    unit.relations.push(Relation::RedundantWith("u0".into()));
                }
                unit
            })
            .collect();
        SemanticDocument::new(atoms, units)
    }

    fn states(doc: &SemanticDocument, budget: u8) -> Vec<DisplayState> {
        decorate(doc, budget).into_iter().map(|(_, s)| s).collect()
    }

    #[test]
    fn essential_units_are_marked_and_the_rest_are_normal_at_full_budget() {
        let doc = doc(&[
            (ReadingTier::Essential, false),
            (ReadingTier::Supporting, false),
            (ReadingTier::Detail, false),
        ]);
        assert_eq!(
            states(&doc, 100),
            [
                DisplayState::Marked,
                DisplayState::Normal,
                DisplayState::Normal
            ]
        );
    }

    /// 核を持つ ESSENTIAL は、核だけ MARKED で残りは NORMAL。
    #[test]
    fn only_the_core_of_an_essential_unit_is_marked() {
        let atoms = (0..3)
            .map(|i| Atom::new(i * 10..i * 10 + 10, AtomKind::Sentence))
            .collect();
        let mut unit = SemanticUnit::new(
            "u0",
            [AtomIndex(0), AtomIndex(1), AtomIndex(2)],
            ReadingTier::Essential,
        );
        unit.set_core([AtomIndex(1)]);
        let doc = SemanticDocument::new(atoms, vec![unit]);
        assert_eq!(
            states(&doc, 100),
            [
                DisplayState::Normal,
                DisplayState::Marked,
                DisplayState::Normal
            ]
        );
    }

    /// 核を持つ Unit が Budget から落ちたら、核も含めて丸ごと DIM。
    /// **DIM は選択的にしない** — 行の一部だけ沈む理由を読者に説明できない。
    #[test]
    fn a_dropped_unit_dims_whole_even_when_it_has_a_core() {
        let atoms = vec![
            Atom::new(0..10, AtomKind::Sentence),
            Atom::new(10..20, AtomKind::Sentence),
            Atom::new(20..60, AtomKind::Sentence),
        ];
        let mut essential = SemanticUnit::new(
            "keep",
            [AtomIndex(0), AtomIndex(1)],
            ReadingTier::Essential,
        );
        essential.set_core([AtomIndex(0)]);
        // **SUPPORTING にしてある。** 二段目に落ちるのは MARKED になれない
        // Unit だけなので（核を持つ ESSENTIAL は一段目が必ず確保する）、
        // 「核を持つのに沈む」を作るには ESSENTIAL 以外を使う。
        // `core_atoms` は付いたままで、**それが DIM に一切効かない**ことが
        // ここの主張である。
        //
        // 2026-09-22 まではここを REDUNDANT な ESSENTIAL で作っていた。
        // 冗長でも核は奪わなくなった（[`bears_a_core`]）ので、それでは
        // 一段目に入ってしまう。
        let mut dropped = SemanticUnit::new("drop", [AtomIndex(2)], ReadingTier::Supporting);
        dropped.set_core([AtomIndex(2)]);
        let doc = SemanticDocument::new(atoms, vec![essential, dropped]);
        // 60 バイト中、一段目の "keep"（20 バイト）を引いた残りに
        // "drop"（40 バイト）は入らない。
        assert_eq!(
            states(&doc, 70),
            [
                DisplayState::Marked,
                DisplayState::Normal,
                DisplayState::Dim
            ]
        );
    }

    /// 核は Budget に依存しない — 絞られた MARKED も、Unit が残る限り一定。
    #[test]
    fn the_core_does_not_move_when_the_budget_does() {
        let atoms = vec![
            Atom::new(0..10, AtomKind::Sentence),
            Atom::new(10..20, AtomKind::Sentence),
            Atom::new(20..90, AtomKind::Sentence),
        ];
        let mut essential =
            SemanticUnit::new("u0", [AtomIndex(0), AtomIndex(1)], ReadingTier::Essential);
        essential.set_core([AtomIndex(1)]);
        let doc = SemanticDocument::new(
            atoms,
            vec![
                essential,
                SemanticUnit::new("u1", [AtomIndex(2)], ReadingTier::Detail),
            ],
        );
        for budget in [100, 60, 30, 1] {
            assert_eq!(
                states(&doc, budget)[..2],
                [DisplayState::Normal, DisplayState::Marked],
                "budget {budget}"
            );
        }
    }

    /// 核が NORMAL / DIM の Unit に付いていても無視される — 効くのは MARKED
    /// になる Unit だけ。
    #[test]
    fn a_core_on_a_non_marked_unit_changes_nothing() {
        let atoms = vec![
            Atom::new(0..10, AtomKind::Sentence),
            Atom::new(10..20, AtomKind::Sentence),
        ];
        let mut unit =
            SemanticUnit::new("u0", [AtomIndex(0), AtomIndex(1)], ReadingTier::Supporting);
        unit.set_core([AtomIndex(0)]);
        let doc = SemanticDocument::new(atoms, vec![unit]);
        assert_eq!(
            states(&doc, 100),
            [DisplayState::Normal, DisplayState::Normal]
        );
    }

    /// **空の核は「核を持たない」** — その Unit は MARKED にならず NORMAL に
    /// なる。リスト 1 本につき核を 1 つに絞るとき、選に漏れた ESSENTIAL な
    /// Unit がこれを受け取る。DIM ではない（沈めるかどうかは Tier と Budget が
    /// 決めることで、核の選に漏れたことは「読まなくてよい」を意味しない）。
    #[test]
    fn an_essential_unit_with_an_empty_core_is_normal_not_marked() {
        let atoms = vec![
            Atom::new(0..10, AtomKind::Sentence),
            Atom::new(10..20, AtomKind::Sentence),
        ];
        let mut unit =
            SemanticUnit::new("u0", [AtomIndex(0), AtomIndex(1)], ReadingTier::Essential);
        unit.set_core([]);
        let doc = SemanticDocument::new(atoms, vec![unit]);
        assert_eq!(
            states(&doc, 100),
            [DisplayState::Normal, DisplayState::Normal]
        );
    }

    /// 核を返さない判定器（と既存の fixture）は従来どおり Unit 全体が MARKED。
    #[test]
    fn a_unit_without_a_core_still_marks_all_of_its_atoms() {
        let atoms = vec![
            Atom::new(0..10, AtomKind::Sentence),
            Atom::new(10..20, AtomKind::Sentence),
        ];
        let unit =
            SemanticUnit::new("u0", [AtomIndex(0), AtomIndex(1)], ReadingTier::Essential);
        let doc = SemanticDocument::new(atoms, vec![unit]);
        assert_eq!(
            states(&doc, 100),
            [DisplayState::Marked, DisplayState::Marked]
        );
    }

    /// **核は奪わない。** 冗長な対の片側でも、ESSENTIAL で核を持つなら
    /// MARKED になる。読み手のモデルを持たない以上、読み手によって重要度が
    /// 逆転する対でどちらか一方を切る規則は持てない
    /// （モジュールドキュメントの「核は奪わない」）。
    ///
    /// **2026-09-22 に逆になった。** それまでは片側が NORMAL に落ちていた。
    #[test]
    fn a_redundant_essential_unit_keeps_its_core() {
        let doc = doc(&[
            (ReadingTier::Essential, false),
            (ReadingTier::Essential, true),
        ]);
        assert_eq!(
            states(&doc, 100),
            [DisplayState::Marked, DisplayState::Marked]
        );
        // どちらも一段目にいるので、Budget 1 % でも両方光る。
        assert_eq!(
            states(&doc, MIN_BUDGET),
            [DisplayState::Marked, DisplayState::Marked]
        );
    }

    #[test]
    fn weaker_tiers_dim_first() {
        let doc = doc(&[
            (ReadingTier::Detail, false),
            (ReadingTier::Context, false),
            (ReadingTier::Supporting, false),
            (ReadingTier::Essential, false),
        ]);
        // 4 Unit × 各 25 %。50 % なら ESSENTIAL と SUPPORTING だけが残る。
        assert_eq!(
            states(&doc, 50),
            [
                DisplayState::Dim,
                DisplayState::Dim,
                DisplayState::Normal,
                DisplayState::Marked
            ]
        );
    }

    #[test]
    fn a_redundant_unit_dims_before_its_non_redundant_sibling() {
        let doc = doc(&[
            (ReadingTier::Essential, false),
            (ReadingTier::Supporting, false),
            (ReadingTier::Supporting, true),
        ]);
        assert_eq!(
            states(&doc, 70),
            [
                DisplayState::Marked,
                DisplayState::Normal,
                DisplayState::Dim
            ]
        );
    }

    /// 負けは Tier を上書きせず 1 段だけ押し下げる。負けた ESSENTIAL は
    /// SUPPORTING として競い、負けた SUPPORTING（実効 CONTEXT）より長く残る。
    ///
    /// **核を空にしてある。** 核を持つ ESSENTIAL は一段目にいて沈まないので、
    /// 実効 Tier の差が表示に出ない（モジュールドキュメントの「核は奪わない」
    /// の最後の段落）。負けの効き目が見えるのは、負けた側が二段目にいるとき
    /// だけである。
    #[test]
    fn losing_weakens_the_tier_by_one_step_without_erasing_it() {
        let mut doc = sized(&[
            (ReadingTier::Essential, 10),
            (ReadingTier::Essential, 10),
            (ReadingTier::Supporting, 10),
            (ReadingTier::Context, 10),
        ]);
        // u1 と u2 が、それぞれ u0 を言い直している（どちらも負ける）。
        doc.units[1]
            .relations
            .push(Relation::RedundantWith("u0".into()));
        doc.units[2]
            .relations
            .push(Relation::RedundantWith("u0".into()));
        doc.units[1].set_core([]);
        // 一段目は u0 の 10 バイト。50 % = 20 バイトなので、あと 1 つだけ入る。
        //
        // 負けた ESSENTIAL は実効 SUPPORTING なので、非 REDUNDANT の
        // CONTEXT（u3）より先に残る。**負けを一律最下位へ落とす実装なら
        // ここは `[0, 3]` になる** — それが「消さない」の意味である。
        assert_eq!(kept_units(&doc, 50), [0, 1]);
    }

    /// **負けるのは持ち主とは限らない。** 相手のほうが Tier が低ければ、
    /// `REDUNDANT_WITH` を書いた側ではなく**指された側**が弱まる。
    /// 2026-09-22 まではここが逆だった。
    #[test]
    fn the_loser_is_the_weaker_tier_not_the_owner() {
        let mut doc = sized(&[
            (ReadingTier::Supporting, 10),
            (ReadingTier::Essential, 10),
            (ReadingTier::Supporting, 10),
        ]);
        // u1（ESSENTIAL・後ろ）が u0（SUPPORTING・前）を言い直している。
        doc.units[1]
            .relations
            .push(Relation::RedundantWith("u0".into()));
        // 核を空にして、u1 も取り合いに出す（一段目に隠れさせない）。
        doc.units[1].set_core([]);
        // 負けたのは u0（実効 CONTEXT）。u2 は同じ SUPPORTING のまま。
        // 70 % = 21 バイト → u1（ESSENTIAL）と u2（SUPPORTING）が入り、
        // u0 は入らない。**持ち主が負ける規則なら、沈むのは u1 だった。**
        assert_eq!(kept_units(&doc, 70), [1, 2]);
    }

    /// 同 Tier なら**長い方**が負ける。位置は見ない。
    #[test]
    fn the_loser_is_the_longer_side_inside_the_same_tier() {
        let mut doc = sized(&[
            (ReadingTier::Essential, 40),
            (ReadingTier::Essential, 10),
            (ReadingTier::Supporting, 10),
        ]);
        // 短い u1 が、長い u0 を言い直している。
        doc.units[1]
            .relations
            .push(Relation::RedundantWith("u0".into()));
        doc.units[0].set_core([]);
        doc.units[1].set_core([]);
        // 負けたのは長い u0（実効 SUPPORTING）。u0 は 40 バイトあるので、
        // 同じ実効 SUPPORTING の u2（10 バイト）より後ろに回る。
        // 60 バイト中 40 % = 24 バイト → u1（10）と u2（10）が入る。
        assert_eq!(kept_units(&doc, 40), [1, 2]);
    }

    /// 引き分け（同 Tier・同長）は**前を勝たせる**。`REDUNDANT_WITH` は
    /// 必ず後ろから前へ向くので、これは 2026-09-22 より前の規則
    /// （持ち主が負ける）にそのまま退化する。
    #[test]
    fn a_tie_lets_the_earlier_unit_win() {
        let mut doc = sized(&[
            (ReadingTier::Supporting, 10),
            (ReadingTier::Supporting, 10),
        ]);
        doc.units[1]
            .relations
            .push(Relation::RedundantWith("u0".into()));
        // 負けたのは後ろの u1（実効 CONTEXT）。
        assert_eq!(kept_units(&doc, 50), [0]);
    }

    /// 1 つの Unit が複数の対に入るとき — **1 つでも負ければ負け**、
    /// それでも弱まるのは **1 段だけ**である（負けた回数を数えない）。
    #[test]
    fn losing_two_pairs_still_weakens_by_exactly_one_step() {
        let mut doc = sized(&[
            (ReadingTier::Essential, 10),
            (ReadingTier::Essential, 10),
            (ReadingTier::Supporting, 10),
            (ReadingTier::Detail, 10),
        ]);
        // u2 は u0 とも u1 とも対を成し、**どちらでも負ける**（Tier が低い）。
        doc.units[2]
            .relations
            .push(Relation::RedundantWith("u0".into()));
        doc.units[2]
            .relations
            .push(Relation::RedundantWith("u1".into()));
        doc.units[1].set_core([]);
        // 一段目は u0 の 10 バイト。75 % = 30 バイトで、あと 2 つ入る。
        //
        // u2 は 2 回負けたが 1 段だけ弱まって実効 CONTEXT なので、
        // 非 REDUNDANT の DETAIL（u3）より先に残る。**負けを数える実装なら
        // u2 は実効 DETAIL に落ち、同じ実効 DETAIL の中で `lost` が真な
        // ぶん u3 に負けて、ここは `[0, 1, 3]` になる。**
        assert_eq!(kept_units(&doc, 75), [0, 1, 2]);
    }

    /// 勝った対があっても救われない — どこかで代表の座を譲った Unit は
    /// 代表ではない。
    #[test]
    fn winning_one_pair_does_not_cancel_losing_another() {
        let mut doc = sized(&[
            (ReadingTier::Essential, 10),
            (ReadingTier::Supporting, 10),
            (ReadingTier::Detail, 10),
            (ReadingTier::Context, 10),
        ]);
        // u1 は u0 との対では負け（Tier が低い）、u2 との対では勝つ。
        doc.units[1]
            .relations
            .push(Relation::RedundantWith("u0".into()));
        doc.units[2]
            .relations
            .push(Relation::RedundantWith("u1".into()));
        // 一段目は u0 の 10 バイト。50 % = 20 バイトで、あと 1 つ。
        //
        // u1 は負けて実効 CONTEXT、`lost` が真。非 REDUNDANT の CONTEXT
        // （u3）と同じ実効 Tier で競って**後ろに回る**。**勝った対で救う
        // 実装なら u1 は SUPPORTING のままで、ここは `[0, 1]` になる。**
        assert_eq!(kept_units(&doc, 50), [0, 3]);
    }

    #[test]
    fn shorter_units_win_inside_the_same_tier_then_document_order() {
        let atoms = vec![
            Atom::new(0..30, AtomKind::Sentence),
            Atom::new(30..40, AtomKind::Sentence),
            Atom::new(40..50, AtomKind::Sentence),
        ];
        let units = vec![
            SemanticUnit::new("long", [AtomIndex(0)], ReadingTier::Context),
            SemanticUnit::new("short_a", [AtomIndex(1)], ReadingTier::Context),
            SemanticUnit::new("short_b", [AtomIndex(2)], ReadingTier::Context),
        ];
        let doc = SemanticDocument::new(atoms, units);
        // 短い 2 つ（10 + 10 = 40 %）が先。長い 30 バイトは 50 % では入らない。
        assert_eq!(
            states(&doc, 50),
            [
                DisplayState::Dim,
                DisplayState::Normal,
                DisplayState::Normal
            ]
        );
        // 同じ長さ同士は文書順。20 % なら先に出てくる short_a だけ。
        assert_eq!(
            states(&doc, 20),
            [DisplayState::Dim, DisplayState::Normal, DisplayState::Dim]
        );
    }

    #[test]
    fn at_least_one_unit_survives_the_smallest_budget() {
        let doc = doc(&[
            (ReadingTier::Detail, false),
            (ReadingTier::Essential, false),
        ]);
        assert_eq!(states(&doc, 1), [DisplayState::Dim, DisplayState::Marked]);
        // 0 は 1 に、101 以上は 100 に丸める。
        assert_eq!(states(&doc, 0), states(&doc, 1));
        assert_eq!(states(&doc, 255), states(&doc, 100));
    }

    #[test]
    fn atoms_outside_every_unit_stay_normal_at_any_budget() {
        let mut doc = doc(&[(ReadingTier::Detail, false)]);
        doc.atoms.push(Atom::new(100..120, AtomKind::CodeBlock));
        let states = states(&doc, 1);
        assert_eq!(states[1], DisplayState::Normal);
    }

    #[test]
    fn an_atom_shared_by_two_units_takes_the_stronger_state() {
        let atoms = vec![Atom::new(0..10, AtomKind::Sentence)];
        let units = vec![
            SemanticUnit::new("u0", [AtomIndex(0)], ReadingTier::Detail),
            SemanticUnit::new("u1", [AtomIndex(0)], ReadingTier::Essential),
        ];
        let doc = SemanticDocument::new(atoms, units);
        assert_eq!(states(&doc, 100), [DisplayState::Marked]);
    }

    // ---------------------------------------------------------------
    // context preservation
    // ---------------------------------------------------------------

    /// バイト長を指定して Unit を並べた文書を作る（Atom 1 つ = Unit 1 つ）。
    fn sized(units: &[(ReadingTier, usize)]) -> SemanticDocument {
        let mut atoms = Vec::new();
        let mut at = 0;
        for &(_, len) in units {
            atoms.push(Atom::new(at..at + len, AtomKind::Sentence));
            at += len;
        }
        let units = units
            .iter()
            .enumerate()
            .map(|(i, &(tier, _))| SemanticUnit::new(format!("u{i}"), [AtomIndex(i)], tier))
            .collect();
        SemanticDocument::new(atoms, units)
    }

    /// `owner` が `target` を前提にする、と書き足す。
    fn presuppose(doc: &mut SemanticDocument, owner: usize, target: usize) {
        doc.units[owner]
            .relations
            .push(Relation::Presupposes(format!("u{target}").into()));
    }

    /// その Budget で DIM でない Unit の添字（= 残った Unit）。
    fn kept_units(doc: &SemanticDocument, budget: u8) -> Vec<usize> {
        let states = states(doc, budget);
        doc.units
            .iter()
            .enumerate()
            .filter(|(_, unit)| {
                unit.atoms
                    .iter()
                    .any(|&AtomIndex(a)| states[a] != DisplayState::Dim)
            })
            .map(|(index, _)| index)
            .collect()
    }

    /// **合格条件の本体。** Budget 1..=100 を全部回して、残る集合が必ず
    /// 入れ子になっていることを確かめる。
    ///
    /// 前提を足すと 1 つの Unit を残す値段が変わるので、単調性がいちばん
    /// 危ないのがここである。支えているのは「`keep_order` が Budget に
    /// 依存しない」と「打ち切りが `break` である」の 2 点だけで、
    /// **辺の構造は一切使っていない** — だから循環を含む fixture も同じ
    /// ループで回している。
    fn assert_monotone(doc: &SemanticDocument, label: &str) {
        let mut previous: Vec<usize> = Vec::new();
        for budget in MIN_BUDGET..=MAX_BUDGET {
            let now = kept_units(doc, budget);
            for index in &previous {
                assert!(
                    now.contains(index),
                    "{label}: budget {budget} で u{index} が消えた \
                     （{previous:?} -> {now:?}）"
                );
            }
            previous = now;
        }
    }

    #[test]
    fn keeping_a_unit_stays_monotone_when_prerequisites_are_charged() {
        // 鎖（深さ 2）。ESSENTIAL が CONTEXT を、それがさらに DETAIL を。
        let mut chain = sized(&[
            (ReadingTier::Detail, 40),
            (ReadingTier::Context, 30),
            (ReadingTier::Essential, 10),
            (ReadingTier::Supporting, 20),
        ]);
        presuppose(&mut chain, 2, 1);
        presuppose(&mut chain, 1, 0);
        assert_monotone(&chain, "chain");

        // 共有前提 — 2 つの Unit が同じ Unit を前提にする。
        let mut shared = sized(&[
            (ReadingTier::Context, 25),
            (ReadingTier::Essential, 15),
            (ReadingTier::Essential, 15),
            (ReadingTier::Detail, 45),
        ]);
        presuppose(&mut shared, 1, 0);
        presuppose(&mut shared, 2, 0);
        assert_monotone(&shared, "shared");

        // 直接の前提が 2 本（判定器の当て木の上限）。
        let mut two = sized(&[
            (ReadingTier::Context, 20),
            (ReadingTier::Detail, 30),
            (ReadingTier::Essential, 10),
            (ReadingTier::Supporting, 40),
        ]);
        presuppose(&mut two, 2, 0);
        presuppose(&mut two, 2, 1);
        assert_monotone(&two, "two");

        // 払えないほど高い閉包。ESSENTIAL が文書の大半を引き連れている。
        let mut heavy = sized(&[
            (ReadingTier::Detail, 400),
            (ReadingTier::Essential, 10),
            (ReadingTier::Supporting, 30),
            (ReadingTier::Context, 60),
        ]);
        presuppose(&mut heavy, 1, 0);
        assert_monotone(&heavy, "heavy");

        // 循環。構造上は起きないが、起きても順序も打ち切りも変わらない。
        let mut cyclic = sized(&[
            (ReadingTier::Essential, 10),
            (ReadingTier::Supporting, 20),
            (ReadingTier::Context, 30),
            (ReadingTier::Detail, 40),
        ]);
        presuppose(&mut cyclic, 0, 2);
        presuppose(&mut cyclic, 2, 0);
        presuppose(&mut cyclic, 1, 1 /* 自己参照は validate が弾くが、ここは 3 へ */);
        cyclic.units[1].relations.clear();
        presuppose(&mut cyclic, 1, 3);
        presuppose(&mut cyclic, 3, 1);
        assert_monotone(&cyclic, "cyclic");

        // **一段目が予算を食い切る。** 核 2 つが同じ重い CONTEXT を前提に
        // していて、一段目だけで文書の 9 割になる。二段目の予算が負
        // （実際には 0 扱い）になる領域を通る。
        let mut crowded = sized(&[
            (ReadingTier::Context, 200),
            (ReadingTier::Essential, 10),
            (ReadingTier::Essential, 10),
            (ReadingTier::Detail, 20),
        ]);
        presuppose(&mut crowded, 1, 0);
        presuppose(&mut crowded, 2, 0);
        assert_monotone(&crowded, "crowded");

        // **二段目の Unit も辺を持つ文書。** 二段目は連れてこないが、
        // 一段目の閉包と行き先が重なることはある。
        let mut mixed = sized(&[
            (ReadingTier::Context, 30),
            (ReadingTier::Supporting, 20),
            (ReadingTier::Essential, 10),
            (ReadingTier::Detail, 40),
        ]);
        presuppose(&mut mixed, 2, 0);
        presuppose(&mut mixed, 1, 0);
        presuppose(&mut mixed, 3, 1);
        assert_monotone(&mixed, "mixed");
    }

    /// その Budget で DIM でない Atom のバイト数の合計。
    fn shown_bytes(doc: &SemanticDocument, budget: u8) -> usize {
        decorate(doc, budget)
            .iter()
            .filter(|(_, state)| *state != DisplayState::Dim)
            .map(|(range, _)| range.len())
            .sum()
    }

    /// **下限の合格条件の本体。** 単調性（`assert_monotone`）の上に乗せて、
    /// 下限より下では残る集合が動かないこと、下限から上では `READ` の数字
    /// が表示量に対して嘘をつかないことを Budget 1..=100 で確かめる。
    fn assert_floor(doc: &SemanticDocument, label: &str) {
        assert_monotone(doc, label);
        let floor = floor(doc);
        let total: usize = doc.atoms.iter().map(|atom| atom.len()).sum();
        let at_floor = kept_units(doc, floor);
        let below = kept_units(doc, MIN_BUDGET);
        for budget in MIN_BUDGET..=MAX_BUDGET {
            let now = kept_units(doc, budget);
            let shown = shown_bytes(doc, budget) as u128 * 100;
            if budget < floor {
                // 下限より下は一段目そのもので、Budget をいくら下げても
                // 動かない。そこでは表示量が Budget を超えている（数字が嘘）。
                assert_eq!(now, below, "{label}: budget {budget} < floor {floor}");
                assert!(
                    shown > budget as u128 * total as u128,
                    "{label}: budget {budget} < floor {floor} なのに表示量が収まっている"
                );
            } else {
                // 下限以上では表示量は Budget 以下（数字は嘘をつかない）、
                // 残る集合は下限のものを含んで単調に広がる。
                assert!(
                    shown <= budget as u128 * total as u128,
                    "{label}: budget {budget} >= floor {floor} で表示量が超えた"
                );
                for index in &at_floor {
                    assert!(now.contains(index), "{label}: budget {budget} で u{index} が消えた");
                }
            }
        }
        // 下限は一段目の切り上げなので、下限の残る集合は一段目を含む。
        for index in &below {
            assert!(at_floor.contains(index), "{label}: floor {floor} で u{index} が消えた");
        }
    }

    #[test]
    fn the_floor_is_where_the_number_stops_lying() {
        // 核 1 つ、前提なし。一段目 = 10 / 100 → 下限 10 %。
        let plain = sized(&[
            (ReadingTier::Essential, 10),
            (ReadingTier::Supporting, 30),
            (ReadingTier::Context, 30),
            (ReadingTier::Detail, 30),
        ]);
        assert_eq!(floor(&plain), 10);
        assert_floor(&plain, "plain");

        // 核が長い系譜を引き連れる。一段目 = 10 + 30 + 40 = 80 / 100。
        let mut chain = sized(&[
            (ReadingTier::Detail, 40),
            (ReadingTier::Context, 30),
            (ReadingTier::Essential, 10),
            (ReadingTier::Supporting, 20),
        ]);
        presuppose(&mut chain, 2, 1);
        presuppose(&mut chain, 1, 0);
        assert_eq!(floor(&chain), 80);
        assert_floor(&chain, "chain");

        // 切り上げ。一段目 = 10 / 30 = 33.3… → 34 %。
        let thirds = sized(&[
            (ReadingTier::Essential, 10),
            (ReadingTier::Detail, 10),
            (ReadingTier::Detail, 10),
        ]);
        assert_eq!(floor(&thirds), 34);
        assert_floor(&thirds, "thirds");

        // 一段目が予算を食い切る（9 割超）。
        let mut crowded = sized(&[
            (ReadingTier::Context, 200),
            (ReadingTier::Essential, 10),
            (ReadingTier::Essential, 10),
            (ReadingTier::Detail, 20),
        ]);
        presuppose(&mut crowded, 1, 0);
        presuppose(&mut crowded, 2, 0);
        assert_eq!(floor(&crowded), 92);
        assert_floor(&crowded, "crowded");
    }

    /// 下限より下で `decorate` は `decorate(doc, floor - 1)` と一致し、
    /// 表示量は一段目そのもの。`floor` が 1 なら「下」は無い。
    #[test]
    fn below_the_floor_the_budget_does_nothing() {
        let mut chain = sized(&[
            (ReadingTier::Detail, 40),
            (ReadingTier::Context, 30),
            (ReadingTier::Essential, 10),
            (ReadingTier::Supporting, 20),
        ]);
        presuppose(&mut chain, 2, 1);
        presuppose(&mut chain, 1, 0);
        let floor = floor(&chain);
        let frozen = decorate(&chain, floor - 1);
        for budget in MIN_BUDGET..floor {
            assert_eq!(decorate(&chain, budget), frozen, "budget {budget}");
        }
        assert_eq!(shown_bytes(&chain, floor - 1), 80);
        assert_eq!(kept_units(&chain, floor), [0, 1, 2], "80 % では残りの 20 は入らない");
        assert_eq!(kept_units(&chain, MAX_BUDGET), [0, 1, 2, 3]);
    }

    /// 核が無い文書では先頭 1 Unit の強制残留が一段目に相当する。
    #[test]
    fn without_a_core_the_floor_is_the_forced_first_unit() {
        let doc = sized(&[
            (ReadingTier::Supporting, 25),
            (ReadingTier::Context, 25),
            (ReadingTier::Detail, 50),
        ]);
        assert_eq!(floor(&doc), 25);
        assert_floor(&doc, "no core");

        // 核を持たない ESSENTIAL（`Some([])`）も一段目に入らない。
        let mut unsettled = sized(&[
            (ReadingTier::Essential, 50),
            (ReadingTier::Detail, 50),
        ]);
        unsettled.units[0].set_core([]);
        assert_eq!(floor(&unsettled), 50);
        assert_floor(&unsettled, "empty core");
    }

    /// 全文が核なら下限は 100。Budget は何をしても動かない。
    #[test]
    fn a_document_that_is_all_core_has_a_floor_of_one_hundred() {
        let doc = sized(&[(ReadingTier::Essential, 10), (ReadingTier::Essential, 20)]);
        assert_eq!(floor(&doc), MAX_BUDGET);
        assert_floor(&doc, "all core");
    }

    #[test]
    fn the_floor_of_an_empty_or_zero_length_document_is_the_minimum() {
        assert_eq!(floor(&SemanticDocument::default()), MIN_BUDGET);
        let zero = sized(&[(ReadingTier::Essential, 0), (ReadingTier::Detail, 0)]);
        assert_eq!(floor(&zero), MIN_BUDGET);
        let broken = SemanticDocument::new(
            vec![Atom::new(0..10, AtomKind::Sentence)],
            vec![SemanticUnit::new("u0", [AtomIndex(7)], ReadingTier::Essential)],
        );
        assert_eq!(floor(&broken), MIN_BUDGET);
    }

    /// 下限は Budget の関数ではなく、`decorate` の一段目と同じ集合で数える。
    /// 一段目が読む集合（`kept_units(doc, MIN_BUDGET)`）のバイト数を切り上げた
    /// 値と一致すること。
    #[test]
    fn the_floor_agrees_with_the_first_tier_of_decorate() {
        let mut shared = sized(&[
            (ReadingTier::Context, 25),
            (ReadingTier::Essential, 15),
            (ReadingTier::Essential, 15),
            (ReadingTier::Detail, 45),
        ]);
        presuppose(&mut shared, 1, 0);
        presuppose(&mut shared, 2, 0);
        let first_tier: usize = kept_units(&shared, MIN_BUDGET)
            .iter()
            .map(|&i| shared.atoms[i].len())
            .sum();
        assert_eq!(first_tier, 55);
        assert_eq!(floor(&shared) as usize, first_tier);
        assert_floor(&shared, "shared");
    }

    /// その Budget で MARKED を持つ Unit の添字。
    fn marked_units(doc: &SemanticDocument, budget: u8) -> Vec<usize> {
        let states = states(doc, budget);
        doc.units
            .iter()
            .enumerate()
            .filter(|(_, unit)| {
                unit.atoms
                    .iter()
                    .any(|&AtomIndex(a)| states[a] == DisplayState::Marked)
            })
            .map(|(index, _)| index)
            .collect()
    }

    /// **「MARKED は Budget に依存しない」を文字どおり固定する。**
    ///
    /// 台帳が一段だった頃、この主張は「生き残った Unit については真、
    /// 落ちた Unit については偽」だった — READ を下げると MARKED が消えた。
    /// 二段にしたので、いまは Budget 1 % と 100 % で MARKED の集合が
    /// **完全に一致する**。
    #[test]
    fn the_marked_set_never_moves_with_the_budget() {
        let mut lineage = sized(&[
            (ReadingTier::Context, 300),
            (ReadingTier::Essential, 10),
            (ReadingTier::Detail, 600),
            (ReadingTier::Essential, 20),
            (ReadingTier::Supporting, 70),
        ]);
        presuppose(&mut lineage, 1, 0);
        presuppose(&mut lineage, 3, 1);

        let mut redundant = sized(&[
            (ReadingTier::Essential, 40),
            (ReadingTier::Essential, 10),
            (ReadingTier::Detail, 50),
        ]);
        redundant.units[0]
            .relations
            .push(Relation::RedundantWith("u1".into()));

        let mut hollow = sized(&[
            (ReadingTier::Essential, 30),
            (ReadingTier::Essential, 10),
            (ReadingTier::Supporting, 60),
        ]);
        hollow.units[0].set_core([]);

        for (doc, label) in [
            (&lineage, "lineage"),
            (&redundant, "redundant"),
            (&hollow, "hollow"),
        ] {
            let full = marked_units(doc, MAX_BUDGET);
            assert!(!full.is_empty(), "{label}: 100 % で MARKED が無い fixture");
            for budget in MIN_BUDGET..=MAX_BUDGET {
                assert_eq!(
                    marked_units(doc, budget),
                    full,
                    "{label}: budget {budget} で MARKED が動いた"
                );
            }
        }
    }

    /// **合格条件。** READ 1 % で、核とその前提だけが出る。
    #[test]
    fn every_core_and_its_lineage_survives_the_smallest_budget() {
        let mut doc = sized(&[
            (ReadingTier::Context, 300),
            (ReadingTier::Essential, 10),
            (ReadingTier::Detail, 600),
            (ReadingTier::Essential, 20),
            (ReadingTier::Supporting, 70),
        ]);
        presuppose(&mut doc, 1, 0);
        // 一段目は u1 + 前提の u0 + u3 で 330 バイト。文書は 1000 バイトで、
        // READ 1 % は 10 バイトしかないが、**核は 2 つとも出る**。
        assert_eq!(kept_units(&doc, 1), [0, 1, 3]);
        assert_eq!(
            states(&doc, 1),
            [
                DisplayState::Normal,
                DisplayState::Marked,
                DisplayState::Dim,
                DisplayState::Marked,
                DisplayState::Dim
            ]
        );
    }

    /// **二段目は自分の前提を連れてこない。** 二段にして変わったのはここで、
    /// 実データでは SUPPORTING の Unit が自分の前提を引き上げていた。
    #[test]
    fn a_second_tier_unit_does_not_bring_its_prerequisite() {
        let mut doc = sized(&[
            (ReadingTier::Detail, 30),
            (ReadingTier::Supporting, 10),
            (ReadingTier::Essential, 10),
            (ReadingTier::Detail, 50),
        ]);
        presuppose(&mut doc, 1, 0);
        // 一段目は核の u2 だけ（10 バイト）。25 % = 25 バイトの残りで
        // u1 は買えるが、**前提の u0（30 バイト）は連れてこない**。
        // 台帳が一段だった頃は u1 が 40 バイトになって買えず、`[2]` だった。
        assert_eq!(kept_units(&doc, 25), [1, 2]);
    }

    /// 核が 1 つも無い文書。読む場所がゼロにならないよう先頭は残すが、
    /// **そこは二段目なので前提は連れてこない**。
    ///
    /// 一段目だけを閉包つきにしたのは、`context preservation` が
    /// 「光っている一文が読める」ための rule だからである。光る Unit が
    /// 無いなら、引き上げる先も無い。
    #[test]
    fn without_a_core_the_forced_first_unit_pays_no_lineage() {
        let mut doc = sized(&[
            (ReadingTier::Context, 500),
            (ReadingTier::Supporting, 10),
            (ReadingTier::Detail, 90),
        ]);
        presuppose(&mut doc, 1, 0);
        assert_eq!(kept_units(&doc, 1), [1]);
        assert!(marked_units(&doc, 1).is_empty());
    }

    /// 核を持たない ESSENTIAL（`Some([])`）は一段目に入らない。
    /// 一段目の条件は**表示状態の割り当てと同じ 3 つ**である。
    #[test]
    fn an_essential_unit_without_a_core_is_not_reserved_first() {
        let mut doc = sized(&[
            (ReadingTier::Essential, 60),
            (ReadingTier::Essential, 10),
            (ReadingTier::Detail, 30),
        ]);
        doc.units[0].set_core([]);
        // u0 は核を持たないので取り合いに出る。30 % = 30 バイトに 60 は
        // 入らない。核を持つ u1 だけが一段目で残る。
        assert_eq!(kept_units(&doc, 30), [1]);
        assert_eq!(marked_units(&doc, 30), [1]);
    }

    /// 単調性の支えは辺ではなく `break` である、を裏から固定する。
    /// 前提が 1 本も無い文書では、閉包の実装が入っても結果が 1 ビットも
    /// 変わらない。
    #[test]
    fn a_document_without_prerequisites_decorates_exactly_as_before() {
        let doc = doc(&[
            (ReadingTier::Essential, false),
            (ReadingTier::Supporting, false),
            (ReadingTier::Context, false),
            (ReadingTier::Detail, false),
        ]);
        assert_eq!(
            states(&doc, 50),
            [
                DisplayState::Marked,
                DisplayState::Normal,
                DisplayState::Dim,
                DisplayState::Dim
            ]
        );
        assert_monotone(&doc, "no edges");
    }

    /// 前提は**一緒に残る**。これが rule の本体である。
    ///
    /// u0 は DETAIL なので、普通なら順序のいちばん後ろで沈む。u2
    /// （ESSENTIAL）がそれを前提にしていると、u2 を残す値段に u0 が乗り、
    /// **2 つ一緒に残る**。実測の穴 A がこの形だった（MARKED の一文が
    /// 人物名で人物を指し、その人物の説明が沈んでいた）。
    #[test]
    fn a_prerequisite_survives_with_the_unit_that_needs_it() {
        let plain = sized(&[
            (ReadingTier::Detail, 25),
            (ReadingTier::Essential, 5),
            (ReadingTier::Essential, 10),
            (ReadingTier::Supporting, 60),
        ]);
        // 前提が無ければ、40 % でも DETAIL の u0 は沈んだまま。
        assert_eq!(kept_units(&plain, 40), [1, 2]);

        let mut linked = plain.clone();
        presuppose(&mut linked, 2, 0);
        assert_eq!(kept_units(&linked, 40), [0, 1, 2]);
        // 引き上げられた前提は **NORMAL**。DIM でもないし、MARKED でもない
        // （沈めないだけで、格上げしたわけではない）。
        assert_eq!(
            states(&linked, 40),
            [
                DisplayState::Normal,
                DisplayState::Marked,
                DisplayState::Marked,
                DisplayState::Dim
            ]
        );
    }

    /// 前提は**根まで**辿る。前提の前提も一緒に払う。
    #[test]
    fn prerequisites_are_followed_to_the_root() {
        let mut doc = sized(&[
            (ReadingTier::Detail, 10),
            (ReadingTier::Context, 10),
            (ReadingTier::Essential, 10),
            (ReadingTier::Detail, 70),
        ]);
        presuppose(&mut doc, 2, 1);
        presuppose(&mut doc, 1, 0);
        // 30 % = 30 バイト。鎖 3 つでちょうど払える。u0 は ESSENTIAL の
        // **前提の前提**で、直接は誰も指していない。
        assert_eq!(kept_units(&doc, 30), [0, 1, 2]);
        // 20 % では鎖が払えないが、ESSENTIAL は核なので一段目で閉包ごと
        // 確保される（「一段目は Budget を見ない」）。
        assert_eq!(kept_units(&doc, 20), [0, 1, 2]);
    }

    /// **逆転はもう起きない。** 一段目が Budget を見ないので、払えないほど
    /// 長い系譜を持つ核も、系譜ごと確保される。
    ///
    /// 台帳が一段だった頃はここが逆で、この fixture では MARKED の u2 が
    /// 落ちて DETAIL の u0 が繰り上がっていた（実測 b3 の 2 / 5 と同じ形）。
    /// **拾い直して直したのではない** — 拾い直しは best-fit で単調性を壊す。
    /// 核を取り合いに出さないことで、そもそも落ちなくなった。
    #[test]
    fn an_unpayable_lineage_no_longer_takes_its_core_down() {
        let mut doc = sized(&[
            (ReadingTier::Detail, 55),
            (ReadingTier::Essential, 10),
            (ReadingTier::Essential, 15),
            (ReadingTier::Context, 20),
        ]);
        // 前提が無ければ 30 % で ESSENTIAL が 2 つとも残り、どちらも光る。
        assert_eq!(kept_units(&doc, 30), [1, 2]);
        assert_eq!(states(&doc, 30)[1..3], [DisplayState::Marked; 2]);

        presuppose(&mut doc, 1, 0);
        // 一段目は u1 + その前提の u0（55 バイト）+ u2 で 80 バイト。
        // 予算 30 % = 30 バイトを大きく超えるが、**一段目は Budget を
        // 見ない**。核は 2 つとも光ったままで、落ちるのは二段目の u3。
        assert_eq!(kept_units(&doc, 30), [0, 1, 2]);
        assert_eq!(
            states(&doc, 30),
            [
                DisplayState::Normal,
                DisplayState::Marked,
                DisplayState::Marked,
                DisplayState::Dim
            ]
        );
        // READ 1 % でも同じ集合。**「これさえ見れば」が消えない。**
        assert_eq!(kept_units(&doc, 1), [0, 1, 2]);
    }

    /// すでに払った前提は二度請求されない（共有前提）。
    #[test]
    fn a_shared_prerequisite_is_paid_for_once() {
        let mut doc = sized(&[
            (ReadingTier::Context, 40),
            (ReadingTier::Essential, 10),
            (ReadingTier::Essential, 10),
            (ReadingTier::Detail, 40),
        ]);
        presuppose(&mut doc, 1, 0);
        presuppose(&mut doc, 2, 0);
        // 二重計上なら 40 + 10 + 40 + 10 = 100 バイトで 60 % を超える。
        // 1 回だけなら 60 バイトちょうどで収まる。
        assert_eq!(kept_units(&doc, 60), [0, 1, 2]);
    }

    /// **一段目は閉包ごと残る。** Budget 1 % の表示は「MARKED 1 つ」では
    /// なく「MARKED 1 つ + その前提」になる。意味の取れない一文だけが
    /// 光っている状態は、この rule が禁じている当のものである。
    #[test]
    fn a_core_brings_its_prerequisites_at_the_smallest_budget() {
        let mut doc = sized(&[
            (ReadingTier::Context, 500),
            (ReadingTier::Essential, 10),
            (ReadingTier::Detail, 90),
        ]);
        presuppose(&mut doc, 1, 0);
        assert_eq!(kept_units(&doc, 1), [0, 1]);
        assert_eq!(
            states(&doc, 1),
            [
                DisplayState::Normal,
                DisplayState::Marked,
                DisplayState::Dim
            ]
        );
    }

    /// 循環があっても止まる（無限ループにならない）。
    #[test]
    fn a_cycle_in_the_prerequisites_terminates() {
        let mut doc = sized(&[
            (ReadingTier::Essential, 10),
            (ReadingTier::Context, 10),
            (ReadingTier::Detail, 80),
        ]);
        presuppose(&mut doc, 0, 1);
        presuppose(&mut doc, 1, 0);
        assert_eq!(kept_units(&doc, 20), [0, 1]);
    }

    /// 知らない id を指す前提は黙って飛ばす — `decorate` は壊れた入力でも
    /// panic しない（弾くのは [`SemanticDocument::validate`] の仕事）。
    #[test]
    fn a_prerequisite_pointing_nowhere_does_not_panic() {
        let mut doc = sized(&[
            (ReadingTier::Essential, 10),
            (ReadingTier::Detail, 90),
        ]);
        doc.units[0]
            .relations
            .push(Relation::Presupposes("nope".into()));
        assert_eq!(kept_units(&doc, 20), [0]);
    }

    /// 前提を持つだけの ESSENTIAL は **MARKED のまま**。`relations` が
    /// 空でないことを redundancy と読むと、ここが NORMAL へ落ちる。
    #[test]
    fn a_unit_that_presupposes_is_still_marked() {
        let mut doc = sized(&[
            (ReadingTier::Context, 10),
            (ReadingTier::Essential, 10),
        ]);
        presuppose(&mut doc, 1, 0);
        assert_eq!(
            states(&doc, 100),
            [DisplayState::Normal, DisplayState::Marked]
        );
    }

    /// 前提でも冗長でもある Unit は、**前提の側がそのまま効く**。
    /// 2 つの relation は独立で、冗長は核を取り上げない。
    ///
    /// **2026-09-22 に逆になった。** それまでは REDUNDANT だと MARKED に
    /// ならず、一段目にも入らないので前提を連れてこなかった。
    #[test]
    fn a_redundant_essential_unit_still_brings_its_prerequisite() {
        let mut doc = sized(&[
            (ReadingTier::Context, 10),
            (ReadingTier::Essential, 10),
        ]);
        presuppose(&mut doc, 1, 0);
        doc.units[1]
            .relations
            .push(Relation::RedundantWith("u0".into()));
        assert!(doc.units[1].is_redundant());
        // 冗長でも ESSENTIAL で核を持つので MARKED → **一段目に入る**。
        assert_eq!(
            states(&doc, 100),
            [DisplayState::Normal, DisplayState::Marked]
        );
        // 一段目は Budget を見ないので、最小の Budget でも前提ごと残る。
        assert_eq!(kept_units(&doc, MIN_BUDGET), [0, 1]);
        assert_eq!(floor(&doc), MAX_BUDGET);
    }

    // ---- 見出しの復帰 ---------------------------------------------------

    /// 節を持つ文書を組み立てる。`spec` は `(見出しか, Tier, バイト長,
    /// 属する節の Unit 番号)` で、Atom 1 つ = Unit 1 つ。
    fn sections(spec: &[(bool, ReadingTier, usize, Option<usize>)]) -> SemanticDocument {
        let mut atoms = Vec::new();
        let mut at = 0;
        for &(heading, _, len, _) in spec {
            let kind = if heading {
                AtomKind::Heading
            } else {
                AtomKind::Sentence
            };
            atoms.push(Atom::new(at..at + len, kind));
            at += len;
        }
        let units = spec
            .iter()
            .enumerate()
            .map(|(i, &(_, tier, _, head))| {
                let mut unit = SemanticUnit::new(format!("u{i}"), [AtomIndex(i)], tier);
                unit.section_of = head.map(|h| format!("u{h}").into());
                unit
            })
            .collect();
        SemanticDocument::new(atoms, units)
    }

    /// **rule の本体。** 節の中の Unit が 1 つでも残るなら、その節の見出しも
    /// 残す。規則 4 で中身が複数 Unit に割れたあと、境界規則 2 の意図を
    /// 守るのはここだけである。
    #[test]
    fn a_section_head_comes_back_when_anything_in_the_section_survives() {
        // u0 見出し（10）／ u1 短い本文（10）／ u2 長い本文（80）。
        let doc = sections(&[
            (true, ReadingTier::Context, 10, None),
            (false, ReadingTier::Essential, 10, Some(0)),
            (false, ReadingTier::Detail, 80, Some(0)),
        ]);
        // 20 % = 20 バイト。ESSENTIAL の u1 だけが入り、見出しの u0 は
        // 予算の取り合いに負けている。それでも見出しは戻る。
        assert_eq!(
            states(&doc, 20),
            [
                DisplayState::Normal,
                DisplayState::Marked,
                DisplayState::Dim
            ]
        );
    }

    /// **逆向きは直さない。** 節ごと沈むなら見出しも沈んだままである。
    /// 実測でも、節ごと沈む 6 件すべてで見出しは DIM だった。
    #[test]
    fn a_section_that_sinks_whole_keeps_its_head_dim() {
        let doc = sections(&[
            (false, ReadingTier::Essential, 10, None),
            (true, ReadingTier::Detail, 10, None),
            (false, ReadingTier::Detail, 80, Some(1)),
        ]);
        assert_eq!(
            states(&doc, 15),
            [DisplayState::Marked, DisplayState::Dim, DisplayState::Dim]
        );
    }

    /// 見出しの Unit に本文が同居していたら、**戻るのは見出しの行だけ**。
    /// Unit ごと戻すと、その第 1 文も一緒に戻ってくる。
    #[test]
    fn only_the_heading_atom_comes_back_not_the_rest_of_its_unit() {
        let atoms = vec![
            Atom::new(0..10, AtomKind::Heading),
            Atom::new(10..50, AtomKind::Sentence),
            Atom::new(50..60, AtomKind::Sentence),
        ];
        // u0 は「見出し + 第 1 文」。規則 2 がこの形を作る。
        let head = SemanticUnit::new("u0", [AtomIndex(0), AtomIndex(1)], ReadingTier::Detail);
        let mut body = SemanticUnit::new("u1", [AtomIndex(2)], ReadingTier::Essential);
        body.section_of = Some("u0".into());
        let doc = SemanticDocument::new(atoms, vec![head, body]);
        assert_eq!(
            states(&doc, 20),
            [
                DisplayState::Normal,
                DisplayState::Dim,
                DisplayState::Marked
            ]
        );
    }

    /// 入れ子は勝手に伝わる。`###` が戻れば `##` が戻り、それが `#` を戻す。
    /// **この層は `#` の数を知らない** — 見出し Unit 自身の `section_of` が
    /// 親を指しているだけで連鎖する。
    #[test]
    fn restoring_a_nested_head_walks_up_to_the_root() {
        let doc = sections(&[
            (true, ReadingTier::Context, 10, None),       // u0 `#`
            (true, ReadingTier::Context, 10, Some(0)),    // u1 `##`
            (true, ReadingTier::Context, 10, Some(1)),    // u2 `###`
            (false, ReadingTier::Essential, 10, Some(2)), // u3 中身
            (false, ReadingTier::Detail, 60, Some(2)),    // u4 長い中身
        ]);
        // 10 % = 10 バイト。入るのは核を持つ u3 だけ。そこから 3 段戻る。
        assert_eq!(
            states(&doc, 10),
            [
                DisplayState::Normal,
                DisplayState::Normal,
                DisplayState::Normal,
                DisplayState::Marked,
                DisplayState::Dim
            ]
        );
    }

    /// 戻した見出しは **NORMAL であって MARKED ではない**。予算の取り合いに
    /// 負けた見出しが光ると、ラベルだけが光る状態になる
    /// （`docs/gotchas/semantic-reading.md`「箇条書きのラベルが MARKED に
    /// なる」）。
    #[test]
    fn a_restored_head_is_normal_never_marked() {
        let mut doc = sections(&[
            (true, ReadingTier::Essential, 10, None),
            (false, ReadingTier::Essential, 10, Some(0)),
            (false, ReadingTier::Detail, 80, Some(0)),
        ]);
        // 見出しは ESSENTIAL だが**核を持たない**ので一段目に入らない。
        doc.units[0].set_core([]);
        assert_eq!(
            states(&doc, 20),
            [
                DisplayState::Normal,
                DisplayState::Marked,
                DisplayState::Dim
            ]
        );
    }

    /// **予算には数えない。** 見出しが戻っても、残る Unit の集合は
    /// `section_of` が 1 本も無い文書と 1 ビットも変わらない。数えると
    /// 「見出しが戻ったせいで本文が沈む」が起きる。
    #[test]
    fn restoring_a_head_does_not_charge_the_budget() {
        let spec = &[
            (true, ReadingTier::Context, 10, None),
            (false, ReadingTier::Essential, 10, Some(0)),
            (false, ReadingTier::Supporting, 20, Some(0)),
            (false, ReadingTier::Detail, 60, Some(0)),
        ];
        let with = sections(spec);
        let mut without = sections(spec);
        for unit in &mut without.units {
            unit.section_of = None;
        }
        for budget in MIN_BUDGET..=MAX_BUDGET {
            // 見出し（atom 0）の外は 1 ビットも変わらない。
            assert_eq!(
                states(&with, budget)[1..],
                states(&without, budget)[1..],
                "budget {budget}"
            );
        }
        // そして見出しのほうは、実際に戻っている Budget がある。
        assert_eq!(states(&without, 20)[0], DisplayState::Dim);
        assert_eq!(states(&with, 20)[0], DisplayState::Normal);
    }

    /// 単調性を **Atom で**見る。`kept_units` は Unit 単位なので、見出しが
    /// 1 Atom だけ戻った Unit を「丸ごと残った」と数えてしまう。
    fn assert_atoms_monotone(doc: &SemanticDocument, label: &str) {
        let mut previous = states(doc, MIN_BUDGET);
        for budget in MIN_BUDGET..=MAX_BUDGET {
            let now = states(doc, budget);
            for (index, (before, after)) in previous.iter().zip(&now).enumerate() {
                assert!(
                    after.attention() >= before.attention(),
                    "{label}: budget {budget} で atom {index} が                      {before:?} -> {after:?} と弱まった"
                );
            }
            previous = now;
        }
    }

    /// 予算を上げたら見えるものが増えるだけ。見出しが戻る条件は「節に残る
    /// Unit があるか」なので、`kept` が入れ子である限りその閉包も入れ子で
    /// ある。
    #[test]
    fn heading_restoration_stays_monotone() {
        let nested = sections(&[
            (true, ReadingTier::Context, 10, None),
            (true, ReadingTier::Detail, 10, Some(0)),
            (false, ReadingTier::Essential, 15, Some(1)),
            (false, ReadingTier::Supporting, 40, Some(1)),
            (true, ReadingTier::Context, 10, Some(0)),
            (false, ReadingTier::Detail, 70, Some(4)),
        ]);
        assert_atoms_monotone(&nested, "nested");
        assert_monotone(&nested, "nested");

        // 前提と混ざっても崩れない（一段目・二段目・復帰の 3 つが同居する）。
        let mut mixed = sections(&[
            (true, ReadingTier::Context, 10, None),
            (false, ReadingTier::Detail, 30, Some(0)),
            (false, ReadingTier::Essential, 10, Some(0)),
            (true, ReadingTier::Context, 10, Some(0)),
            (false, ReadingTier::Supporting, 50, Some(3)),
        ]);
        presuppose(&mut mixed, 2, 1);
        assert_atoms_monotone(&mixed, "mixed");
    }

    /// 節の辺が循環していても止まる（無限ループにならない）。
    #[test]
    fn a_cycle_in_the_section_heads_terminates() {
        let mut doc = sections(&[
            (true, ReadingTier::Context, 10, Some(1)),
            (true, ReadingTier::Context, 10, Some(0)),
            (false, ReadingTier::Essential, 10, Some(0)),
        ]);
        doc.units[2].section_of = Some("u0".into());
        assert_eq!(
            states(&doc, 40)[2],
            DisplayState::Marked,
            "循環を辿っても戻ってくる"
        );
    }

    /// 知らない id を指す `section_of` は黙って飛ばす — `decorate` は壊れた
    /// 入力でも panic しない（弾くのは [`SemanticDocument::validate`] の
    /// 仕事）。
    #[test]
    fn a_section_head_pointing_nowhere_does_not_panic() {
        let mut doc = sections(&[
            (true, ReadingTier::Context, 10, None),
            (false, ReadingTier::Essential, 10, Some(0)),
        ]);
        doc.units[1].section_of = Some("nope".into());
        assert_eq!(states(&doc, 30), [DisplayState::Dim, DisplayState::Marked]);
    }

    /// `section_of` を知らない判定器・既存の fixture では、表示が 1 ビットも
    /// 変わらない。
    #[test]
    fn a_document_without_sections_decorates_exactly_as_before() {
        let doc = doc(&[
            (ReadingTier::Essential, false),
            (ReadingTier::Context, false),
            (ReadingTier::Detail, false),
        ]);
        assert!(doc.units.iter().all(|unit| unit.section_of.is_none()));
        assert_eq!(
            states(&doc, 70),
            [
                DisplayState::Marked,
                DisplayState::Normal,
                DisplayState::Dim
            ]
        );
    }

    #[test]
    fn an_empty_document_decorates_to_nothing() {
        assert!(decorate(&SemanticDocument::default(), 50).is_empty());
    }

    #[test]
    fn broken_atom_indices_do_not_panic() {
        let mut doc = doc(&[(ReadingTier::Essential, false)]);
        doc.units[0].atoms.push(AtomIndex(99));
        assert_eq!(states(&doc, 50), [DisplayState::Marked]);
    }

    #[test]
    fn zero_length_atoms_do_not_divide_by_zero() {
        let atoms = vec![Atom::new(5..5, AtomKind::Sentence)];
        let units = vec![SemanticUnit::new("u0", [AtomIndex(0)], ReadingTier::Detail)];
        let doc = SemanticDocument::new(atoms, units);
        assert_eq!(states(&doc, 1), [DisplayState::Normal]);
    }
}
