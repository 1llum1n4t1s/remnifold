# Workflow

## Setup の更新の取得と適用

Remnifold はセッションごとの `mcp` とプロンプトごとの `hook` を持つ。Windows の `Update.exe apply` は導入フォルダーの他プロセスを強制終了するため、取得と適用を分ける。プロセスを止めない macOS/Linux にも、版の混在を避けるため同じ規則を使う。

- `main()` は全サブコマンドで引数解析より先に、自動適用を切った `VelopackApp` を実行する。フックは常に終了コード 0 で、導入・更新・削除を妨げない。
- `mcp` のスレッドは確認とダウンロードだけを行う。成功も失敗も 24 時間抑制し（`last_checked_at`、`last_failed_at`）、排他 try-lock で二重取得を防ぐ。`app-update.json` の `auto_update`（既定 on）で止められる。stdout には書かず `app-update.log` へ出す。フックはネットワークに触れず、Velopack 以外のビルドは何もしない。
- ログオン時の `update-apply` は、保留がなければ即終了し、他プロセスが動いていれば次回に回し、署名者を検証してから再起動なしで適用する。確認と適用の間に始まったプロセスだけはミリ秒の時間差で止められうる。
- Windows の更新フックは 15 秒、他のフックは 30 秒で Velopack に終了される。フック内の CLI 呼び出しはプロセスツリーごと 3 秒に制限する。更新フックは合計 12 秒の予算を持ち、経過時間に CLI 3 回分を足して収まる場合だけ次のクライアントを始める。`remove` と `add` の間では止めない。未完了は記録から外し、次のログオン、引数なし起動、`mcp` 開始で 30 秒上限の再試行を行う。MCP エントリがないと `mcp` は起動しないので、ログオン時が確実な再試行となる。

## Setup の公開

フィード `https://remnifold-updates.kagayoi.com/velopack/` は strict の索引とは別で、`releases.<channel>.json`（`win`、`osx`、`linux-x64`、`linux-arm64`）、`*-full.nupkg`、Setup、署名済みブートストラップを持つ。版付きの物は create-only、固定名の Setup とマニフェストは置き換え、マニフェストが最後。ソースは `update_source` だけが構築し、`update-feed-test` は本番ビルドにない。

`release-velopack.yml` は `main` のコミット上の軽量タグ `v<x.y.z>` の push で始まる。GitHub Release は作らず、タグを ref に指定した手動実行で再開できる。Environment もシークレットもない `gate` が、タグの形、プラグイン版との一致、実行コミット、既定ブランチの祖先であること（compare API）を確かめ、同じコミットの既定ブランチの push による `ci.yml` の成功を待ち、タグを再確認する。タグの push による 4 ターゲットの CI は待たない事後検査である。

続いて `r2-release` の独立した 4 ジョブが、署名した `win`、Developer ID 署名と公証の `osx`、署名なしの Linux 2 種（glibc 2.39 以下）を公開する。各ジョブは `ci-release-tag` と公開フィードを確認し、掲載済みのチャンネルは飛ばし、より新しい版があれば失敗する。署名と R2 のシークレットは該当ステップだけに渡し、実行可能な Action は使わない。
