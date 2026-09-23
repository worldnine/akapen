//! **`Esc` は一番手前の層から 1 枚ずつはがす** — その順番の表。
//!
//! view と source と Review の据え付けの一覧の 3 か所の `Esc` が、みな
//! [`peel`] を通る。フッタ右端の予告（`Esc: clear review`）も同じ表を読む
//! （[`preview`]）。**表は 1 本**なので、予告と実際に消えるものはずれない。
//!
//! ```text
//! 確認中の終了の取り消し → 選択 → 削除のフォーカス（source）
//!   → Review の据え付けの一覧 → フォーカス（f）→ Review の下線と !
//!   → marks → 終了（--esc-quit のときだけ）
//! ```
//!
//! 並びの理由は「手前ほど軽い」である。上ほど一時的で、消しても何も
//! 失わない（選択・一覧を閉じる・沈めるのを解く）。下ほど作り直しに手間が
//! 要る — Review と marks は消すと次に出すとき解析をやり直す（キャッシュに
//! 当たれば 0 円だが、当たらなければお金と数秒がかかる）。反射で押した
//! `Esc` がそこまで落ちてくるには、手前の層を全部はがしている必要がある。
//!
//! **Review が marks より手前**なのは、Review が一時的な作業（直す候補を
//! 巡る）で、marks は読むための下敷きだからである。候補を片付けた後も
//! マーカーは残っていてほしい。
//!
//! popup（mark for・ヘルプ・ファイル一覧など）と composer はこの表に
//! 載らない。そちらはキーを丸ごと取る別のハンドラで、`Esc` は閉じるだけ
//! である（`crate::overlay::on_overlay_key`・`crate::on_input_key`）。

use crate::app::{App, Mode};
use crate::overlay::Overlay;

/// `Esc` ではがれる層の 1 枚。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Layer {
    /// 終了の確認（未送信のコメントがあるときの `q`）。`--esc-quit` の
    /// ときは確認そのものに答える（2 度目の `q` と同じ）。
    QuitConfirm,
    /// 行の選択（`v`・一覧の Enter）。
    Selection,
    /// 削除のフォーカス（source の `n` / `N`）。
    DeletionFocus,
    /// Review の据え付けの一覧（`R`）。閉じるだけで、候補は残る。
    ReviewList,
    /// フォーカス（`f`）— 沈めるのを解く。
    Focus,
    /// Review の下線とガターの `!`（候補そのもの）。
    Review,
    /// marks — 問いとマーカーと読み出し。
    Marks,
    /// 終了（`--esc-quit` のときだけ）。
    Quit,
}

/// **はがす順番。** ここを並べ替えれば、`Esc` の挙動もフッタの予告も
/// 同時に変わる。
pub(crate) const ORDER: [Layer; 8] = [
    Layer::QuitConfirm,
    Layer::Selection,
    Layer::DeletionFocus,
    Layer::ReviewList,
    Layer::Focus,
    Layer::Review,
    Layer::Marks,
    Layer::Quit,
];

impl Layer {
    /// いま画面にこの層があるか。
    pub(crate) fn present(self, app: &App) -> bool {
        match self {
            Layer::QuitConfirm => app.confirm_quit,
            Layer::Selection => app.selection.is_some(),
            // 削除のフォーカスは source のものである（view の `Esc` は
            // 以前からここを見ていない）。
            Layer::DeletionFocus => app.mode == Mode::Source && app.deletion_focus().is_some(),
            Layer::ReviewList => app.overlay == Some(Overlay::Review),
            Layer::Focus => app.focused(),
            Layer::Review => app.review_shown(),
            Layer::Marks => app.can_clear_marks_question(),
            Layer::Quit => app.esc_quit_enabled(),
        }
    }

    /// フッタの予告に出す動詞句（`Esc: ` の後ろ）。フラッシュの語と対に
    /// なるように揃えてある（`clear review` → `review cleared`）。
    pub(crate) fn preview(self, app: &App) -> &'static str {
        match self {
            Layer::QuitConfirm if app.esc_quit_enabled() => "quit",
            Layer::QuitConfirm => "cancel quit",
            Layer::Selection => "cancel selection",
            Layer::DeletionFocus => "cancel deletion focus",
            Layer::ReviewList => "close list",
            Layer::Focus => "focus off",
            Layer::Review => "clear review",
            Layer::Marks => "clear marks",
            Layer::Quit => "quit",
        }
    }

    /// この層をはがし、何が消えたかを 1 行でフラッシュする。
    fn peel(self, app: &mut App) {
        match self {
            Layer::QuitConfirm => {
                if app.esc_quit_enabled() {
                    app.running = false;
                } else {
                    app.confirm_quit = false;
                    app.flash("quit cancelled");
                }
            }
            Layer::Selection => {
                app.selection = None;
                app.flash("selection cancelled");
            }
            Layer::DeletionFocus => {
                app.focused_deletion = None;
                app.flash("deletion focus cancelled");
            }
            Layer::ReviewList => {
                app.overlay = None;
                app.flash("list closed");
            }
            Layer::Focus => {
                app.clear_focus();
                app.flash("focus off");
            }
            Layer::Review => {
                app.clear_review();
                app.flash("review cleared");
            }
            Layer::Marks => {
                app.clear_marks_question();
                app.flash("marks cleared");
            }
            // 未送信のコメントがあれば確認を挟む（`q` と同じ道）。
            Layer::Quit => crate::request_quit(app),
        }
    }
}

/// 次の `Esc` がはがす層。何も無ければ `None`（`Esc` は何もしない）。
pub(crate) fn top(app: &App) -> Option<Layer> {
    ORDER.into_iter().find(|layer| layer.present(app))
}

/// **`Esc` の本体。** 一番手前の層を 1 枚だけはがす。
pub(crate) fn peel(app: &mut App) {
    if let Some(layer) = top(app) {
        layer.peel(app);
    }
}

/// フッタ右端の予告 — 次の `Esc` で何が消えるか（`Esc: clear review`）。
///
/// 何も消すものが無く、`Esc` で終了しない設定なら `None`（出さない）。
/// **表が効いている場面でだけ出す**: popup と composer は `Esc` を自分で
/// 取るので、そこで表の予告を出すと嘘になる。
pub(crate) fn preview(app: &App) -> Option<String> {
    let table_owns_esc = match app.overlay {
        None => app.mode != Mode::Input,
        Some(Overlay::Review) => true,
        Some(_) => false,
    };
    if !table_owns_esc {
        return None;
    }
    top(app).map(|layer| format!("Esc: {}", layer.preview(app)))
}
