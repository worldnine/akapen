//! Semantic Reading Layer — 文書を書き換えずに「読む場所」を決める層。
//!
//! > Document stays fixed. Attention moves.
//!
//! この crate は文書を要約も書き換えもしない。文書内の意味的なまとまり
//! （[`SemanticUnit`]）と、それを構成する機械的な位置単位（[`Atom`]）を
//! 受け取り、Reading Budget に応じて
//!
//! ```text
//! どの source range が
//! どの semantic state か
//! ```
//!
//! だけを返す。
//!
//! # 判断単位は Unit、表示単位は Atom
//!
//! Reading Tier や redundancy といった意味情報は [`SemanticUnit`] に付く。
//! しかし表示はもっと細かい [`Atom`] 単位で行う。[`policy::decorate`] は
//! Unit に付いた判断を、その Unit を構成する Atom へ投影して出力する。
//!
//! ```text
//! 意味判断  ->  Semantic Unit
//! 表示      ->  Atom
//! ```
//!
//! Atom は意味単位ではない。**安全に位置を指定できる機械的単位**であり、
//! sentence / list item / heading / code block などがこれにあたる。
//!
//! # 表示の決定はクライアント側
//!
//! この crate が出すのは [`DisplayState`]（MARKED / NORMAL / DIM）までで、
//! 色・背景・underline・dim といった見た目の決定はクライアント
//! （akapen）の責務である。したがってここには ratatui も akapen 本体も
//! 入らない。依存は serde / serde_json と、Atom 生成のための
//! pulldown-cmark（akapen 本体のレンダラと同じ version）だけ。
//!
//! ```text
//! Semantic Engine        Client (akapen)
//! ---------------        ---------------
//! source range      ->   range decoration
//! DisplayState      ->   色 / background / modifier
//! ```
//!
//! # Atom 生成に Jev は要らない
//!
//! [`atomize`] は source を Markdown として解析し、[`Atom`] 列だけを返す。
//! Tier も Unit も付けない。Jev を呼ばずネットワークも使わない、完全に
//! 決定論的な処理なので、API キーを持たないユーザーにもこの層までは
//! 値が届く。意味の境界（どの Atom が同じ Unit か）は Jev の仕事である。
//!
//! ```text
//! source text  -> atomize          (速い / 決定論的 / ローカル)
//!              -> Jev boundary     (遅い / 非決定的 / Provider の内側)
//!              -> Semantic Units
//! ```
//!
//! # Budget 変更では Jev を呼ばない
//!
//! Jev（意味判断を行う System One モデル。**LLM ではない** —
//! `docs/jev.md`）が関わるのは [`Provider`] の内側だけで、
//! 一度 [`SemanticDocument`] が得られたあとの `100% -> 70% -> 30%` という
//! Budget 操作は [`policy::decorate`] だけで完結する純粋ローカル計算である。
//! この crate 自体は Jev を呼ばない（[`FixtureProvider`] のみ）。
//!
//! ```text
//! Jev analysis -> SemanticDocument   (遅い / 非決定的 / Provider の内側)
//!              -> Reading Policy     (速い / 決定論的 / 純粋関数)
//!              -> Atom Decorations
//! ```
//!
//! 外へ判断を委ねるときの話し方は [`protocol`] にある — **Atom を渡して
//! Unit を受け取る**だけで、位置（range）は一度も外へ出ない。だから
//! 外側が壊れた位置を返して文書の違う場所を装飾する事故が起きえない。
//!
//! # 使い方
//!
//! ```
//! use semantic_reading::{DisplayState, FixtureProvider, Provider, policy};
//!
//! let json = r#"{
//!   "atoms": [
//!     { "range": { "start": 0, "end": 7 }, "kind": "heading" },
//!     { "range": { "start": 9, "end": 40 }, "kind": "sentence" }
//!   ],
//!   "units": [
//!     { "id": "u1", "atoms": [0], "reading_tier": "essential", "relations": [] },
//!     { "id": "u2", "atoms": [1], "reading_tier": "detail", "relations": [] }
//!   ]
//! }"#;
//!
//! let provider = FixtureProvider::from_json(json)?;
//! let doc = provider.analyze("# Title\n\n...")?;
//!
//! // Budget 100% なら誰も DIM にならず、ESSENTIAL だけが MARKED。
//! let full = policy::decorate(&doc, 100);
//! assert_eq!(full[0].1, DisplayState::Marked);
//! assert_eq!(full[1].1, DisplayState::Normal);
//!
//! // Budget を絞ると DETAIL から落ちる。
//! let thin = policy::decorate(&doc, 20);
//! assert_eq!(thin[1].1, DisplayState::Dim);
//! # Ok::<(), semantic_reading::Error>(())
//! ```

#![warn(missing_docs)]

mod atom;
mod atomize;
mod display;
mod document;
mod error;
pub mod policy;
pub mod protocol;
mod provider;
mod unit;

pub use atom::{Atom, AtomIndex, AtomKind};
pub use atomize::atomize;
pub use display::DisplayState;
pub use document::SemanticDocument;
pub use error::Error;
pub use protocol::{AnalyzeRequest, AnalyzeResponse};
pub use provider::{FixtureProvider, Provider};
pub use unit::{ReadingTier, Relation, SemanticUnit, UnitId};

/// この crate の [`std::result::Result`]。エラーは [`Error`] に寄せる。
pub type Result<T> = std::result::Result<T, Error>;
