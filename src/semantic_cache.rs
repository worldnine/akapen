//! 解析結果のディスクキャッシュ — 同じ文書は二度解析しない。
//!
//! 作った理由は**費用**である。1 文書 1 回の解析は、業務議事録（22.7 KB）で
//! 約 5 円、`docs/gotchas/semantic-reading.md` で約 8 円かかる
//! （`docs/design/marks-only-and-review-mode.md` の 1 節）。開き直し・再起動・
//! READ の上げ下げでそれを毎回払うのは、判定の質と何の関係も無い出費である。
//! `docs/gotchas/open-questions.md` の 1 番は「実測してから作る」と言っていて、
//! その実測が 2026-09-22 に出た。
//!
//! # 何をキャッシュするか — [`SemanticDocument`] であって、生の応答ではない
//!
//! 外部コマンドが返すのは **Atom の添字**である
//! （[`crate::semantic::CommandProvider`]「コマンドは range を返さない」）。
//! 添字は akapen 自身の [`semantic_reading::atomize`] の出力に対する位置なので、
//! 生の応答を保存すると、**`atomize` が将来変わった日に、古い添字が黙って別の
//! Atom を指す**。[`SemanticDocument`] は byte range と自分の Atom 列を持つので
//! 自己完結していて、この事故が起きない。
//!
//! そしてこの形で保存すると、読み戻しに**新しい照合の仕組みが要らない** —
//! [`semantic_reading::FixtureProvider`] と
//! [`crate::semantic::DigestChecked`] をそのまま通せる（`--semantic` 経路の
//! ために既にあるもの）。読み戻したキャッシュは、fixture と同じように
//! `source_sha256` を名乗り、同じ検査で撥ねられる。
//!
//! # キー
//!
//! ```text
//! <root>/semantic/v1/<sha256(コマンド行)>/<sha256(source)>.json
//! ```
//!
//! - **`source` の sha** — 文書が 1 バイトでも変われば別のキーになる。
//!   無効化は自動で、古い項目は当たらない
//! - **コマンド行の sha** — 判定器を変えたら古いキャッシュが当たってはいけない。
//!   ディレクトリに分けてあるのは、「この判定器の分だけ消す」が `rm -r` 一発に
//!   なるからである
//! - **`v1`** — [`semantic_reading::protocol::VERSION`]。[`SemanticDocument`]
//!   の形が変わった日に、古い項目が黙って当たらないようにする
//!
//! **同じコマンド行のままプロンプトだけ変えると当たる。** これは仕様で、
//! 逃げ道は `--semantic-cache-clear` の 1 つだけである（理由と、他の手を
//! 採らなかった理由は `docs/gotchas/semantic-reading.md`）。
//!
//! # 機密度
//!
//! キャッシュは**ユーザーのホームの下だけ**に置く。リポジトリの中にも、
//! 文書の隣にも置かない。ファイルは 0600、ディレクトリは 0700 で作る。
//!
//! 中身に**文書の本文は入らない** — [`semantic_reading::Atom`] は
//! `{range, kind}` で、[`semantic_reading::SemanticUnit`] も添字と Tier と
//! 関係しか持たない。それでも構造と Tier は業務文書を語る（節の数、どこが
//! 要点か）ので、本文と同じ扱いにしてある。**文書のパスは書かない。**
//!
//! **鍵も書かない。** 注記に残すコマンド行からは環境変数の代入の値を落とす
//! （[`redacted`]）。`--semantic-cmd 'TYPESAFE_API_KEY=… python3 …'` と
//! 書けてしまうので、そのまま残すとアダプタが守っている「鍵は stdout にも
//! stderr にも出さない」をここで破ることになる。

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use semantic_reading::{FixtureProvider, Provider, SemanticDocument, protocol};
use sha2::{Digest, Sha256};

use crate::semantic::{DigestChecked, source_digest};

/// キャッシュ項目のファイルに載る、人間のための注記。
///
/// [`SemanticDocument`] は未知のフィールドを読み飛ばすので、この 1 つを
/// **同じオブジェクトの中に**差し込んでも読み戻しは壊れない。おかげで
/// キャッシュのファイルはそのまま `--semantic <file>` に渡せる（どの項目が
/// 化けているのかを調べるときの唯一の手段になる）。
const METADATA_KEY: &str = "akapen_cache";

/// 解析結果の置き場。
#[derive(Clone, Debug)]
pub(crate) struct SemanticCache {
    root: PathBuf,
}

impl SemanticCache {
    /// ユーザーごとの置き場を、触らずに決める。
    ///
    /// 解決の順序は [`crate::snapshot::SnapshotCache::discover`] と**同じ**で
    /// ある（`AKAPEN_CACHE_DIR` → `XDG_CACHE_HOME` → `$HOME/.cache`）。
    /// macOS で `~/Library/Caches` を使う流儀もあるが、akapen は既に
    /// `~/.cache/akapen/snapshots` を持っていて、置き場が 2 つに割れる方が
    /// 害が大きい。`dirs` 系 crate は、この 1 本のパスのために依存を 1 つ
    /// 増やす価値が無いので使わない。
    ///
    /// `None` は「置き場が決められない」= キャッシュ無しで動く、である。
    pub(crate) fn discover() -> Option<Self> {
        let base = std::env::var_os("AKAPEN_CACHE_DIR")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from))
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .map(|home| home.join(".cache"))
            })?;
        Some(Self::at(root_under(base)))
    }

    pub(crate) fn at(root: PathBuf) -> Self {
        Self { root }
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// 判定器 1 つ分のディレクトリ。コマンド行そのものではなく sha を使うのは、
    /// コマンド行がパスを含みうるからである（ディレクトリ名として安全で、
    /// かつ**文書のパスでない**ことが一目で分かる）。
    fn analyzer_dir(&self, analyzer: &str) -> PathBuf {
        self.root
            .join(format!("v{}", protocol::VERSION))
            .join(digest_of(analyzer))
    }

    /// 1 項目のファイル名。
    ///
    /// marks モードでは **問いの文面の sha** が名前に入る
    /// （`<sha(source)>.q<sha(問い)[..16]>.json`）。3 つのことが同時に立つ:
    ///
    /// - **同じ (文書, 問い) は二度呼ばない** — 巡って戻れば 0 円
    /// - **問いを変えれば別の項目** — 別の答えが同じ鍵に当たらない
    /// - **定型の文面を直せば自動で外れる** — `docs/gotchas/semantic-reading.md`
    ///   の「同じコマンド行のままプロンプトだけ変えると当たる」が、marks
    ///   モードでは `--semantic-cache-clear` を待たずに塞がる
    ///
    /// DIM 版（問い無し）の名前は従来どおりなので、既存の項目は当たり続ける。
    /// 問いを持たない項目のパス。**テストだけが呼ぶ** — 本番経路は
    /// [`Self::get`] / [`Self::put`] を通り、そちらが
    /// [`Self::entry_path_asking`] へ降りる。
    #[cfg(test)]
    fn entry_path(&self, analyzer: &str, source_sha: &str) -> PathBuf {
        self.entry_path_asking(analyzer, source_sha, None)
    }

    /// [`Self::entry_path`] に問いを添えたもの（上の説明のとおり）。
    fn entry_path_asking(
        &self,
        analyzer: &str,
        source_sha: &str,
        question: Option<&str>,
    ) -> PathBuf {
        let name = match question {
            Some(question) => {
                let sha = digest_of(question);
                format!("{source_sha}.q{}.json", &sha[..16])
            }
            None => format!("{source_sha}.json"),
        };
        self.analyzer_dir(analyzer).join(name)
    }

    /// 当たれば [`SemanticDocument`]、外れれば `None`。
    ///
    /// **読めない項目は「外れ」である。** 壊れた JSON、検証に落ちる文書、
    /// 別の文書を名乗る `source_sha256` — どれもエラーにせず `None` を返す。
    /// キャッシュが壊れていることは解析を拒む理由にならない（走らせ直せば
    /// 上書きされる）。
    pub(crate) fn get(&self, analyzer: &str, source: &str) -> Option<SemanticDocument> {
        self.get_asking(analyzer, source, None)
    }

    /// [`Self::get`] に**問い**を添えたもの（marks モード）。
    ///
    /// `None` を渡せば [`Self::get`] と 1 ビットも変わらない — DIM 版の
    /// 項目はこれまでと同じ名前のまま当たる。
    pub(crate) fn get_asking(
        &self,
        analyzer: &str,
        source: &str,
        question: Option<&str>,
    ) -> Option<SemanticDocument> {
        let path = self.entry_path_asking(analyzer, &source_digest(source), question);
        let json = fs::read_to_string(path).ok()?;
        let document: SemanticDocument = serde_json::from_str(&json).ok()?;
        // 問いを聞いたなら、答えも問いを名乗っていなければならない。鍵
        // （文面の sha）と二重になるが、こちらは**判定器が別の問いに
        // 答えた**場合を捕まえる — 鍵は akapen が何を聞いたかしか知らない。
        if question.is_some() && document.question.is_none() {
            return None;
        }
        // 読み戻しの検査は fixture 経路のものをそのまま使う —
        // `FixtureProvider` が `validate`、`DigestChecked` が `source_sha256`。
        let provider = DigestChecked::new(FixtureProvider::from_document(document).ok()?);
        provider.analyze(source).ok()
    }

    /// 解析結果を置く。
    ///
    /// 呼び出し側は失敗を**握りつぶしてよい** — 書けなかったことは、いま手に
    /// ある注釈を捨てる理由にならない（次に開いたときにもう一度払うだけ）。
    pub(crate) fn put(
        &self,
        analyzer: &str,
        source: &str,
        document: &SemanticDocument,
    ) -> Result<()> {
        self.put_asking(analyzer, source, None, document)
    }

    /// [`Self::put`] に**問い**を添えたもの（marks モード）。
    pub(crate) fn put_asking(
        &self,
        analyzer: &str,
        source: &str,
        question: Option<&str>,
        document: &SemanticDocument,
    ) -> Result<()> {
        let dir = self.analyzer_dir(analyzer);
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)
            .with_context(|| format!("create {}", dir.display()))?;
        let target = self.entry_path_asking(analyzer, &source_digest(source), question);
        let json = serde_json::to_string(&with_metadata(document, analyzer)?)?;

        // tmp + rename。半分書けたファイルが「壊れた項目」として残らない
        // （`snapshot.rs::write_blob` と同じ作法）。tmp にも 0600 を付ける —
        // rename まで一瞬でも 0644 のファイルが存在しないように。
        let temp = target.with_extension(format!("tmp-{}", std::process::id()));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temp)
            .with_context(|| format!("create {}", temp.display()))?;
        file.write_all(json.as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, &target).with_context(|| format!("replace {}", target.display()))?;
        Ok(())
    }

    /// 全部消す。戻り値は消した項目数。
    ///
    /// **判定器のプロンプトを変えたときの唯一の逃げ道**である
    /// （`--semantic-cache-clear`）。存在しない置き場は「0 件消した」。
    pub(crate) fn clear(&self) -> Result<usize> {
        let removed = count_entries(&self.root);
        match fs::remove_dir_all(&self.root) {
            Ok(()) => Ok(removed),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(e) => Err(e).with_context(|| format!("remove {}", self.root.display())),
        }
    }
}

/// `.json` の数を数える（`clear` の報告用。壊れた木でも落ちない）。
fn count_entries(dir: &Path) -> usize {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| {
            let path = entry.path();
            if path.is_dir() {
                count_entries(&path)
            } else {
                usize::from(path.extension().is_some_and(|ext| ext == "json"))
            }
        })
        .sum()
}

/// 文書 + 注記。注記は [`SemanticDocument`] が読み飛ばすフィールドに入るので、
/// 読み戻しにも `--semantic` にもそのまま通る。
fn with_metadata(document: &SemanticDocument, analyzer: &str) -> Result<serde_json::Value> {
    let mut value = serde_json::to_value(document)?;
    if let Some(object) = value.as_object_mut() {
        let mut meta = BTreeMap::new();
        meta.insert("cmd".to_string(), serde_json::json!(redacted(analyzer)));
        meta.insert("written_ms".to_string(), serde_json::json!(now_ms()));
        meta.insert(
            "akapen".to_string(),
            serde_json::json!(env!("CARGO_PKG_VERSION")),
        );
        object.insert(METADATA_KEY.to_string(), serde_json::json!(meta));
    }
    Ok(value)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 注記に残すコマンド行から、**環境変数の代入の値を落とす**。
///
/// `--semantic-cmd 'TYPESAFE_API_KEY=sk-… python3 …'` と書けてしまうので、
/// コマンド行をそのまま注記にすると鍵がキャッシュに落ちる。アダプタは
/// 「鍵は stdout にも stderr にも出さない」を守っている
/// （`examples/semantic/jev-annotate.py`）ので、ここが抜け道になってはいけない。
///
/// 判定器の**識別**には影響しない — 引き当てに使うのはディレクトリ名の
/// `sha256(コマンド行そのもの)` の方で、注記は人が読むためだけにある。
///
/// `NAME=value` の形のトークンだけを潰す。`--model=jev-latest` のような
/// フラグは `-` で始まるので当たらない。
fn redacted(cmd: &str) -> String {
    cmd.split_whitespace()
        .map(|token| match token.split_once('=') {
            Some((name, value))
                if !value.is_empty()
                    && !name.is_empty()
                    && name
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_')
                    && !name.starts_with(|c: char| c.is_ascii_digit()) =>
            {
                format!("{name}=…")
            }
            _ => token.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// 決めた base の下のどこに置くか。純粋なので、環境変数を触らずに
/// 「リポジトリの中には行かない」を試せる。
fn root_under(base: PathBuf) -> PathBuf {
    base.join("akapen").join("semantic")
}

fn digest_of(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    const CMD: &str = "python3 examples/semantic/jev-annotate.py";
    const SOURCE: &str = "# 見出し\n\n本文である。\n";

    fn document() -> SemanticDocument {
        let json = format!(
            r#"{{"atoms":[{{"range":{{"start":0,"end":13}},"kind":"heading"}}],
                 "units":[{{"id":"u1","atoms":[0],"reading_tier":"essential"}}],
                 "source_sha256":"{}"}}"#,
            source_digest(SOURCE)
        );
        serde_json::from_str(&json).unwrap()
    }

    fn cache() -> (tempfile::TempDir, SemanticCache) {
        let dir = tempfile::tempdir().unwrap();
        let cache = SemanticCache::at(dir.path().join("semantic"));
        (dir, cache)
    }

    /// 置いて、引ける。これが機能の全部である。
    #[test]
    fn a_stored_document_comes_back() {
        let (_dir, cache) = cache();
        assert!(cache.get(CMD, SOURCE).is_none(), "空のキャッシュは外れる");
        cache.put(CMD, SOURCE, &document()).unwrap();
        let back = cache.get(CMD, SOURCE).expect("置いたものは引ける");
        assert_eq!(back.units.len(), 1);
        assert_eq!(back.source_digest(), Some(source_digest(SOURCE).as_str()));
    }

    /// 文書が 1 バイト変われば別のキー。古い項目は当たらない。
    #[test]
    fn one_byte_of_the_document_is_a_different_key() {
        let (_dir, cache) = cache();
        cache.put(CMD, SOURCE, &document()).unwrap();
        let changed = format!("{SOURCE} ");
        assert!(cache.get(CMD, &changed).is_none());
    }

    /// 判定器が変われば別のキー。**プロンプトではなくコマンド行**で分かれる。
    #[test]
    fn a_different_analyzer_is_a_different_key() {
        let (_dir, cache) = cache();
        cache.put(CMD, SOURCE, &document()).unwrap();
        assert!(cache.get("python3 別のアダプタ.py", SOURCE).is_none());
        assert!(cache.get(CMD, SOURCE).is_some(), "元の方は残っている");
    }

    /// 別の文書を名乗る項目は「外れ」であってエラーではない。
    #[test]
    fn an_entry_naming_another_document_is_a_miss() {
        let (_dir, cache) = cache();
        // 素性だけ他人のものに差し替えて、同じ場所へ手で書く。
        let mut document = document();
        document.source_sha256 = Some(source_digest("まったく別の文書"));
        let dir = cache.analyzer_dir(CMD);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            cache.entry_path(CMD, &source_digest(SOURCE)),
            serde_json::to_string(&document).unwrap(),
        )
        .unwrap();
        assert!(cache.get(CMD, SOURCE).is_none());
    }

    /// 壊れた項目もエラーにしない。走らせ直して上書きできる。
    #[test]
    fn a_broken_entry_is_a_miss_and_can_be_overwritten() {
        let (_dir, cache) = cache();
        let dir = cache.analyzer_dir(CMD);
        fs::create_dir_all(&dir).unwrap();
        let path = cache.entry_path(CMD, &source_digest(SOURCE));
        fs::write(&path, "{ not json").unwrap();
        assert!(cache.get(CMD, SOURCE).is_none());
        cache.put(CMD, SOURCE, &document()).unwrap();
        assert!(cache.get(CMD, SOURCE).is_some());
    }

    /// **機密度。** 中身は業務文書を語るので、ホームの下・0600・0700 で置く。
    #[test]
    fn entries_are_private_to_the_user() {
        let (_dir, cache) = cache();
        cache.put(CMD, SOURCE, &document()).unwrap();
        let file = fs::metadata(cache.entry_path(CMD, &source_digest(SOURCE))).unwrap();
        assert_eq!(file.permissions().mode() & 0o777, 0o600, "ファイルは 0600");
        let dir = fs::metadata(cache.analyzer_dir(CMD)).unwrap();
        assert_eq!(dir.permissions().mode() & 0o777, 0o700, "ディレクトリは 0700");
    }

    /// 文書の本文もパスも書かない。
    #[test]
    fn an_entry_carries_neither_the_text_nor_the_path() {
        let (_dir, cache) = cache();
        cache.put(CMD, SOURCE, &document()).unwrap();
        let json = fs::read_to_string(cache.entry_path(CMD, &source_digest(SOURCE))).unwrap();
        assert!(!json.contains("見出し"), "本文が入っていない: {json}");
        assert!(!json.contains("本文である"), "本文が入っていない: {json}");
        assert!(!json.contains(".md"), "文書のパスが入っていない: {json}");
        // 判定器の素性は残す（どの項目が誰の仕事かを人が読めるように）。
        assert!(json.contains(CMD), "コマンド行は注記として残る: {json}");
    }

    /// 注記を足しても fixture として読める — 化けた項目を `--semantic` で
    /// 直に開いて調べられる、が成り立つ。
    #[test]
    fn an_entry_is_still_a_fixture() {
        let (_dir, cache) = cache();
        cache.put(CMD, SOURCE, &document()).unwrap();
        let path = cache.entry_path(CMD, &source_digest(SOURCE));
        let provider = crate::semantic::load_fixture(&path).expect("--semantic で読める");
        assert_eq!(provider.analyze(SOURCE).unwrap().units.len(), 1);
    }

    /// **鍵はキャッシュに書かない。** 注記のコマンド行からは環境変数の代入の
    /// 値を落とす。`--semantic-cmd` にはインラインで書けてしまうので、
    /// アダプタが守っている「鍵は stdout にも stderr にも出さない」を
    /// ここで破らない。
    #[test]
    fn a_key_passed_inline_never_reaches_the_cache() {
        let (_dir, cache) = cache();
        let cmd = "TYPESAFE_API_KEY=sk-secret-value python3 jev-annotate.py --model=jev-latest";
        cache.put(cmd, SOURCE, &document()).unwrap();
        let json = fs::read_to_string(cache.entry_path(cmd, &source_digest(SOURCE))).unwrap();
        assert!(!json.contains("sk-secret-value"), "鍵が落ちている: {json}");
        assert!(json.contains("TYPESAFE_API_KEY=…"), "伏せた形で残る: {json}");
        // 判定器の識別は落ちない（引き当てはコマンド行そのものの sha）。
        assert!(json.contains("jev-annotate.py"), "{json}");
        assert!(json.contains("--model=jev-latest"), "フラグは潰さない: {json}");
        assert!(cache.get(cmd, SOURCE).is_some(), "引き当ては効いたまま");
        // 伏せた形は別のコマンド行なので、当たらない。
        assert!(cache.get("TYPESAFE_API_KEY=… python3 jev-annotate.py", SOURCE).is_none());
    }

    /// 消す — プロンプトを変えたときの逃げ道。
    #[test]
    fn clearing_removes_every_entry_and_says_how_many() {
        let (_dir, cache) = cache();
        cache.put(CMD, SOURCE, &document()).unwrap();
        cache.put("別のアダプタ", SOURCE, &document()).unwrap();
        assert_eq!(cache.clear().unwrap(), 2);
        assert!(cache.get(CMD, SOURCE).is_none());
        assert_eq!(cache.clear().unwrap(), 0, "無い置き場は 0 件");
    }

    /// 置き場は決めた base の下の `akapen/semantic` で、`snapshot.rs` の
    /// `akapen/snapshots` と同じ屋根に入る。**カレントディレクトリからは
    /// 組み立てない** — 相対パスが 1 つも混ざらないことが、「リポジトリの中や
    /// 文書の隣には置かない」の担保である。
    #[test]
    fn the_cache_root_hangs_off_the_base_it_was_given() {
        let root = root_under(PathBuf::from("/home/someone/.cache"));
        assert_eq!(root, Path::new("/home/someone/.cache/akapen/semantic"));
        assert!(root.is_absolute());
    }
}
