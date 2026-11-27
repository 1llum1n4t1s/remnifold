[English](README.md) | [日本語](README.ja.md) | [简体中文](README.zh-CN.md) | [繁體中文](README.zh-TW.md) | [한국어](README.ko.md) | [Español](README.es.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [Português (Brasil)](README.pt-BR.md)

# Remnifold

自行代管的 Cloudflare 記憶伺服器，提供遠端 MCP 存取，並附帶獨立的本機應用程式，讓 Claude Code 與 Codex 能跨專案資料夾回想有用的實作歷程。

應用程式會把已完成的工作存入共用集合，並保留其專案資料夾作為來源紀錄。你可以依內容主題瀏覽紀錄，也可以詢問某個主題，例如「Chrome 擴充功能」：回想功能會在一次伺服器請求中搜尋所有記憶空間，注入簡明的索引，並讓助理在重用實作之前，透過 MCP 開啟適用的來源紀錄。

## 系統需求

遠端 MCP 用戶端可直接連線到已部署的伺服器，無須在本機安裝任何東西。本機應用程式是針對你的作業系統與 CPU 的單一原生執行檔；其 MCP 伺服器、hook、連線設定、歷史匯入與安裝程式都不需要 Node.js、Codex 或 Claude Code 的外掛系統。一般使用者請使用 Setup 安裝程式，它不需要命令列，並且會自行保持更新。供進階使用者使用的嚴格 `remnifold install` 流程，則需要已驗證的原生 GitHub CLI。向 Claude Code 註冊時，若有 Claude Code CLI 就會使用它；否則會印出該執行的完整指令。

伺服器的設定與維護同樣使用這個原生執行檔，並需要你自己的 Cloudflare 帳號，且已啟用 Workers、D1、Vectorize 與 Workers AI。使用平台套件不需要安裝 Node.js、套件管理員、Wrangler 或 Rust。

## 連接遠端 MCP 用戶端

依下文完成伺服器更新與驗證後，請將 `https://your-worker.example.workers.dev/mcp` 作為 Streamable HTTP MCP 端點。相容的用戶端可透過 OAuth 2.1 探索、公開的動態用戶端註冊與 PKCE S256 進行驗證。在伺服器的授權頁面輸入既有的記憶 API 金鑰，並核准該具名用戶端。若完成頁面沒有自動返回用戶端，請依其連結繼續。OAuth 權杖僅適用於 MCP；既有的 REST 用戶端仍使用 API 金鑰。支援設定 Bearer 憑證的用戶端，也可以直接使用記憶 API 金鑰。請妥善保管金鑰與權杖。

[Grok Bot 的 Connect apps](https://docs.x.ai/grok-bot/overview) 與 [Grok connectors](https://docs.x.ai/grok/connectors) 提供應用程式與自訂 MCP 的連線方式。用戶端支援 OAuth 時請選擇 OAuth；Grok Bot 是否能手動輸入 Bearer 金鑰，尚未驗證。

若要用於 ChatGPT，請依照[私人外掛設定](https://developers.openai.com/plugins/build/plugins)：選擇 **+ Add custom MCP server**，輸入 `/mcp` URL 與驗證資訊，然後選擇 **Create as a plugin**。這種私人連線不需要提交到公開目錄。[ChatGPT dots](https://learn.chatgpt.com/docs/dots/computers-and-apps) 使用受支援、已安裝且已啟用的外掛；請在你的帳號中確認該外掛有出現在 dots 中並能正常運作。能否在 dots 中直接輸入任意 MCP URL，尚未驗證。

伺服器會要求 AI 搜尋相關脈絡、在需要時開啟完整紀錄，並儲存具持久價值的成果。工具的使用由用戶端與模型決定，因此不保證會自動搜尋或儲存。需要時請明確要求 AI 搜尋或儲存。這個遠端整合不會取代本機 stdio 伺服器或 hook；請避免在同一個用戶端中重複註冊遠端伺服器。

## 安裝本機應用程式

### Setup 安裝程式（建議）

在 Windows x64 上，下載 [`RemnifoldApp-win-Setup.exe`](https://remnifold-updates.kagayoi.com/velopack/RemnifoldApp-win-Setup.exe)（由發行者簽署，不需要系統管理員權限）並執行。下文所述的 macOS 與 Linux Setup 套件從 1.0.32 之後的第一個版本起，與 Windows 版同時發布。Setup 會以你的使用者身分把應用程式安裝到 `%LOCALAPPDATA%\RemnifoldApp`，並將它註冊到 Claude Code 與 Codex（僅限找得到命令列工具的那些），然後在終端機開啟首次執行引導，說明下一步：連接你的伺服器（`remnifold setup`）。在 Windows、macOS 與 Linux 上，已經透過 Toolport 閘道連接到 Remnifold 的用戶端不會再重複進行直接註冊（見下文「更新、驗證與註冊」）。其檢查不需要 GitHub 帳號。在 macOS（Apple Silicon）上，請開啟 `RemnifoldApp-osx-Setup.pkg`（以發行者的 Developer ID 簽署並經過公證）。它會安裝 `Remnifold.app`（選擇「僅為我安裝」會放在 `~/Applications`；為所有使用者安裝則會放在 `/Applications`，此時套用更新可能需要系統管理員密碼），並啟動應用程式一次，藉此把它註冊到 Claude Code 與 Codex。若用戶端的命令列工具是稍後才安裝，下次啟動 Remnifold 時就會註冊它。執行 `"/Applications/Remnifold.app/Contents/MacOS/remnifold" setup --base-url <URL>`（若安裝在 `~/Applications`，請調整路徑）即可連接你的伺服器。在 Linux（x64 或 ARM64）上，Setup 是單一 AppImage（`RemnifoldApp-linux-x64.AppImage` 或 `RemnifoldApp-linux-arm64.AppImage`）：把它放在你想長期保存的位置（例如 `~/Applications`），設為可執行，然後不帶任何引數執行一次，即可將該 AppImage 註冊到 Claude Code 與 Codex；之後若移動檔案，下次啟動時會註冊新的位置。執行 `<path-to-AppImage> setup --base-url <URL>` 即可連接你的伺服器。AppImage 沒有程式碼簽章，因此在 Linux 上，應用程式信任的是來自更新網站的 HTTPS 與更新來源（feed）中的檢查碼。

更新是自動的。MCP 工作階段執行期間，應用程式每天最多檢查一次更新來源（檢查失敗時也會等一天），並在背景下載較新的版本，不會中斷任何作業。新版本會在你下次登入時套用（Windows：`Run` 項目；macOS：LaunchAgent `~/Library/LaunchAgents/com.kagayoi.RemnifoldApp.update.plist`；Linux：自動啟動項目 `~/.config/autostart/com.kagayoi.RemnifoldApp.update.desktop`），而且只在沒有其他 Remnifold 處理程序執行時才會套用，所以開啟中的工作階段絕不會被關閉。套用之前，應用程式會驗證下載套件中的每個執行檔都由發行者簽署（macOS：發行者的 Developer ID；Linux：沒有簽章，但下載資料夾 `/var/tmp/velopack/RemnifoldApp` 必須屬於你，且不可由其他使用者寫入）。若要關閉自動更新，請在應用程式狀態資料夾（`%APPDATA%\Remnifold`、`~/Library/Application Support/Remnifold`、Linux 上的 `$XDG_STATE_HOME/remnifold`（預設為 `~/.local/state/remnifold`），或 `<REMNIFOLD_HOME>/remnifold`）的 `app-update.json` 中設定 `"auto_update": false`；診斷資訊會寫入同一位置的 `app-update.log`。如果你已經在使用下方的嚴格安裝，Setup 會把 Claude Code 與 Codex 的註冊切換到它自己，並沿用相同的連線與記憶狀態；嚴格安裝的檔案會被保留，因此不再需要時，請自行刪除 `%LOCALAPPDATA%\Programs\Remnifold`。如果 Setup 期間某項註冊未能完成（例如用戶端 CLI 回應緩慢），會在你下次登入時自動重試。請從 Windows 設定中解除安裝；Claude Code 與 Codex 的註冊及登入項目會一併移除，而你的連線與記憶狀態會被保留。之後若要回到嚴格安裝，請用它自己的 `connect claude-code --apply` 與 `connect codex --apply` 重新註冊。若只想移除註冊而不解除安裝，請執行 `remnifold connect claude-code --remove --apply` 與 `remnifold connect codex --remove --apply`。

macOS 沒有解除安裝程式。在把 `Remnifold.app` 移到垃圾桶之前，請先移除它的註冊與登入項目（不加 `--apply` 時，指令只會顯示它將要移除的內容）：

```sh
"/Applications/Remnifold.app/Contents/MacOS/remnifold" uninstall-setup --apply
```

位於 `~/Library/Application Support/Remnifold` 的連線、設定與記憶狀態會被保留；已下載的更新快取位於 `~/Library/Caches/velopack/RemnifoldApp`。

Linux 同樣沒有解除安裝程式。在刪除 AppImage 之前，請以相同方式執行它（路徑請調整為你存放的位置）：

```sh
~/Applications/RemnifoldApp-linux-x64.AppImage uninstall-setup --apply
```

位於 `~/.config/remnifold` 與 `~/.local/state/remnifold` 的連線、設定與記憶狀態會被保留；已下載的更新快取 `/var/tmp/velopack/RemnifoldApp` 可以刪除。

### 嚴格安裝（進階）

嚴格安裝已凍結在最終版本 1.0.32。更新的版本只透過上方的 Setup 安裝程式（Windows、macOS、Linux x64 與 ARM64）發布，因此 `remnifold install` 只能安裝到 1.0.32 為止；已安裝的版本與 `--verify-only` 仍可繼續使用。若要改用更新的版本，請執行 Setup 安裝程式，它會接手 Claude Code 與 Codex 中的註冊。

已發布的原生套件由 [Remnifold updates](https://remnifold-updates.kagayoi.com) 提供。版本索引是 `releases/<version>/index.json`；其資訊清單會為下列每個目標選出一個以雜湊值命名的封存檔。執行下載的執行檔之前，請先透過 GitHub 驗證發布來源；僅憑 R2 URL 或 commit status 連結，無法確立其來源。

| 平台 | Rust 目標 |
| --- | --- |
| Windows x64 | `x86_64-pc-windows-msvc` |
| Mac Apple Silicon | `aarch64-apple-darwin` |
| Linux x64 | `x86_64-unknown-linux-gnu` |
| Linux ARM64 | `aarch64-unknown-linux-gnu` |

不支援 Mac Intel。

應用程式依使用者安裝，不需要系統管理員權限：

| 平台 | 應用程式資料夾 | 固定執行檔 |
| --- | --- | --- |
| Windows | `%LOCALAPPDATA%\Programs\Remnifold` | `bin\remnifold.exe` |
| macOS | `~/Library/Application Support/Remnifold/app` | `bin/remnifold` |
| Linux | `$XDG_DATA_HOME/remnifold`（預設為 `~/.local/share/remnifold`） | `bin/remnifold` |

`remnifold install` 會在寫入任何內容之前，驗證發布的 commit、CI 發布者成功的記錄、R2 資訊清單、封存檔的大小與摘要，以及套件來源。它會把每個版本只解壓縮一次到 `versions/<version>`（已安裝的版本絕不會被不同的位元組覆寫），在 `receipts/` 保留收據以供日後離線驗證，並替換固定執行檔。註冊一律指向固定執行檔，因此更新時不需要重新註冊。在 Windows 上，執行中的 MCP 伺服器會沿用舊的執行檔直到重新啟動；此時結果會回報 `"restartRequired": true`。

### 首次安裝

在 Windows 上，發布工作流程會把已簽章的 bootstrap 執行檔放到 R2 中帶版本號的不可變鍵下。它從 1.0.30 版開始提供；更早的版本請從可信任的原始碼建置第一個執行檔。由於 `install` 只接受嚴格安裝的版本，請將版本設為 1.0.30 到 1.0.32 之間的某個版本。請選擇一個已存在、受信任的私人目錄，使其他使用者無法在驗證與執行之間替換下載的檔案。替換下方引號內的版本與路徑後，從 PowerShell 執行：

```powershell
$version = '<published-version>'
$bootstrap = 'C:\absolute\trusted\private\path\remnifold-bootstrap.exe'
Invoke-WebRequest -MaximumRedirection 0 -Uri "https://remnifold-updates.kagayoi.com/velopack/remnifold-bootstrap-$version-x86_64-pc-windows-msvc.exe" -OutFile $bootstrap
$signature = Get-AuthenticodeSignature -LiteralPath $bootstrap
if ($signature.Status -eq 'Valid' -and $signature.SignerCertificate.Subject -like 'CN=Open Source Developer Yuichiro Shinozaki*' -and $signature.TimeStamperCertificate) { & $bootstrap install --version $version --apply } else { Write-Error 'Signature verification failed. Do not run this file.' }
```

下載不得被重新導向，且檔案在執行之前必須通過 Authenticode 檢查（狀態為 `Valid`，簽署者為 `CN=Open Source Developer Yuichiro Shinozaki`，並帶有 RFC 3161 時間戳記）；檢查失敗就不要執行。此簽章只能證明是誰簽署的，並不綁定產品、版本或發布是否被接受。`install` 仍需要已驗證的原生 GitHub CLI，並自行驗證發布（請參閱 [DESIGN.md](DESIGN.md#release-installation)）。請勿改用未經驗證的 R2 輔助程式，也不要繞過失敗的驗證。

在 macOS 與 Linux 上，請取得並檢閱來源可信的 [Bash bootstrap](scripts/bootstrap-plugin.sh)，並將它儲存在工作目錄之外。它需要原生 GitHub CLI、curl、tar，以及 `sha256sum` 或 `shasum`：

```sh
bash ./bootstrap-plugin.sh --version '<published-version>' --apply
```

不加 `--apply` 時，它只會驗證發布內容；`--download-only` 會回傳已驗證的封存檔而不執行它。

### 更新、驗證與註冊

```sh
remnifold install --version <published-version>                 # 僅驗證發布內容
remnifold install --version <published-version> --apply         # 安裝或更新
remnifold install --version <published-version> --verify-only   # 離線重新檢查已安裝的版本
remnifold connect claude-code                                    # 顯示預定的註冊內容
remnifold connect claude-code --apply                            # 註冊 MCP 伺服器與 hook
remnifold connect claude-code --remove --apply                   # 只移除 Remnifold 的 MCP 項目與 hook
```

請使用應用程式資料夾中的固定執行檔（或將其 `bin` 資料夾加入 `PATH`）。`install` 在 Windows 上使用 `%LOCALAPPDATA%\remnifold` 底下的工作資料夾，在其他系統上使用 `~/.cache/remnifold`；`--work-root <absolute-path>` 可選擇應用程式、設定與儲存庫資料夾之外的其他資料夾。當見證記錄（witness log）過期時，全新安裝會以失敗收場（fail closed）；保留下來的收據仍支援 `--verify-only`。仍然需要能存取 GitHub 上的發布來源中繼資料。

`connect claude-code --apply` 會以 `claude mcp add --scope user remnifold -- <fixed executable> mcp` 註冊 MCP 伺服器（當 `PATH` 中沒有 Claude Code CLI 時則改為印出該指令；可用 `--cli <path>` 指定），並將 `SessionStart`、`UserPromptSubmit`、非同步的 `Stop` 與 `PreToolUse` hook 合併到 Claude Code 的 `settings.json`（`$CLAUDE_CONFIG_DIR` 或 `~/.claude`）；`recallMode` 為 `off` 時不註冊 `SessionStart` 與 `UserPromptSubmit`，並移除 Remnifold 既有的這兩項。只會取代既有的 Remnifold hook 項目；第一次變更時會保留 `settings.json.remnifold-backup`。格式錯誤的設定檔不會被更動。`connect <tool> --remove`（先預覽；`--apply` 才會寫入）只會移除 Remnifold 自己的 MCP 項目與 hook，保留其他所有設定；若加上 `--executable <path>`，則只移除指向該執行檔的項目。`connect codex --apply` 以 `codex mcp add` 註冊 MCP 伺服器；Codex 沒有提供在外掛之外註冊 hook 的受支援方式，因此自動回想與擷取是透過 Claude Code 執行。`connect` 從不檢查 Toolport，一律直接註冊；若 Toolport 已提供 Remnifold，其 JSON 結果會附上 `toolport: {serverId, command, duplicate: true, sameExecutable}` 作為提示。在 Windows、macOS 與 Linux 上，當 Claude Code 或 Codex 的設定中有 Toolport 閘道，且 Toolport 的 registry（`%APPDATA%\Toolport\registry.json`（Windows）、`~/Library/Application Support/Toolport/registry.json`（macOS）、`$XDG_CONFIG_HOME/Toolport/registry.json`（Linux，須為絕對路徑，否則為 `~/.config/Toolport/registry.json`），或 `TOOLPORT_REGISTRY` / `TOOLPORT_DATA_DIR`）在所有使用中的設定檔裡都有一個以 `mcp` 參數執行「`command` 的檔名（不分大小寫，若有 `.exe` 則去掉）為 `remnifold`」的指令的已啟用伺服器時，Setup 會略過直接註冊；無法判定時則直接註冊；在 Windows、macOS、Linux 以外的系統上不做判定，一律直接註冊。 但只有該指令指向與 Setup 所註冊的可執行檔相同的實體時，才算「已提供」（比較前會正規化路徑，僅大小寫不同視為相同）。若指向其他實體，同樣會略過直接註冊並顯示警告，原因記錄為 `toolport-foreign`，並在每次啟動時重新判定。此時請把 Toolport 中 remnifold 的 command 改為 `%LOCALAPPDATA%\RemnifoldApp\current\remnifold.exe`（macOS、Linux 上為 Setup 的可執行檔）。`connect` 的 JSON 中的 `sameExecutable` 在實體相同時為 `true`，不同時為 `false`（兩種情況都帶有 `duplicate: true`）。`remnifold.json` 中的 `mcpRegistration` 可控制這一點：`auto`（預設）、`direct`（一律直接註冊）或 `off`（不自動新增或修復；解除安裝時的移除仍會進行）；無效值視為 `auto`。Setup 從不移除既有的直接註冊；Setup 直接註冊過一次的用戶端，即使你移除了該註冊，也不會被自動重新註冊（只有 Setup 略過的用戶端才可能回到直接註冊）。略過後若用戶端中的 Toolport 閘道消失，或 Toolport 所指向的可執行檔已不存在，則在下次 Remnifold 啟動時（`mcp` 啟動、無引數啟動、登入時的套用）回到直接註冊；在 Toolport 中停用或刪除該伺服器時不會新增任何項目。先前未完成的用戶端之後處理時同樣會判定。兩種情況下都會合併 Claude Code 的 hook，但僅當存在 Claude Code 命令列工具或 `~/.claude` 資料夾時才會建立。若只想移除直接註冊，請執行 `claude mcp remove remnifold --scope user` 或 `codex mcp remove remnifold`（`connect claude-code --remove --apply` 也會移除 hook）。`PreToolUse` 的比對器是 `^mcp__remnifold__|^mcp__toolport__toolport_(call_tool|run_script)$`；經由 `toolport_call_tool` 時，僅當 `name` 是 Toolport registry 中 Remnifold 伺服器的 id 轉換為 Toolport 工具名稱前綴（`A-Za-z0-9_` 以外的字元替換為 `_`）後加上 `__` 與唯讀工具時才放行。`toolport_run_script` 不會被自動放行，因為無法證明腳本只呼叫唯讀工具。腳本以 `sessionId: "current"` 呼叫 `search_memory` 時，掛鉤會在腳本開頭加上一行 JavaScript（`data`、`input` 等維持不變），在執行時把傳給 `toolport.call` / `callAsync`（`servers.<id>.*` 存根與 `callAll` 也經由它們）的 `<前綴>__search_memory` / `searchMemory` 呼叫中的 `"current"` 取代為實際的工作階段 ID。在 auto 模式（未經判定而修改的呼叫會被拒絕）下，以及以 `"use strict"` 開頭的指令碼（前置的一行會使嚴格模式失效）中，不會解析 `run_script` 內的 `"current"`，仍回傳原有的固定錯誤。註冊後請開啟新的工作階段。

`Stop` 擷取除了讀取 Codex rollout，也會讀取 Claude Code 的逐字稿。它只儲存每一則人類的請求與助理的最終文字，略過思考內容、工具呼叫與結果、請求副本，以及其他工作階段的資料列。背景通知之後的回覆，會被存成獨立的延續回合，讓已儲存的回合保有原本的擷取金鑰。Claude Code 的紀錄帶有 `sm_source: "claude-code"`。為避免出現兩套記憶系統，請以 `"autoMemoryEnabled": false` 停用 Claude Code 內建的自動記憶；Remnifold 不會讀取或匯入那些檔案。在本儲存庫內，原始碼的 `.mcp.json`（`cargo run`）優先於使用者層級的註冊；若要改用已安裝的應用程式，請在未納入版本控制的 `.claude/settings.local.json` 中加入 `"disabledMcpjsonServers": ["remnifold"]`。在不改變已儲存回合的前提下，它還會把三類工作階段事件另存為獨立紀錄：AI 產生的壓縮摘要、你在 AI 工作期間送出的訊息，以及你對 AI 提問（AskUserQuestion）的回答。它們只在 `Stop` 時儲存，因此壓縮剛結束時只能搜尋到上一次 `Stop` 為止的紀錄。

先前的 Codex 外掛指令 `plugin-update` 與 `plugin-bootstrap` 已停用；它們會回傳 `USE_REMNIFOLD_INSTALL`。請改為安裝獨立應用程式；之後即可從 Codex 移除舊的外掛。

### 連線到你的伺服器

```sh
remnifold setup --base-url https://your-worker.example.workers.dev
```

若尚未提供 API 金鑰，請在隱藏的提示處輸入。Setup 會先驗證已通過驗證的工作階段，再寫入連線資訊。若要更新既有的連線，請使用 `--force`；不相關的設定會被保留。`REMNIFOLD_API_URL` 與 `REMNIFOLD_API_KEY` 可提供處理程序層級的覆寫。進行非互動式設定時，`--api-key-stdin` 會從標準輸入讀取金鑰。

連線與本機狀態存放在應用程式自己的設定資料夾中，獨立於 Codex 與 Claude Code：

| 平台 | 連線（`remnifold.json`）與伺服器狀態 | 擷取與回想狀態 |
| --- | --- | --- |
| Windows | `%APPDATA%\Remnifold` | 同一資料夾 |
| macOS | `~/Library/Application Support/Remnifold` | 同一資料夾 |
| Linux | `$XDG_CONFIG_HOME/remnifold`（預設為 `~/.config/remnifold`） | `$XDG_STATE_HOME/remnifold`（預設為 `~/.local/state/remnifold`） |

`REMNIFOLD_HOME` 會同時覆寫兩者（連線位於 `<REMNIFOLD_HOME>/remnifold.json`，狀態位於 `<REMNIFOLD_HOME>/remnifold/` 之下，應用程式位於 `<REMNIFOLD_HOME>/app`）；`setup --config-dir <directory>` 會把連線寫到其他位置。較早的版本把這些檔案放在 Codex 家目錄（`CODEX_HOME`，預設為 `~/.codex`）。當新資料夾沒有連線資訊時，首次執行會把有效的舊版連線複製過去，並讀取較早的擷取與回想狀態，新的狀態只會寫入新資料夾。舊檔案絕不會被刪除；`server-setup` 只有在加上 `--apply` 時才會搬移其舊版狀態。

執行 `whoAmI` MCP 工具即可驗證連線與記憶空間探索。

若要匯入既有的 Codex 工作階段，請先以 `remnifold import --limit 10` 預覽一個有上限的批次。若要上傳經過淨化的紀錄，請加上 `--apply`。預覽不會寫入遠端記憶；重複匯入會沿用確定性的擷取識別碼。`--codex-home` 可指定讀取 Codex 工作階段的位置。

## 建立或更新伺服器

套件內含 Worker 組合檔與 migration。請透過處理程序環境變數 `CLOUDFLARE_API_TOKEN` 或 `--token-stdin` 提供 Cloudflare API 權杖，然後預覽既有的資源：

```sh
remnifold server-setup --account-id <Cloudflare-account-id>
```

預設是經過驗證的唯讀探索。權杖需要帳號讀取、Workers 指令碼、D1 與 Vectorize 權限；變更網域或路由還需要對應的 Zone、Routes 或 Custom Domain 權限。Setup 不會匯入 Wrangler 的 OAuth 憑證。請妥善保管權杖與記憶 API 金鑰。

對於既有的安裝，請傳入 `--config <absolute-existing-jsonc-path>` 來讀取其設定而不覆寫。Setup 會依 `remnifold`、`kagayoi-memory`、`cloudflare-supermemory` 的順序沿用資源名稱；明確指定的 `--worker-name`、`--database-name` 與 `--vector-index-name` 優先。請確認計畫選取的是既有的 Worker、D1 資料庫與 Vectorize 索引。已設定的 ID 與向量維度／度量必須相符。既有的綁定、cron 排程與路由會被保留；發生衝突時 Setup 會停止。這次升級請勿建立替代伺服器或複製記憶。

當你打算佈建或部署時，請透過 `REMNIFOLD_API_KEY` 提供所選的記憶金鑰，並套用計畫：

```sh
remnifold server-setup --account-id <Cloudflare-account-id> --apply --location apac
```

請使用與預覽時相同的設定與名稱。`--apply` 會建立缺少的資源、將提供的金鑰安裝為 Worker secret、套用僅新增性質的 D1 migration、部署內嵌的 Worker，並檢查端點。對於既有的伺服器，請提供它現有的金鑰，以免意外將其取代。若沒有提供金鑰，Setup 會保留原有的 secret，並回報已略過驗證；僅憑健康檢查無法確認 migration 與 MCP 存取。提供金鑰後，即可進行工作階段驗證、遠端 MCP 初始化，以及探索 `search_memory`、`getDocument` 與 `add_memory`。

請在連接或更新用戶端之前，先套用伺服器更新與 migration。Setup 會把可續行的設定存放在儲存庫之外，也就是應用程式設定資料夾中的 `server/config.json`（參見[連線到你的伺服器](#連線到你的伺服器)）；`--state <absolute-json-path>` 可選擇另一個外部位置。請保留這份狀態以供日後更新，絕不要提交產生的 ID 或憑證，並在修正失敗的步驟後重新執行相同的指令。對於既有的自訂端點請使用 `--base-url`，查看所有選項請使用 `--help`。Cloudflare 的用量由代管伺服器的帳號計費。

對於 1.0.22，請先以該版本的 `server-setup --apply` 更新伺服器，再更新用戶端。Migration `0013_memory_created_index.sql` 為依日期範圍整理的功能新增索引；它不會更動任何已儲存的紀錄或 ID。

對於 1.0.21，請先以該版本的 `server-setup --apply` 更新伺服器，再更新用戶端。Migration `0010_capture_variants.sql`、`0011_consolidation_recall_opt_out.sql` 與 `0012_organization_retry_fairness.sql` 新增已驗證的擷取變體、使含有已排除回想之來源的歷史檢查點失效，並把日後舊版延後處理的時間縮短為一小時。原始紀錄與 ID 都會保留；受影響的檢查點可從符合資格的來源重新產生。

## 記憶行為

- 已完成的使用者請求與最終回答，會連同來源資料夾的出處一併存入共用的 `memories` 集合。附加的技能定義與瀏覽器／頁面狀態封包，會從儲存的請求文字中省略；最終回答則完整保留。確定性的擷取識別碼讓重試時沿用同一筆已儲存的紀錄，而不會產生重複。你對 AI 提問輸入的回答和回合中途送出的訊息也會被儲存。電子郵件地址和電話號碼會被遮蔽；對詢問個人資料（姓名、地址、電話、電子郵件、信用卡、密碼等）的問題所作的自由輸入回答會被替換。對其他問題的回答或中途訊息（不做聯絡方式遮蔽）中輸入的個人資料仍會被儲存，壓縮摘要也可能重述你說過的內容。壓縮摘要是 AI 寫的線索，不是授權依據，不會被自動回想（`recallable: false`），但手動搜尋可以找到。遮蔽的只有位數結構合理的電話號碼和電子郵件（不含 `git@host`），沒有分隔符號的國內號碼等會保留；對問題用詞的判斷可能因措辭而漏掉，選項式回答會保留。已忘記的事件記錄在重新傳送時（例如收據遺失後）可能重新出現。
- 使用 `listTopics` 找出內容分類，再把 `topic` 傳給 `listDocuments` 或 `listMemories`。清單同時包含舊版資料夾空間與共用集合中的紀錄。使用 `topic: "__unclassified__"` 可查看沒有目前主題標籤的紀錄，包括分類失敗、已停用或被刻意略過的紀錄。
- 手動 `add_memory` 會存入共用集合。若要在讀取某筆紀錄後將其遺忘，請以 `action: "forget"`、它的 `documentId` 與來源 `containerTag` 呼叫 `add_memory`；為了相容性，仍支援以完整儲存內容指定。使用 Codex 外掛時，由 MCP 用戶端提供的絕對路徑 `sourceFolder` 或單一工作區根目錄會加入出處資訊；遠端 MCP 工具則接受明確的 `projectId`。明確指定的 `containerTag` 會覆寫儲存位置。外掛絕不會把它的安裝資料夾當作使用者的專案。`listSpaces` 為了相容性與診斷而公開實體空間。
- 提示回想會依主題跨空間搜尋。簡短的後續提問，可以從同一工作階段較早的使用者訊息中找回主題，甚至能越過大量工具輸出。歷史回復有範圍限制；沒有具體主題時，不會執行主題搜尋。自動回想會選取每個已擷取回合與片段的最新可用修訂版，而手動搜尋則保留歷史修訂版。全域搜尋涵蓋超出舊探索上限的空間；舊版伺服器使用上限為 100 個空間的相容路徑，並在探索不完整時回報。部分搜尋會回報無法使用的空間與文件索引，同時保留成功的結果。起始的工作階段事件只會重設回想狀態。預設情況下（`recallScope` 為 `session`），自動回想只會注入目前工作階段的紀錄：它依主題搜尋，並在套用結果數量上限之前，只保留已儲存的 `sessionId` 與 hook 的 `session_id` 相同的項目；沒有工作階段 ID 的紀錄，或沒有工作階段 ID 的提示，都不會注入。它不會請求其他工作階段的近期任務，因此 `maxRecentTasks` 不會生效。要查看其他工作階段，請使用手動的 `search_memory` 工具；傳入 `sessionId` 可限定在某個工作階段。`"current"` 代表目前的工作階段，僅在 Claude Code 中可用（由其 `PreToolUse` 掛鉤針對直接註冊的 `mcp__remnifold__search_memory`，以及經由 Toolport 閘道 `toolport_call_tool` 或 `toolport_run_script` 呼叫的 `search_memory` 解析；使用其他閘道時請明確指定工作階段 ID；不支援 Codex）。除非 `includeProfile` 為 true，結果不含容器清單。在 `remnifold.json` 中將 `recallScope` 設為 `all` 可恢復原有行為：跨所有工作階段的主題回想，加上最多兩項來自其他工作階段的近期已完成任務，附上來源專案、日期與文件 ID，並另外標示，用來提示過去的工作而不暗示其相關性。在 `all` 模式下，`maxRecentTasks` 接受 0 到 3 的整數；0 會停用近期工作脈絡。沒有此列表模式的舊版伺服器，仍會繼續提供主題回想。`recallScope` 的其他值會視為 `session`。在 `remnifold.json` 中將 `recallMode` 設為 `off` 可完全停止自動回想：無論 `recallScope` 為何，都不會注入索引，也不會送出回想搜尋，但每個回合仍會被擷取，`search_memory` 等手動工具也繼續可用。設為 `off` 時，`connect claude-code` 與 Setup 安裝也不會在 Claude Code 的 `settings.json` 中註冊 Remnifold 的 `SessionStart`、`UserPromptSubmit` 掛鉤，並會移除既有的這兩項；用於儲存的 `Stop` 掛鉤與用於讀取授權的 `PreToolUse` 掛鉤會保留。變更 `recallMode` 後，Setup 安裝會在下一次 `mcp` 啟動、無引數啟動或登入時重新同步這些掛鉤，Claude Code 則從再下一個工作階段起生效；既有安裝也依同樣方式遷移：當 `recallMode` 為 `off` 時，首次更新到此版本或首次啟動 `mcp` 時會移除回想用的掛鉤。透過 strict `install` 方式（凍結於 1.0.32）註冊的仍保留四個掛鉤，`off` 時回想用掛鉤會立即返回；以目前版本的執行檔執行 `connect claude-code --apply` 即可依同樣規則重新註冊。外掛套件本身的掛鉤是固定的，只會直接返回、不做任何事。預設值為 `off`，在將 `recallMode` 設為 `direct` 之前不會自動回想；其他非空值也會視為 `direct`。 記錄除工作階段 ID 外，還會依用戶端類型（取自記錄的 `sm_source`，即 Codex 或 Claude Code）比對，因此不會注入另一個用戶端的相同 ID；能辨識目前用戶端時，未儲存用戶端類型的記錄也不會被注入。 對於 Claude Code，繼續（resume）時複製進對話記錄的舊工作階段 ID（從記錄檔開頭讀取，連同目前 ID 最多 8 個）也視為同一工作階段，因此繼續之前的記錄同樣會被注入，但僅限原始輪次時間不晚於該 ID 最後一筆被複製列的記錄；原工作階段在分叉之後或並行儲存的記錄絕不會被注入，沒有時間的記錄也會被排除。Codex 仍只比對目前的 ID。 伺服器 migration `0015_memory_search_projection_session.sql` 會新增索引，讓此工作階段篩選保持快速；未套用時回傳的記錄相同，只是較慢。
- 手動搜尋也會比對儲存的專案名稱。僅以專案名稱搜尋時，會優先考慮相符的來源專案與較新的來源紀錄；專案與主題併用的搜尋，仍以涵蓋主題為優先。
- 自動脈絡包含標題、說明、章節名稱、時間戳記與文件 ID。簡短而相符的原文段落，可以呈現索引中沒有的細節；它仍是歷史來源文字，重用之前必須先驗證。需要時，Codex 會以 `getDocument` 開啟完整紀錄。
- 注入的 `<remnifold-index>` 區塊、搜尋結果與回想通知中的固定文字，其語言取決於寫入它們的環境：先看 `REMNIFOLD_LANGUAGE`，再看 `LC_ALL`、`LC_MESSAGES` 與 `LANG`，最後看作業系統的顯示語言（Windows 使用者介面語言、macOS `AppleLanguages`）。日文環境使用日文文字，其他一律使用英文。已儲存的標題、說明與摘錄會依儲存的原樣顯示，不會翻譯。
- 主題回想預設最多回傳三筆紀錄。請在 `remnifold.json` 中將 `maxMemories` 設為 1 到 5 的整數，以變更自動回想與手動搜尋的上限；更新連線時，Setup 會保留既有的值。自動脈絡另外限制在 3,000 個 UTF-16 code unit 以內，因此實際容納的紀錄可能更少。
- 自動脈絡會把近期任務與主題索引分開追蹤，並在可取得工作階段與逐字稿資訊時，於後續提示省略未變動的項目。更新過的紀錄可能再次出現，而壓縮或清除脈絡會重設重複抑制。本機回想狀態存放在所選 Codex 家目錄下的 `remnifold/recall-state/`，不會複製對話文字；狀態無法使用時，一般回想仍維持啟用。查詢比對與狀態的細節記載於 [DESIGN.md](https://github.com/1llum1n4t1s/remnifold/blob/main/DESIGN.md#recall)。
- 伺服器每天 03:00 JST 檢查專案。當專案有至少 20 筆尚未整合的紀錄，或有尚未整合的紀錄且距離上次成功的檢查點已過三天（在第一個檢查點之前，則從最舊的待處理紀錄起算）時，它會要求 Workers AI 建立整合檢查點。專案第一次到期執行時，伺服器會以連續、有上限的 AI 批次自動處理完整個既有積壓，因此舊紀錄不需要手動回填指令。原始紀錄會被保留並連結自檢查點。較舊的檢查點會被取代，而變更或遺忘來源會使依賴它的檢查點失效。
- 使用 `consolidate_memory` MCP 工具，可以立即將某個專案檢查點推進一個批次，而不必等待每日檢查。遠端 MCP 需要 `projectId`，即儲存的出處 ID。Codex 外掛可以從恰好一個工作區根目錄推斷目前的專案，也可以明確接受 `sourceFolder` 或 `projectId`。每次呼叫最多處理 40 筆新紀錄；若仍有較大的積壓，請重複呼叫。手動執行仍然要求至少有一筆尚未整合的來源紀錄。
- 自動回想在主題相關度相同時優先選用目前的檢查點，並避免重複已被所選檢查點涵蓋的原始紀錄。檢查點中沒有的具體相符細節，仍會呈現其原始紀錄。手動搜尋則持續呈現原始紀錄。
- 檢查點的內容與中繼資料無法直接編輯：更新會回傳 HTTP 409。請改為編輯原始紀錄並重新產生檢查點。重新充實（re-enrichment）與刪除仍然可用。
- 擷取會遮蔽已設定的記憶 API 金鑰與常見的機密格式，並重試未被確認的紀錄。本機擷取狀態存放在應用程式狀態資料夾的 `capture-state/` 之下；Codex 家目錄中較早版本的收據仍可讀取。

擷取失敗會附帶固定的 `stage`、`cause` 與 `action` 代碼，不會印出例外文字或憑證。`action=repair-required` 的結果（例如 `stage=receipt; cause=invalid`）需要修復對應的本機擷取狀態；光是重試只會持續失敗。`action=retry-or-repair` 保留一般的重試行為，同時允許你調查持續存在的連線或回應問題。若要另外檢查 API 是否可連線，請從解壓縮後的外掛資料夾執行：

```sh
remnifold status
```

主題是在既有的 Workers AI 充實步驟中，從紀錄內容推導而來。分類是非同步的。沒有實質結果的簡短確認，以及類似的低資訊量紀錄，仍可供手動搜尋使用，但會被排除在自動回想與充實之外；明確的主題標籤仍會保留。資料夾名稱會作為來源中繼資料保留。既有的紀錄保有原本的文件 ID 與儲存位置，並出現在同一個主題瀏覽器中。

在安裝使用主題瀏覽或整合功能的用戶端之前，請先更新伺服器並套用其 migration。若要對舊紀錄分類，請使用既有的記憶連線，以平台執行檔預覽一個有上限的批次：

```sh
remnifold topics-setup --limit 10
```

預設批次為 10；`--limit` 接受 1 到 50。確定要進行遠端充實時，加上 `--apply` 即可將該有限批次排入佇列。這會使用既有的 Workers AI 計費，重新整理衍生的 AI／向量／事實充實，並使受影響的專案檢查點失效，同時保留退出設定與修訂保護。結果回報的是已接受的工作，而非已完成的分類。請等待處理完成後再選擇下一批；可檢查 `getDocument` 與 `listTopics` 來驗證結果。

API 金鑰可存取單一伺服器安裝中的紀錄。不同的信任邊界請使用各自獨立的安裝。遮蔽可降低意外擷取機密的機率；請檢視你選擇存入自己後端的內容。

## 記憶整理

安裝應用程式並套用[目前的伺服器 migration](#建立或更新伺服器)（包含 `0009_memory_organization.sql` 中的整理結構，以及 `0013_memory_created_index.sql` 中的範圍整理索引）之後，可隨時使用 [organize-memory 技能](skills/organize-memory/SKILL.md) 整理記憶，例如 `$organize-memory 7`，或說「整理最近 7 天的記憶」。天數（1–3650）依伺服器時間選取最近 N 天內建立的記憶；執行會重複處理批次，直到該範圍沒有到期的工作，或達到選用的批次上限（預設 3，最多 20）。未指定天數時，會從全域佇列取出一個批次。整理不再註冊為 Codex 排程任務；[setup-remnifold](skills/setup-remnifold/SKILL.md) 只會暫停較早版本遺留的任務。

每次執行都使用目前已安裝的執行檔。一個批次最多處理二十筆目標紀錄與 48,000 個 UTF-16 來源單位，包含部分完成或延後的工作。這些並不是已完成紀錄的數量，也不保證總閱讀量或 token 數：候選摘要與針對性的證據讀取是額外的。每個批次的查詢（lookup）最多兩次。未解決的工作會被延後，而不是擴大為大規模探索；因上限而停止的範圍執行，只需再執行一次即可。

若要直接使用 CLI，請執行已安裝的執行檔：

```text
remnifold memory-organize prepare [--days N]
remnifold memory-organize lookup --line
remnifold memory-organize commit --line
remnifold memory-organize sources <summary-document-id> [cursor]
```

這些指令使用既有的擁有者 API 金鑰連線；遠端 MCP 的 OAuth 憑證無法授權整理。它們既不會排程 Codex，也不會部署或 migrate 伺服器。`prepare` 會建立三十分鐘的租約（lease）；`--days N` 會將範圍限制為最近 N 天內建立的記憶，而不會移動全域探索佇列；每個範圍回應都會回傳 `range.cursor`／`range.exhausted`，下一次呼叫則把非 null 的游標以 `--range-cursor <cursor>` 傳入，舊版伺服器會拒絕它而不會退回其他做法。它會回傳來源 ID／修訂版／區塊、延續備註與候選摘要；它不是唯讀的連線檢查。`lookup` 從 stdin 讀取 `{"query":"a concise topic"}`。`commit` 讀取一個包含 `batchId`、`summaries` 與 `coverage` 的 JSON 物件。請遵循執行技能中確切的欄位限制、分割規則與多部分 `groupProgress` 程序。只有當每個區塊與重要主題都已呈現，並保留結論、理由、證據與不確定性時，才宣告 `complete`；無法做到時，請使用部分延續或延後。由 Codex 做出這項判斷。請檢查 commit 收據，並區分已發布的摘要與未完成的進度。

在 `prepare` 之前，請先建立安全的 stdin 路徑，以及保留並分頁檢視其完整回應的方法；在 Windows 上，請先依執行技能進行本機合成 PTY 探測，再取得租約。請從同一份保留下來的回應中，讀取每個來源區塊、延續備忘與候選摘要。顯示被截斷時，應從該次呼叫的回應復原，而不是再準備另一個批次。在 commit 之前，請在記憶體中驗證確切的 JSON：每個 coverage 項目都需要其準備好的 ID／修訂版，以及明確的 `summaries` 陣列，其中部分或延後的項目也要包含 `summaries:[]`。分頁、預檢與中斷後的復原，請遵循[執行技能](skills/organize-memory/SKILL.md#one-batch)。

JSON stdin 有三十秒的期限，且包含跳脫字元在內以 128,000 個 UTF-8 位元組為上限。`--line` 要求一行精簡的 JSON 與其結尾的換行；不加時，請在物件之後關閉 stdin。請使用不回顯的輸入管道，並避免讓產生的文字出現在 shell 引數與指令記錄中。不支援的伺服器會明確停止，不會退回到儲存／遺忘的替代做法。

對於 Windows 的 PTY 輸入，請在專用的 PTY 中直接啟動原生執行檔，並使用 `memory-organize lookup --line --private-stdin` 或 `memory-organize commit --line --private-stdin`。請先等到 stderr 出現 `REMNIFOLD_STDIN_READY`，再正好送出一行精簡的 JSON 與其最後的換行，之後不得再有任何位元組。請讓酬載加上換行不超過 127,000 個 UTF-8 位元組，以便為終端機的轉換保留空間。私密模式會停用回顯與行編輯，並在結束符號之後還原原本的主控台模式。若未出現就緒訊號，或任何步驟失敗，請停止傳送輸入並關閉該 PTY；不要重複使用它，也不要在可能還有輸入殘留時還原回顯。請勿使用共用的主控台或 Git Bash 包裝。重新導向的 stdin 與非 Windows 主機會拒絕 `--private-stdin`；在 macOS／Linux 上，請使用不回顯的管道，以整行或 EOF 輸入。完整的輸入程序請參閱[執行技能](skills/organize-memory/SKILL.md#safe-stdin-and-failures)。

不會刪除任何原始紀錄。已被完整呈現的原始紀錄，只有在其摘要仍然有效期間，才會離開一般搜尋與主題回想；原始紀錄的 ID 與內文仍可直接取得，而近期任務感知保留其既有的資格。衝突的擷取會維持可見並被延後，除非伺服器驗證了不同的單一部分 v2 擷取金鑰與精確的內文雜湊，並將其視為獨立證據。這項驗證無法確立哪個版本最新；尚未解決的矛盾必須保留在摘要中。一般的延後會在一小時後符合資格，而未解決的擷取衝突則持續阻擋自動重試。有上限的佇列會在較舊的到期工作、延續工作與尚未處理的紀錄之間分攤進度；單次執行無法證明整個封存已全部完成。變更或遺忘會使依賴它的摘要失效，並讓仍存在的原始紀錄再次出現。較舊的專案檢查點在被當作完整紀錄處理之前，仍可能呈現資訊。啟用 AI 時，已支援語意搜尋的原始紀錄會等到驗證過的摘要嵌入／向量就緒後才消失。停用 AI 時，詞彙層面的涵蓋即已足夠；啟用 AI 可能使原始紀錄再次出現，直到語意替代項目就緒為止。向量復原是非同步的，十五分鐘一次的 cron 並不是完成期限。摘要嵌入使用既有的 Workers AI 計費；內文由 Codex 產生。

`sources` 是唯讀的，一次最多分頁取得五十個已記錄的來源 ID、空間與日期。請以 getDocument 取得內文；跟隨檢查點本身的來源預覽／游標，可找到其已記錄的成員。不同的出處路徑可能重複出現同一個 ID。目前的原生與遠端 MCP getDocument 回應也包含來源預覽與選用的 `sourceCursor` 分頁；沒有這些欄位的舊版原生外掛，可以改用 CLI。若游標的父項改變，游標會因衝突而停止。[整理設計](DESIGN.md#memory-organization)詳細說明涵蓋範圍、失效與向量復原。

## 從 1.x 名稱升級

請移除已安裝的 `kagayoi-memory`、1.x 的 `cloudflare-supermemory` 或 `remnifold` Codex 外掛，並安裝獨立應用程式。用戶端會依序讀取其設定資料夾，再讀取先前的 Codex 家目錄中的 `remnifold.json`、`kagayoi-memory.json` 與 `supermemory.json`。`REMNIFOLD_API_URL` 與 `REMNIFOLD_API_KEY` 是一般使用的環境變數；先前的 `KAGAYOI_MEMORY_*`、`SUPERMEMORY_API_URL`、`SUPERMEMORY_CODEX_API_KEY` 與 `CLOUDFLARE_MEMORY_API_KEY` 讀取器仍為相容性而保留。請執行 `remnifold setup` 寫入一般的 `remnifold.json`，同時保留先前的檔案。既有的擷取收據與回想狀態，仍可從先前的產品資料夾讀取。伺服器設定會依序沿用名為 `remnifold`、`kagayoi-memory`、`cloudflare-supermemory` 的資源；明確指定的資源名稱一律優先。變更本機應用程式不會部署 Worker，也不會搬移遠端資料庫。既有的文件 ID、擷取 ID、專案出處與持久保存的 `sm_*` 中繼資料都維持不變。儲存相容性的細節記載於 [DESIGN.md](https://github.com/1llum1n4t1s/remnifold/blob/main/DESIGN.md#storage-and-provenance)。

Remnifold 實作了自己的自行代管 API。它獨立於代管的 Supermemory 服務，也不會讀取或修改 Codex 內建的本機記憶檔案。

架構與儲存庫配置，請參閱儲存庫的 [DESIGN.md](https://github.com/1llum1n4t1s/remnifold/blob/main/DESIGN.md)。貢獻者的限制、驗證與封裝指令，記載於 [AGENTS.md](https://github.com/1llum1n4t1s/remnifold/blob/main/AGENTS.md)。

以 [MIT License](LICENSE) 授權。
