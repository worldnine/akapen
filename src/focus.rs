//! フォーカス（`f`）の状態機械 — **トグル 1 つ**。
//!
//! `docs/design/marks-only-and-review-mode.md` 0 節「フォーカス」。
//!
//! 押すたびに沈む / 戻るが入れ替わる。端末で振る舞いが変わるところは
//! 1 つも無い。
//!
//! # 「押している間だけ沈む」は 2026-09-22 に試して捨てた
//!
//! 最初の実装は「短押しはトグル、押しっぱなしは hold」だった。離した
//! ことを知るために kitty keyboard protocol（`REPORT_EVENT_TYPES`）を
//! 押していたが、**その旗は端末の auto-repeat を
//! [`crossterm::event::KeyEventKind::Repeat`] に変える** — `Press` だけを
//! 渡していた既存のキー経路が崩れて、矢印の押しっぱなしでスクロールが
//! 続かなくなった。旗ごと外し、`f` はトグルだけにしてある
//! （`docs/gotchas/terminal-keys.md`）。
//!
//! # auto-repeat よけ（[`REPEAT_GUARD`]）
//!
//! `f` を押しっぱなしにすると、端末の auto-repeat が**押下の連打**として
//! 届く（`Press` と区別できない）。そのままだと沈む / 戻るが毎秒何回も
//! 入れ替わって点滅する。**直前に見た押下**から [`REPEAT_GUARD`] 以内の
//! 押下は無視して塞ぐ（捨てた押下も時刻を進める。通したものだけを憶えると
//! 門の幅ごとに 1 発通って、遅い点滅になる）。
//!
//! **押し始めの 1 回は通る。** OS の「キーリピート開始までの待ち時間」は
//! macOS の既定で 375 ms あり、門の 120 ms より長い。押しっぱなしにすると
//! 沈んで、最初の repeat が来たところで 1 度だけ戻り、そのあとは動かない。
//! 「押している間ずっと沈んだまま」にするには門を初動より長くする
//! （400〜500 ms）ことになるが、それは素早い 2 度打ちを捨てる取引なので、
//! 数字は読み手が決める（2026-09-22 は 120 ms のまま）。

use std::time::{Duration, Instant};

/// 直前の押下からこれ以内の押下は auto-repeat と見なして捨てる。
pub(crate) const REPEAT_GUARD: Duration = Duration::from_millis(120);

/// フォーカスの状態。
///
/// `Instant` を引数で受けるので、テストは眠らずに時間を進められる。
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Focus {
    /// いま沈んでいるか。
    on: bool,
    /// 直前の押下（[`REPEAT_GUARD`] 用）。
    last_press: Option<Instant>,
}

impl Focus {
    /// いま沈んでいるか。
    pub(crate) fn is_on(&self) -> bool {
        self.on
    }

    /// `f` の押下。**状態が変わったら `true`**（呼び出し側は装飾を作り直す）。
    pub(crate) fn press(&mut self, now: Instant) -> bool {
        // **捨てた押下も時刻を進める。** 通したものだけを憶えると、門の
        // 幅を跨いだところで連打が 1 発通り、押している間じゅう
        // `REPEAT_GUARD` ごとに入れ替わる（30 ms 刻みなら 8 Hz の点滅）。
        // 測るのは「直前に**見た**押下」からである。
        let repeat = self
            .last_press
            .is_some_and(|last| now.duration_since(last) < REPEAT_GUARD);
        self.last_press = Some(now);
        if repeat {
            // auto-repeat。押し直しではない。
            return false;
        }
        self.on = !self.on;
        true
    }

    /// 明示的に戻す（Esc、文書の切替、問いの入れ替え）。変わったら `true`。
    pub(crate) fn clear(&mut self) -> bool {
        let was = self.on;
        self.on = false;
        was
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    #[test]
    fn a_press_toggles_and_stays() {
        let t0 = Instant::now();
        let mut focus = Focus::default();
        assert!(focus.press(t0));
        assert!(focus.is_on(), "押した瞬間から沈む");
        // 離しても何も起きない（Release は見ていない）。
        assert!(focus.is_on(), "沈んだまま残る");
        assert!(focus.press(at(t0, 1000)));
        assert!(!focus.is_on(), "もう 1 打で戻る");
        assert!(focus.press(at(t0, 2000)));
        assert!(focus.is_on());
    }

    #[test]
    fn auto_repeat_does_not_flicker() {
        // 押しっぱなしにすると、押下が連打で届く。
        let t0 = Instant::now();
        let mut focus = Focus::default();
        focus.press(t0);
        for ms in [30, 60, 90, 119] {
            assert!(!focus.press(at(t0, ms)), "{ms} ms の連打は捨てる");
            assert!(focus.is_on(), "沈んだまま");
        }
        // 門を越えれば普通の押し直し。**最後に見た押下（119 ms）から**
        // 測るので、250 ms は 131 ms あいている。
        assert!(focus.press(at(t0, 250)));
        assert!(!focus.is_on());
    }

    /// **押しっぱなしの間じゅう、1 度も入れ替わらない。**
    ///
    /// 門を「通した押下」からではなく「見た押下」から測らないと、
    /// 連打が門の幅を跨いだところで 1 発通り、8 Hz で点滅する
    /// （30 ms 刻みなら 120 ms ごと）。押しっぱなしは何秒も続くので、
    /// 4 発だけ流すテストではここを踏めない。
    #[test]
    fn a_sustained_auto_repeat_never_toggles() {
        let t0 = Instant::now();
        let mut focus = Focus::default();
        focus.press(t0);
        for ms in (30..=1200).step_by(30) {
            assert!(!focus.press(at(t0, ms)), "{ms} ms の連打を通した");
            assert!(focus.is_on(), "{ms} ms で戻った");
        }
    }

    #[test]
    fn the_guard_is_the_only_knob() {
        // 門の境目。120 ms ちょうどは通す（`<` で捨てている）。
        let t0 = Instant::now();
        for (ms, still_on) in [(119u64, true), (120, false), (121, false)] {
            let mut focus = Focus::default();
            focus.press(t0);
            focus.press(at(t0, ms));
            assert_eq!(focus.is_on(), still_on, "{ms} ms");
        }
    }

    #[test]
    fn clear_reports_whether_anything_was_focused() {
        let t0 = Instant::now();
        let mut focus = Focus::default();
        assert!(!focus.clear(), "沈んでいなければ何もしていない");
        focus.press(t0);
        assert!(focus.clear());
        assert!(!focus.is_on());
    }
}
