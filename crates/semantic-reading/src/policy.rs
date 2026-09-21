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
//!    （= MARKED になりうるもの。ESSENTIAL かつ非 REDUNDANT かつ
//!    `core_atoms != Some([])`）を、**Budget を見ずに**残す。そのとき
//!    **その Unit が前提にしている Unit（`PRESUPPOSES` の閉包）も一緒に
//!    残す**（下の「context preservation」）。
//! 3. **二段目 — 残りの予算を、残りの Unit が奪い合う。** `keep_order` の
//!    先頭から順に、attention が尽きるまで残す。attention の量は Unit が
//!    占める source のバイト数で測り、`budget` はその何 % までを残すかを
//!    表す。入らない Unit が現れた時点で打ち切る（後ろの小さい Unit を
//!    拾い直さない）。**二段目の Unit は自分の前提を連れてこない。**
//! 4. **残った Unit の意味情報を Atom へ投影する**。判断単位は Unit、
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
//! (実効 Tier, redundant か, バイト長, 先頭バイト位置, Unit の並び順)
//! ```
//!
//! - **実効 Tier**: `ESSENTIAL < SUPPORTING < CONTEXT < DETAIL`。
//!   ただし redundant な Unit は 1 段だけ弱い Tier として扱う
//!   （[`crate::ReadingTier::weakened`]）。これが「REDUNDANT は元 Tier に
//!   かかわらず優先的に DIM 候補」の実装。Tier を無視して重複を一律
//!   最下位へ落とす案も取れるが、それでは redundancy が Tier を上書きして
//!   しまい「別軸」でなくなるので採らなかった。重複した ESSENTIAL は
//!   重複した SUPPORTING より長く残る。
//! - **redundant か**: 同じ実効 Tier なら、重複していない方を先に残す。
//!   これで「REDUNDANT は同 Tier の非 REDUNDANT より先に DIM になる」。
//! - **バイト長**: 短い方を先に残す。同じ attention でより多くの意味単位が
//!   残り、文書全体の骨格が見えやすくなる。
//! - **先頭バイト位置 / Unit の並び順**: 文書順。最後の同点崩しであり、
//!   これで順序は必ず全順序になる（安定でない並び替えでも結果が揺れない）。
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
//! ## 二段目が予算を使い切ることはある
//!
//! 一段目は Budget を見ないので、**核とその閉包が `budget` を超えることが
//! ある**。実測の記事では一段目だけで文書の 28.3 % を占めた。READ 1 % でも
//! その 28.3 % が出る。
//!
//! これは「30 % と言って 45 % 出る」を避けるという上の話と衝突して見えるが、
//! 衝突していない。あちらは**二段目の取り合いの中で前提を隠れて払う**こと
//! （予算の意味が文書ごとに変わる）を避けている。こちらは
//! **「最低限これを読め」は予算より先にある**という宣言で、量は
//! 一段目の大きさとして数えられる。
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
//! `redundant` は [`crate::SemanticUnit::is_redundant`] であって、
//! `REDUNDANT_WITH` の参照先がその Budget で残っているかは見ていない。
//! u0 を参照して弱められた Unit は、u0 自身が DIM になる Budget でも
//! 弱められたままである。ここでの「既読」は「文書の中で前にある」であって
//! 「読者が実際に辿る経路の上で前にある」ではない。**これは未解決のまま
//! 残っている**（`docs/gotchas/open-questions.md`）。今回入れたのは
//! `PRESUPPOSES` の側だけである。
//!
//! # 表示状態の割り当て
//!
//! ```text
//! 残った Unit で ESSENTIAL かつ非 REDUNDANT
//!     その Unit の核（core_atoms）        -> MARKED
//!     同じ Unit の残り                    -> NORMAL
//! 残ったそれ以外                          -> NORMAL
//! 残らなかったもの                        -> DIM（Unit 全体に一律）
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
//! 核を選ぶのは判定器（Jev）で、この層は [`crate::SemanticUnit::core_atoms`]
//! を読むだけである。**核の選択は Budget に依存しない**ので、上の単調性は
//! そのまま成り立つ。

use std::collections::HashMap;
use std::ops::Range;

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

    // Unit ごとの attention コスト（構成 Atom のバイト長の合計）。
    // 範囲外の添字は validate で弾かれるが、ここでも黙って無視して panic しない。
    let cost = |unit: &crate::unit::SemanticUnit| -> usize {
        unit.atoms
            .iter()
            .filter_map(|&index| doc.atom(index))
            .map(|atom| atom.len())
            .sum()
    };

    let order = keep_order(doc);
    let total: usize = doc.units.iter().map(cost).sum();
    let by_id = index_by_id(doc);

    let mut kept = vec![false; doc.units.len()];
    let mut spent: usize = 0;

    // ---- 一段目 — 核を先に、単独で確保する ----------------------------
    //
    // **ここは Budget を見ない。** 核（= MARKED になりうる Unit）と、その
    // 前提の閉包を、予算の取り合いの前に確保する。取り合いに混ぜると
    // 「READ を下げたら『最低限これを読め』が消える」が起きる（下の
    // 「台帳の単位」）。この集合は Budget の関数ではないので、`spent` の
    // 初期値も Budget に依存しない定数である。
    let mut bill = Vec::new();
    let mut any_core = false;
    for &unit_index in &order {
        if !bears_a_core(&doc.units[unit_index]) {
            continue;
        }
        any_core = true;
        // この Unit と、その前提の閉包。重複は無い（`prerequisites` は
        // 訪問済み集合で辿り、seed 自身を含めない）。
        bill.clear();
        bill.push(unit_index);
        prerequisites(doc, &by_id, unit_index, &mut bill);
        for &index in &bill {
            if !kept[index] {
                kept[index] = true;
                spent += cost(&doc.units[index]);
            }
        }
    }

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
        let due = cost(&doc.units[unit_index]);
        let fits = ((spent + due) as u128) * 100 <= (budget as u128) * (total as u128);
        if fits || (!any_core && rank == 0) {
            kept[unit_index] = true;
            spent += due;
        } else {
            break;
        }
        rank += 1;
    }

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
            let state = if !kept[unit_index] {
                DisplayState::Dim
            } else if marks && unit.is_core(atom) {
                DisplayState::Marked
            } else {
                DisplayState::Normal
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

/// この Unit は**一段目**に入るか — すなわち MARKED になりうるか。
///
/// 表示状態の割り当てと同じ条件である（ESSENTIAL / 非 REDUNDANT / 核を持つ）。
/// **2 箇所で別々に書かない**こと — ずれると「一段目で確保したのに MARKED に
/// ならない Unit」や、その逆が出る。
fn bears_a_core(unit: &crate::unit::SemanticUnit) -> bool {
    unit.reading_tier == ReadingTier::Essential && !unit.is_redundant() && unit.has_core()
}

/// Unit を「残したい順」に並べた添字列を返す。Budget には依存しない。
///
/// 並び替え鍵はモジュールドキュメントのとおり。`decorate` の単調性は
/// 「この順序が Budget に依存しないこと」と「prefix で打ち切ること」の
/// 2 点だけに支えられている。
fn keep_order(doc: &SemanticDocument) -> Vec<usize> {
    let mut order: Vec<usize> = (0..doc.units.len()).collect();
    order.sort_by_cached_key(|&index| {
        let unit = &doc.units[index];
        let redundant = unit.is_redundant();
        let effective_tier = if redundant {
            unit.reading_tier.weakened()
        } else {
            unit.reading_tier
        };
        let length: usize = unit
            .atoms
            .iter()
            .filter_map(|&atom| doc.atom(atom))
            .map(|atom| atom.len())
            .sum();
        let start = unit
            .atoms
            .iter()
            .filter_map(|&atom| doc.atom(atom))
            .map(|atom| atom.range.start)
            .min()
            .unwrap_or(usize::MAX);
        (effective_tier, redundant, length, start, index)
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
        // **REDUNDANT にしてある。** 二段目に落ちるのは核を持たない Unit
        // だけなので（核は一段目が必ず確保する）、「核を持つのに沈む」を
        // 作るには MARKED になれない Unit を使う。`core_atoms` は付いたまま
        // で、**それが DIM に一切効かない**ことがここの主張である。
        let mut dropped = SemanticUnit::new("drop", [AtomIndex(2)], ReadingTier::Essential);
        dropped.set_core([AtomIndex(2)]);
        dropped.relations.push(Relation::RedundantWith("keep".into()));
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

    #[test]
    fn a_redundant_essential_unit_is_never_marked() {
        let doc = doc(&[
            (ReadingTier::Essential, false),
            (ReadingTier::Essential, true),
        ]);
        assert_eq!(
            states(&doc, 100),
            [DisplayState::Marked, DisplayState::Normal]
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

    #[test]
    fn redundancy_weakens_the_tier_by_one_step_without_erasing_it() {
        // 重複した ESSENTIAL は SUPPORTING として競い、重複した SUPPORTING
        // （実効 CONTEXT）より長く残る。
        let doc = doc(&[
            (ReadingTier::Essential, false),
            (ReadingTier::Essential, true),
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

    /// 前提でも重複でもある Unit は、重複として弱められる。2 つの relation は
    /// 独立に効く。
    #[test]
    fn a_redundant_essential_unit_does_not_bring_its_prerequisite() {
        let mut doc = sized(&[
            (ReadingTier::Context, 10),
            (ReadingTier::Essential, 10),
        ]);
        presuppose(&mut doc, 1, 0);
        doc.units[1]
            .relations
            .push(Relation::RedundantWith("u0".into()));
        assert!(doc.units[1].is_redundant());
        // REDUNDANT なので MARKED にならない → **一段目に入らない**。
        assert_eq!(
            states(&doc, 100),
            [DisplayState::Normal, DisplayState::Normal]
        );
        // 55 % = 11 バイト。u1 は入るが、**前提の u0 は連れてこない**。
        // 台帳が一段だった頃はここが `[0, 1]` で、20 バイトを 11 バイトの
        // 予算で買っていた。
        assert_eq!(kept_units(&doc, 55), [1]);
        assert_eq!(kept_units(&doc, 100), [0, 1]);
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
