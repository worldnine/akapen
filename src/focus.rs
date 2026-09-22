//! フォーカス（`f`）の状態機械 — **短押しはトグル、押しっぱなしは hold**。
//!
//! `docs/design/marks-only-and-review-mode.md` 0 節「フォーカス」。
//!
//! # 同じキーが端末で振る舞いを変える
//!
//! kitty keyboard protocol の使える端末（Ghostty など。`REPORT_EVENT_TYPES`）
//! では [`crossterm::event::KeyEventKind::Release`] が届くので、
//! **押している間だけ沈んで離すと戻る**。届かない端末では離したことが
//! 分からないので、**押した時点で沈んだまま = トグル**になる。
//!
//! ここが 1 つの状態機械で両方を出せるのは、判定を「押下」ではなく
//! **離したときの経過時間**でしているためである:
//!
//! ```text
//! 押す ──→ 沈む（このとき hold かトグルかはまだ決めない）
//!   ├─ Release が [`HOLD_THRESHOLD`] 以上で来た → hold だった → 戻す
//!   ├─ Release が [`HOLD_THRESHOLD`] 未満で来た → 短押し → 沈んだまま
//!   └─ Release が来ない（対応していない端末） → 沈んだまま
//! ```
//!
//! **対応していない端末で壊れない**のはこの形による — Release を待つ分岐が
//! どこにも無い。
//!
//! # auto-repeat よけ（[`REPEAT_GUARD`]）
//!
//! Release の来ない端末で `f` を押しっぱなしにすると、端末の auto-repeat が
//! **押下の連打**として届く（`Press` と区別できない）。そのままだと沈む /
//! 戻るが毎秒何回も入れ替わって点滅する。直前の押下から [`REPEAT_GUARD`]
//! 以内の押下は無視して塞ぐ。
//!
//! kitty protocol の端末では repeat は `KeyEventKind::Repeat` として届き、
//! 呼び出し側（`crate::on_key`）がそれを渡さないので、この門は**効かない
//! ままで正しい**。

use std::time::{Duration, Instant};

/// これ以上押していたら「押しっぱなし」と読む。
///
/// 250 ms は、打鍵として短いほう（人が 1 文字打つ押下は概ね 50〜150 ms）と、
/// 「押さえている」と自覚する長さの間にある。読み手の決定（2026-09-22）。
pub(crate) const HOLD_THRESHOLD: Duration = Duration::from_millis(250);

/// 直前の押下からこれ以内の押下は auto-repeat と見なして捨てる。
pub(crate) const REPEAT_GUARD: Duration = Duration::from_millis(120);

/// フォーカスの状態。
///
/// `Instant` を引数で受けるので、テストは眠らずに時間を進められる。
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Focus {
    /// いま沈んでいるか。
    on: bool,
    /// 沈めた押下のうち、まだ離されていないものの時刻。
    ///
    /// `None` は「離したあと（短押しと決まった）」か「押していない」。
    pressed_at: Option<Instant>,
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
        if let Some(last) = self.last_press
            && now.duration_since(last) < REPEAT_GUARD
        {
            // auto-repeat。押し直しではない。
            return false;
        }
        self.last_press = Some(now);
        if self.on {
            // 沈んでいるところへもう 1 打 = 戻す（トグル）。この押下の
            // Release は何もしない（`pressed_at` が空なので）。
            self.on = false;
            self.pressed_at = None;
        } else {
            self.on = true;
            self.pressed_at = Some(now);
        }
        true
    }

    /// `f` を離した。**状態が変わったら `true`**。
    ///
    /// Release の来ない端末ではこれが呼ばれないだけで、押下の側は同じ。
    pub(crate) fn release(&mut self, now: Instant) -> bool {
        let Some(pressed_at) = self.pressed_at.take() else {
            return false;
        };
        if !self.on {
            return false;
        }
        if now.duration_since(pressed_at) >= HOLD_THRESHOLD {
            self.on = false;
            return true;
        }
        // 短押し。沈んだまま残る。
        false
    }

    /// 明示的に戻す（Esc、文書の切替、問いの入れ替え）。変わったら `true`。
    pub(crate) fn clear(&mut self) -> bool {
        let was = self.on;
        self.on = false;
        self.pressed_at = None;
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
    fn a_hold_sinks_while_pressed_and_comes_back_on_release() {
        let t0 = Instant::now();
        let mut focus = Focus::default();
        assert!(focus.press(t0));
        assert!(focus.is_on(), "押した瞬間から沈む");
        assert!(focus.release(at(t0, 400)), "250 ms 以上なら hold");
        assert!(!focus.is_on());
    }

    #[test]
    fn a_tap_toggles_and_stays() {
        let t0 = Instant::now();
        let mut focus = Focus::default();
        focus.press(t0);
        assert!(!focus.release(at(t0, 80)), "短押しでは何も変わらない");
        assert!(focus.is_on(), "沈んだまま残る");
        // もう 1 打で戻る。
        assert!(focus.press(at(t0, 1000)));
        assert!(!focus.is_on());
        assert!(!focus.release(at(t0, 1080)), "戻したあとの Release は無害");
        assert!(!focus.is_on());
    }

    #[test]
    fn a_terminal_without_release_events_behaves_as_a_toggle() {
        // Release が 1 度も来ない端末。押下だけで沈み、もう 1 打で戻る。
        let t0 = Instant::now();
        let mut focus = Focus::default();
        focus.press(t0);
        assert!(focus.is_on());
        focus.press(at(t0, 5_000));
        assert!(!focus.is_on());
        focus.press(at(t0, 10_000));
        assert!(focus.is_on());
    }

    #[test]
    fn auto_repeat_does_not_flicker() {
        // Release の来ない端末で押しっぱなしにすると、押下が連打で届く。
        let t0 = Instant::now();
        let mut focus = Focus::default();
        focus.press(t0);
        for ms in [30, 60, 90, 119] {
            assert!(!focus.press(at(t0, ms)), "{ms} ms の連打は捨てる");
            assert!(focus.is_on(), "沈んだまま");
        }
        // 門を越えれば普通の押し直し。
        assert!(focus.press(at(t0, 200)));
        assert!(!focus.is_on());
    }

    #[test]
    fn the_threshold_is_the_only_knob() {
        let t0 = Instant::now();
        for (ms, still_on) in [(249u64, true), (250, false), (251, false)] {
            let mut focus = Focus::default();
            focus.press(t0);
            focus.release(at(t0, ms));
            assert_eq!(focus.is_on(), still_on, "{ms} ms");
        }
    }

    #[test]
    fn a_release_that_arrives_over_a_popup_still_ends_the_hold() {
        // 押しっぱなしのまま `m` で popup を開き、そこで離す。
        // **状態機械はモードを知らない** — 押下を記録していれば離した
        // ことに意味があり、記録していなければ素通りする。呼び出し側に
        // モードの門を置くと、この場合に「離したのに沈んだまま」になる。
        let t0 = Instant::now();
        let mut focus = Focus::default();
        focus.press(t0);
        assert!(focus.release(at(t0, 500)), "popup の上でも hold は終わる");
        assert!(!focus.is_on());
    }

    #[test]
    fn a_release_without_a_press_is_ignored() {
        // composer で `f` を文字として打ったときの Release がこれである。
        let t0 = Instant::now();
        let mut focus = Focus::default();
        assert!(!focus.release(t0));
        assert!(!focus.is_on());
    }

    #[test]
    fn clear_reports_whether_anything_was_focused() {
        let t0 = Instant::now();
        let mut focus = Focus::default();
        assert!(!focus.clear(), "沈んでいなければ何もしていない");
        focus.press(t0);
        assert!(focus.clear());
        assert!(!focus.is_on());
        // clear のあとの Release は無害（hold の途中で Esc を押した形）。
        assert!(!focus.release(at(t0, 400)));
        assert!(!focus.is_on());
    }
}
