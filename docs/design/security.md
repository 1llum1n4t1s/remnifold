# Security

## Setup の信頼

Setup は R2 の HTTPS、フィードの SHA-256/SHA-1、コード署名を信頼する（オーナー決定、2026-11-14）。GitHub CLI とパブリッシャーログの証跡は strict 経路だけが使う。検証後に同じユーザーがパッケージを差し替える行為は、導入物も直接変えられるため対象外。

- Windows: 全 PE の Authenticode が有効で、署名者が `Open Source Developer Yuichiro Shinozaki`、メイン実行ファイルとアップデーターが各 1 つ必要。オフライン適用のため失効は見ない。
- macOS: `UpdateMac` と同じ関数で展開し、`codesign --verify --deep --strict`、Developer ID の証明書連鎖、チーム `SL228Y8UUR`、2 つの実行ファイルを確かめ、`spctl` で公証を見る。チケットはステープル済みで、オフライン動作は見込み（未検証）。Gatekeeper が無効なら公証だけを省く。全ユーザー向け導入では管理者パスワードの要求が見込まれる（未検証）。
- Linux: 署名はなく HTTPS とフィードのハッシュに頼る（2026-11-15 決定）。`/var/tmp` は共有なので、nupkg は通常ファイル、nupkg・`packages`・`<packId>` はユーザー所有で他者が書けず、曖昧な名前のない `lib/app/*.AppImage` がちょうど 1 つでなければ `untrusted`（終了コード 2）。グループ書き込みは本人のプライマリグループだけを許す。フィードの差し替えと、確認後の同一ユーザーの差し替えは防がない。独自の分離署名は未実装。

## strict 経路とブートストラップ

署名証明書はオーナーの全アプリで共有され、証明するのはパブリッシャーだけである。製品、版、公開の受理は証明せず、R2 に書ける人は別の署名済み実行ファイルへ置き換えられる。署名でバイト列が変わるため、ブートストラップは CI のアーカイブとも別物である。起動後の `install` がコミット、成功した CI ログ、マニフェスト、アーカイブ、ソースを検証する。GitHub の資格情報は R2 へ送らず、未検証の R2 ヘルパーは自身を認証できない。

`r2-staging` は `release/*` の push だけを、`r2-release` は現在 Setup 用のタグ `v*.*.*` を許す。`validation` は配信シークレットを持たない。選択ブランチのポリシー（`protected_branches: false`、`custom_branch_policies: true`）は PR を配信から遮るが、ブランチ自体は保護しない。信頼する者はオーナーと書き込み権限者で、鍵は必要なバケットの権限だけを持つ。コミットステータスの書き込みはパブリッシャーだけが持つ。

作業フォルダーはリポジトリ、アプリ、設定の外に置き、リンクや重複するパスを外部要求の前に拒む。Unix は root か現在のユーザーの祖先を要し、sticky でない書き込み可能な親を拒み、macOS は deny だけの ACL を許す。

子プロセスの出力は上限付きで読む。Windows は一時停止で開始して kill-on-close Job に入れ、Unix は分離プロセスグループを使い、上限とタイムアウトで全体を止める。グループを離れてデーモン化した子孫は対象外。生ログとシークレットは出力しない。

## macOS と Linux の検証細部

macOS は Velopack の `extract_lib_contents_to_path`（`public-utils`）で展開し、`anchor apple generic` を要件に含め、`spctl --assess --type execute` の `source=Notarized Developer ID` を見る。無効判定は `spctl --status`。公開前の `pack-macos.sh --sign` も同じ要件を調べる。Linux の `get_update_pending_restart` は版だけで nupkg を選ぶため、ローカルの所有権検査が必要。
