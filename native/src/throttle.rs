//! 想起（UserPromptSubmit の読み取り）用の、プロセス間の同時 API 数の制限と再試行の補助。
//!
//! 背景: 本番の Worker は D1 を使い、重い検索（/v4/search）が D1 で実質直列に処理される。複数のセッションが
//! 同時にフックを起動すると D1 の待ち行列が溢れ、`503 Memory database is temporarily overloaded`
//! （Retry-After: 10）が返って想起が空になる。サーバーの処理能力は変えられないため、クライアント側で
//! 「同時に API へ出す読み取り数」をプロセスをまたいで絞り、一時エラーは締切内で短く再試行する。
//!
//! 仕組み: 状態フォルダー配下のスロットファイルに対する OS のアドバイザリロック（`File::try_lock`）を
//! 数個のスロットとして使う。プロセスが異常終了してもロックは OS が解放するため、残骸で詰まらない。
//! ロックの準備に失敗した場合（読み取り専用の環境など）は制限なしで進める（フェイルオープン）。
use std::{
    fs::{self, File, OpenOptions},
    path::Path,
    thread,
    time::{Duration, Instant},
};

/// プロセス間で同時に API へ出す想起の読み取り数。D1 は重い検索を直列に処理するため、
/// 増やしても処理量は増えず、待ち行列の溢れ（過負荷）だけを招く。2 は単独セッションの
/// topic と recent の並列（従来どおり）を妨げない最小値。
pub const SLOTS: usize = 2;
/// 1 回の試行に最低限残しておきたい時間。これを切る残り時間では新しい試行を始めない。
pub const MIN_ATTEMPT_MS: u64 = 1200;
/// 一時エラー（429/502/503/504）を含む最大試行回数（最初の1回を含む）。
pub const MAX_ATTEMPTS: u32 = 3;
const POLL: Duration = Duration::from_millis(25);

/// 取得したスロット。破棄（ファイルを閉じる）でロックが解放される。
pub struct Permit {
    _file: File,
}

/// 0..bound の擬似乱数。暗号用途ではなく、待ち時間のばらつき（ジッター）用。
pub fn jitter(bound: u64) -> u64 {
    if bound == 0 {
        return 0;
    }
    let bytes = uuid::Uuid::new_v4().as_u128();
    (bytes % bound as u128) as u64
}

/// `dir` のスロットファイルを `give_up_at` まで待って1つ取得する。取得できなければ `Err(())`。
/// `dir` を準備できない場合は、制限できないので待たずに `Ok(None)`（フェイルオープン）。
pub fn acquire(dir: &Path, slots: usize, give_up_at: Instant) -> Result<Option<Permit>, ()> {
    if fs::create_dir_all(dir).is_err() {
        return Ok(None);
    }
    let mut files: Vec<Option<File>> = (0..slots.max(1))
        .map(|n| {
            OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(dir.join(format!("recall-api-{n}.lock")))
                .ok()
        })
        .collect();
    if files.iter().all(Option::is_none) {
        return Ok(None);
    }
    // 起点をずらし、同時起動したプロセスが同じスロットへ殺到しないようにする。
    let start = jitter(files.len() as u64) as usize;
    loop {
        for offset in 0..files.len() {
            let index = (start + offset) % files.len();
            if files[index].as_ref().is_some_and(|f| f.try_lock().is_ok()) {
                // ロックを取った元のハンドルをそのまま持たせる（複製せず、閉じるまでロックを保つ）。
                return Ok(files[index].take().map(|file| Permit { _file: file }));
            }
        }
        if Instant::now() >= give_up_at {
            return Err(());
        }
        thread::sleep(POLL + Duration::from_millis(jitter(20)));
    }
}

/// 429/502/503/504 は一時的な過負荷・中継の失敗として再試行の対象にする。
pub fn retryable_status(status: Option<u16>) -> bool {
    matches!(status, Some(429 | 502 | 503 | 504))
}

/// 再試行までの待ち時間（ミリ秒）を決める。`remaining_ms` は締切までの残り。
/// 待った後に最低 MIN_ATTEMPT_MS が残らないなら `None`（これ以上は再試行しない）。
///
/// サーバーの Retry-After は尊重する（収まるなら必ずその時間だけ待つ）。ただし D1 過負荷の Retry-After は
/// 固定値の 10 秒で、フックの締切（約 9 秒）に収まらない。収まらない場合は待ち切れないので、
/// 指数バックオフ（600ms, 1200ms … + 最大 300ms のジッター）の短い間隔で試す。試行回数は MAX_ATTEMPTS までなので、
/// 過負荷のサーバーへ連打することはない。
pub fn retry_delay(attempt: u32, retry_after_ms: Option<u64>, remaining_ms: u64) -> Option<u64> {
    let budget = remaining_ms.checked_sub(MIN_ATTEMPT_MS)?;
    let delay = match retry_after_ms {
        Some(ms) if ms <= budget => ms + jitter(100),
        _ => 600u64 * (1 << attempt.min(4)) + jitter(300),
    };
    (delay <= budget).then_some(delay)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("remnifold-throttle-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn slots_are_exclusive_and_released_on_drop() {
        let dir = dir();
        let soon = || Instant::now() + Duration::from_millis(80);
        let a = acquire(&dir, 2, soon()).unwrap().unwrap();
        let b = acquire(&dir, 2, soon()).unwrap().unwrap();
        // 2 スロットが埋まると、期限まで待っても取得できない。
        assert!(acquire(&dir, 2, soon()).is_err());
        drop(a);
        // 解放されたスロットはすぐ取得できる。
        let c = acquire(&dir, 2, soon()).unwrap().unwrap();
        drop((b, c));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn unusable_directory_fails_open() {
        // ディレクトリを作れない場所（ファイルの配下）では待たずに制限なしで進む。
        let file =
            std::env::temp_dir().join(format!("remnifold-throttle-file-{}", uuid::Uuid::new_v4()));
        fs::write(&file, b"x").unwrap();
        assert!(matches!(
            acquire(&file.join("sub"), 2, Instant::now()),
            Ok(None)
        ));
        let _ = fs::remove_file(&file);
    }

    #[test]
    fn retry_delay_honors_retry_after_only_when_it_fits() {
        // Retry-After が締切に収まるなら、その時間（+小さなジッター）を待つ。
        let d = retry_delay(0, Some(2000), 8000).unwrap();
        assert!((2000..2100).contains(&d));
        // 収まらない固定 10 秒は待てないため、短いバックオフに切り替える。
        let d = retry_delay(0, Some(10_000), 8000).unwrap();
        assert!((600..900).contains(&d));
        // 試行ごとにバックオフが伸びる。
        let d = retry_delay(1, None, 8000).unwrap();
        assert!((1200..1500).contains(&d));
        // 待った後に最低限の試行時間が残らないなら再試行しない。
        assert!(retry_delay(0, None, 1500).is_none());
        assert!(retry_delay(0, None, 100).is_none());
    }

    #[test]
    fn only_transient_statuses_retry() {
        for status in [429, 502, 503, 504] {
            assert!(retryable_status(Some(status)));
        }
        for status in [None, Some(400), Some(401), Some(404), Some(500)] {
            assert!(!retryable_status(status));
        }
    }
}
