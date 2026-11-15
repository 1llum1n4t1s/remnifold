[English](README.md) | [日本語](README.ja.md) | [简体中文](README.zh-CN.md) | [繁體中文](README.zh-TW.md) | [한국어](README.ko.md) | [Español](README.es.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [Português (Brasil)](README.pt-BR.md)

# Remnifold

원격 MCP 접근을 지원하는 셀프 호스팅 Cloudflare 메모리 서버와, Claude Code 및 Codex에서 여러 프로젝트 폴더에 걸쳐 유용한 구현 이력을 다시 불러오는 독립 실행형 로컬 앱입니다.

앱은 완료된 작업을 공유 컬렉션에 저장하고 프로젝트 폴더를 출처 정보로 유지합니다. 내용 기반 주제별로 기록을 둘러볼 수 있으며, "Chrome extension" 같은 주제를 물어보면 recall이 한 번의 서버 요청으로 여러 메모리 공간을 검색하고 간결한 인덱스를 주입합니다. 어시스턴트는 구현을 재사용하기 전에 MCP를 통해 해당하는 원본 기록을 열어 볼 수 있습니다.

## 요구 사항

원격 MCP 클라이언트는 로컬에 아무것도 설치하지 않고 배포된 서버에 연결합니다. 로컬 앱은 운영 체제와 CPU에 맞는 단일 네이티브 실행 파일이며, MCP 서버, 훅, 연결 설정, 이력 가져오기, 설치 프로그램에 Node.js, Codex, Claude Code 플러그인 시스템이 필요하지 않습니다. 일반 사용자는 명령줄이 필요 없고 스스로 최신 상태를 유지하는 Setup 설치 프로그램으로 설치합니다. 고급 사용자를 위한 엄격한 `remnifold install` 경로에는 인증된 네이티브 GitHub CLI가 필요합니다. Claude Code에 등록할 때는 Claude Code CLI가 있으면 이를 사용하고, 없으면 실행해야 할 명령을 그대로 출력합니다.

서버 설정과 유지 관리도 같은 네이티브 실행 파일과 사용자 본인의 Cloudflare 계정을 사용하며, Workers, D1, Vectorize, Workers AI가 활성화되어 있어야 합니다. 플랫폼 패키지를 사용하는 데 Node.js, 패키지 관리자, Wrangler, Rust 설치는 필요하지 않습니다.

## 원격 MCP 클라이언트 연결

아래 절차로 서버를 업데이트하고 검증한 뒤, `https://your-worker.example.workers.dev/mcp`를 Streamable HTTP MCP 엔드포인트로 사용하세요. 호환되는 클라이언트는 OAuth 2.1 디스커버리, 공개 동적 클라이언트 등록, PKCE S256으로 인증할 수 있습니다. 서버의 인증 페이지에서 기존 메모리 API 키를 입력하고 지정된 클라이언트를 승인합니다. 완료 페이지가 클라이언트로 자동으로 돌아가지 않으면 페이지의 링크를 따라 계속 진행하세요. OAuth 토큰은 MCP에서만 유효하며, 기존 REST 클라이언트는 계속 API 키를 사용합니다. 설정된 Bearer 자격 증명을 지원하는 클라이언트는 메모리 API 키를 직접 사용할 수도 있습니다. 키와 토큰은 외부에 노출되지 않도록 관리하세요.

[Grok Bot의 Connect apps](https://docs.x.ai/grok-bot/overview)와 [Grok connectors](https://docs.x.ai/grok/connectors)는 앱 연결 경로와 사용자 지정 MCP 연결 경로를 제공합니다. 클라이언트가 지원하면 OAuth를 선택하세요. Grok Bot에서 Bearer 키를 수동으로 입력할 수 있는지는 확인되지 않았습니다.

ChatGPT에서는 [비공개 플러그인 설정](https://developers.openai.com/plugins/build/plugins)을 따르세요. **+ Add custom MCP server**를 선택하고 `/mcp` URL과 인증 방식을 입력한 뒤 **Create as a plugin**을 선택합니다. 이 비공개 연결은 공개 디렉터리에 제출할 필요가 없습니다. [ChatGPT dots](https://learn.chatgpt.com/docs/dots/computers-and-apps)는 지원되며 설치 및 활성화된 플러그인을 사용하므로, 사용자 계정에서 이 플러그인이 dots에 표시되고 동작하는지 확인하세요. dots에서 임의의 MCP URL을 직접 입력할 수 있는지는 확인되지 않았습니다.

서버는 AI에게 관련 맥락을 검색하고, 필요하면 전체 기록을 열고, 오래 남을 결과를 저장하도록 요청합니다. 도구 사용 여부는 클라이언트와 모델이 결정하므로 자동 검색이나 저장은 보장되지 않습니다. 필요할 때는 AI에게 검색이나 저장을 명시적으로 요청하세요. 이 원격 연동은 로컬 stdio 서버나 훅을 대체하지 않으므로, 같은 클라이언트에 원격 서버를 중복 등록하지 마세요.

## 로컬 앱 설치

### Setup 설치 프로그램 (권장)

Windows x64에서는 [`RemnifoldApp-win-Setup.exe`](https://remnifold-updates.kagayoi.com/velopack/RemnifoldApp-win-Setup.exe)(게시자 서명 포함, 관리자 권한 불필요)를 내려받아 실행하세요. 아래에 설명하는 macOS와 Linux용 Setup 패키지는 아직 게시되지 않았으므로, 해당 플랫폼에서는 당분간 아래의 엄격한 설치를 사용하세요. Setup은 앱을 사용자 단위로 `%LOCALAPPDATA%\RemnifoldApp`에 설치하고, Claude Code와 Codex(명령줄 도구가 발견된 쪽만)에 등록한 뒤, 다음 단계인 서버 연결(`remnifold setup`)을 안내하는 첫 실행 가이드를 터미널에서 엽니다. 이 점검에는 GitHub 계정이 필요하지 않습니다. macOS(Apple Silicon)에서는 `RemnifoldApp-osx-Setup.pkg`(게시자의 Developer ID로 서명 및 공증됨)를 여세요. `Remnifold.app`이 설치되며("나만을 위해 설치"를 선택하면 `~/Applications`에, 모든 사용자용 설치는 `/Applications`에 들어갑니다. 후자에서는 업데이트를 적용할 때 관리자 암호를 묻는 경우가 있습니다), 앱을 한 번 실행하여 Claude Code와 Codex에 등록합니다. 클라이언트의 명령줄 도구를 나중에 설치한 경우에는 다음 Remnifold 시작 시 등록됩니다. 서버에 연결하려면 `"/Applications/Remnifold.app/Contents/MacOS/remnifold" setup --base-url <URL>`을 실행하세요(`~/Applications`에 설치했다면 경로를 조정하세요). Linux(x64 또는 ARM64)에서 Setup은 AppImage 하나(`RemnifoldApp-linux-x64.AppImage` 또는 `RemnifoldApp-linux-arm64.AppImage`)입니다. 보관할 위치(예: `~/Applications`)에 두고 실행 권한을 부여한 뒤 인수 없이 한 번 실행하면 해당 AppImage가 Claude Code와 Codex에 등록되며, 나중에 파일을 옮기면 다음 시작 시 새 위치가 등록됩니다. 서버에 연결하려면 `<path-to-AppImage> setup --base-url <URL>`을 실행하세요. AppImage에는 코드 서명이 없으므로, Linux에서 앱은 업데이트 사이트의 HTTPS와 업데이트 피드의 체크섬을 신뢰합니다.

업데이트는 자동입니다. MCP 세션이 실행되는 동안 앱은 하루에 최대 한 번 업데이트 피드를 확인하고(확인에 실패해도 하루를 기다립니다), 새 버전을 백그라운드에서 아무것도 방해하지 않고 내려받습니다. 새 버전은 다음 로그온 시에 적용되며(Windows: `Run` 항목, macOS: LaunchAgent `~/Library/LaunchAgents/com.kagayoi.RemnifoldApp.update.plist`, Linux: 자동 시작 항목 `~/.config/autostart/com.kagayoi.RemnifoldApp.update.desktop`), 다른 Remnifold 프로세스가 실행 중이 아닐 때만 적용되므로 열려 있는 세션이 닫히는 일은 없습니다. 적용하기 전에 앱은 내려받은 패키지의 모든 실행 파일이 게시자의 서명을 받았는지 검증합니다(macOS: 게시자의 Developer ID, Linux: 서명은 없지만 다운로드 폴더 `/var/tmp/velopack/RemnifoldApp`이 본인 소유이고 다른 사용자가 쓸 수 없어야 함). 이 기능을 끄려면 앱의 상태 폴더(`%APPDATA%\Remnifold`, `~/Library/Application Support/Remnifold`, Linux에서는 `$XDG_STATE_HOME/remnifold`(기본값 `~/.local/state/remnifold`), 또는 `<REMNIFOLD_HOME>/remnifold`)에 있는 `app-update.json`에 `"auto_update": false`를 설정하세요. 진단 정보는 같은 폴더의 `app-update.log`에 기록됩니다. 아래의 엄격한 설치를 이미 사용 중이라면, Setup은 Claude Code와 Codex 등록을 자신으로 전환하고 같은 연결과 메모리 상태를 사용합니다. 엄격한 설치의 파일은 그대로 남으므로 더 이상 필요 없으면 `%LOCALAPPDATA%\Programs\Remnifold`를 직접 삭제하세요. Setup 중에 등록을 끝내지 못한 경우(예: 클라이언트 CLI가 느린 경우)에는 다음 로그온 시 자동으로 다시 시도합니다. 제거는 Windows 설정에서 하며, Claude Code와 Codex 등록 및 로그온 항목이 함께 제거되고 연결과 메모리 상태는 유지됩니다. 이후 엄격한 설치로 되돌아가려면 해당 설치의 `connect claude-code --apply`와 `connect codex --apply`로 다시 등록하세요. 제거하지 않고 등록만 해제하려면 `remnifold connect claude-code --remove --apply`와 `remnifold connect codex --remove --apply`를 실행하세요.

macOS에는 제거 프로그램이 없습니다. `Remnifold.app`을 휴지통으로 옮기기 전에 등록과 로그온 항목을 제거하세요(`--apply`가 없으면 명령은 제거 대상만 보여 줍니다).

```sh
"/Applications/Remnifold.app/Contents/MacOS/remnifold" uninstall-setup --apply
```

`~/Library/Application Support/Remnifold`의 연결, 구성, 메모리 상태는 유지되며, 내려받은 업데이트 캐시는 `~/Library/Caches/velopack/RemnifoldApp`입니다.

Linux에도 제거 프로그램이 없습니다. AppImage를 삭제하기 전에 같은 방식으로 실행하세요(경로는 보관한 위치에 맞게 조정하세요).

```sh
~/Applications/RemnifoldApp-linux-x64.AppImage uninstall-setup --apply
```

`~/.config/remnifold`와 `~/.local/state/remnifold`의 연결, 구성, 메모리 상태는 유지되며, 내려받은 업데이트 캐시 `/var/tmp/velopack/RemnifoldApp`은 삭제해도 됩니다.

### 엄격한 설치 (고급)

게시된 네이티브 패키지는 [Remnifold updates](https://remnifold-updates.kagayoi.com)에서 제공됩니다. 버전 인덱스는 `releases/<version>/index.json`이며, 그 매니페스트가 아래 각 대상에 대해 해시 이름의 아카이브를 선택합니다. 내려받은 실행 파일을 실행하기 전에 GitHub를 통해 게시를 인증하세요. R2 URL이나 커밋 상태 링크만으로는 출처가 증명되지 않습니다.

| 플랫폼 | Rust 대상 |
| --- | --- |
| Windows x64 | `x86_64-pc-windows-msvc` |
| Mac Apple Silicon | `aarch64-apple-darwin` |
| Linux x64 | `x86_64-unknown-linux-gnu` |
| Linux ARM64 | `aarch64-unknown-linux-gnu` |

Mac Intel은 지원하지 않습니다.

앱은 관리자 권한 없이 사용자 단위로 설치됩니다.

| 플랫폼 | 앱 폴더 | 고정 실행 파일 |
| --- | --- | --- |
| Windows | `%LOCALAPPDATA%\Programs\Remnifold` | `bin\remnifold.exe` |
| macOS | `~/Library/Application Support/Remnifold/app` | `bin/remnifold` |
| Linux | `$XDG_DATA_HOME/remnifold` (기본값 `~/.local/share/remnifold`) | `bin/remnifold` |

`remnifold install`은 아무것도 쓰기 전에 릴리스 커밋, 성공한 CI 게시자 로그, R2 매니페스트, 아카이브 크기와 다이제스트, 패키지 출처를 검증합니다. 각 버전을 `versions/<version>`에 한 번만 풀고(설치된 버전을 다른 바이트로 덮어쓰지 않습니다), 이후 오프라인 검증을 위해 `receipts/`에 영수증을 보관하며, 고정 실행 파일을 교체합니다. 등록은 항상 고정 실행 파일을 가리키므로 업데이트 시 다시 등록할 필요가 없습니다. Windows에서는 실행 중인 MCP 서버가 재시작될 때까지 이전 실행 파일을 유지하며, 이 경우 결과에 `"restartRequired": true`가 표시됩니다.

### 최초 설치

Windows에서는 릴리스 워크플로가 서명된 bootstrap 실행 파일을 R2의 버전이 붙은 변경 불가능한 키에 올립니다. 버전 1.0.30 이후에만 존재하며, 그보다 이전 버전은 신뢰할 수 있는 소스에서 첫 실행 파일을 직접 빌드하세요. 검증과 실행 사이에 다른 사용자가 내려받은 파일을 바꿔치기할 수 없는, 신뢰할 수 있는 기존 비공개 디렉터리를 선택하세요. 아래의 따옴표 안 버전과 경로를 바꾼 뒤 PowerShell에서 실행하세요.

```powershell
$version = '<published-version>'
$bootstrap = 'C:\absolute\trusted\private\path\remnifold-bootstrap.exe'
Invoke-WebRequest -MaximumRedirection 0 -Uri "https://remnifold-updates.kagayoi.com/velopack/remnifold-bootstrap-$version-x86_64-pc-windows-msvc.exe" -OutFile $bootstrap
$signature = Get-AuthenticodeSignature -LiteralPath $bootstrap
if ($signature.Status -eq 'Valid' -and $signature.SignerCertificate.Subject -like 'CN=Open Source Developer Yuichiro Shinozaki*' -and $signature.TimeStamperCertificate) { & $bootstrap install --version $version --apply } else { Write-Error 'Signature verification failed. Do not run this file.' }
```

다운로드는 리다이렉트되어서는 안 되며, 실행하기 전에 파일이 Authenticode 검사(상태 `Valid`, 서명자 `CN=Open Source Developer Yuichiro Shinozaki`, RFC 3161 타임스탬프 있음)를 통과해야 합니다. 실패하면 실행하지 마세요. 이 서명은 누가 서명했는지만 보여 줄 뿐, 제품·버전·게시 수락까지 보증하지는 않습니다. `install`에는 여전히 인증된 네이티브 GitHub CLI가 필요하며, 게시 검증은 `install`이 직접 수행합니다([DESIGN.md](DESIGN.md#release-installation) 참조). 검증되지 않은 R2 헬퍼로 대체하거나 실패한 검증을 우회하지 마세요.

macOS와 Linux에서는 신뢰할 수 있는 출처의 [Bash 부트스트랩](scripts/bootstrap-plugin.sh)을 받아 내용을 검토하고, 작업 디렉터리 밖에 저장하세요. 네이티브 GitHub CLI, curl, tar, 그리고 `sha256sum` 또는 `shasum`이 필요합니다.

```sh
bash ./bootstrap-plugin.sh --version '<published-version>' --apply
```

`--apply`가 없으면 게시만 검증하며, `--download-only`는 검증된 아카이브를 실행하지 않고 반환합니다.

### 업데이트, 확인, 등록

```sh
remnifold install --version <published-version>                 # 게시만 검증
remnifold install --version <published-version> --apply         # 설치 또는 업데이트
remnifold install --version <published-version> --verify-only   # 설치된 버전을 오프라인으로 다시 확인
remnifold connect claude-code                                    # 등록 계획 표시
remnifold connect claude-code --apply                            # MCP 서버와 훅 등록
remnifold connect claude-code --remove --apply                   # Remnifold의 MCP 항목과 훅만 제거
```

앱 폴더의 고정 실행 파일을 사용하세요(또는 그 `bin` 폴더를 `PATH`에 추가하세요). `install`은 Windows에서는 `%LOCALAPPDATA%\remnifold`, 그 외에는 `~/.cache/remnifold` 아래의 작업 폴더를 사용하며, `--work-root <absolute-path>`로 앱, 구성, 저장소 폴더 밖의 다른 폴더를 지정할 수 있습니다. 증인(witness) 로그가 만료되면 새 설치는 안전하게 실패하며, 보관된 영수증으로는 계속 `--verify-only`를 사용할 수 있습니다. 릴리스 소스 메타데이터에 대한 GitHub 접근은 여전히 필요합니다.

`connect claude-code --apply`는 `claude mcp add --scope user remnifold -- <fixed executable> mcp`로 MCP 서버를 등록하고(Claude Code CLI가 `PATH`에 없으면 이 명령을 출력하며, `--cli <path>`로 CLI를 지정할 수 있습니다), `SessionStart`, `UserPromptSubmit`, 비동기 `Stop`, `PreToolUse` 훅을 Claude Code의 `settings.json`(`$CLAUDE_CONFIG_DIR` 또는 `~/.claude`)에 병합합니다. 기존 Remnifold 훅 항목만 교체되며, 첫 변경 시 `settings.json.remnifold-backup`을 남깁니다. 형식이 잘못된 설정 파일은 변경하지 않고 둡니다. `connect <tool> --remove`(먼저 미리 보기, `--apply`로 실제 적용)는 Remnifold 자신의 MCP 항목과 훅만 제거하고 다른 설정은 모두 유지하며, `--executable <path>`를 지정하면 해당 실행 파일을 가리키는 항목만 제거합니다. `connect codex --apply`는 `codex mcp add`로 MCP 서버를 등록합니다. Codex는 플러그인 밖에서 훅을 등록하는 공식 방법을 제공하지 않으므로, 자동 recall과 캡처는 Claude Code를 통해 동작합니다. 등록한 뒤에는 새 세션을 시작하세요.

`Stop` 캡처는 Codex rollout뿐 아니라 Claude Code 트랜스크립트도 읽습니다. 사람의 요청과 마지막 어시스턴트 텍스트만 저장하며, 사고 과정, 도구 호출과 결과, 요청 사본, 다른 세션의 행은 건너뜁니다. 백그라운드 알림 뒤의 응답은 별도의 이어지는 턴으로 저장되므로, 이미 저장된 턴의 캡처 키는 바뀌지 않습니다. Claude Code 기록에는 `sm_source: "claude-code"`가 붙습니다. 메모리 시스템이 둘이 되지 않도록 Claude Code의 내장 자동 메모리를 `"autoMemoryEnabled": false`로 비활성화하세요. Remnifold는 해당 파일을 읽거나 가져오지 않습니다. 이 저장소 안에서는 소스의 `.mcp.json`(`cargo run`)이 사용자 등록보다 우선합니다. 설치된 앱을 대신 사용하려면 추적되지 않는 `.claude/settings.local.json`에 `"disabledMcpjsonServers": ["remnifold"]`를 추가하세요.

이전의 Codex 플러그인 명령 `plugin-update`와 `plugin-bootstrap`은 폐지되었으며 `USE_REMNIFOLD_INSTALL`을 반환합니다. 대신 독립 실행형 앱을 설치하세요. 그러면 기존 플러그인은 Codex에서 제거해도 됩니다.

### 서버에 연결

```sh
remnifold setup --base-url https://your-worker.example.workers.dev
```

API 키가 아직 제공되지 않았다면 숨김 프롬프트에 입력하세요. Setup은 연결을 기록하기 전에 인증된 세션을 검증합니다. 기존 연결을 업데이트하려면 `--force`를 사용하세요. 관련 없는 설정은 유지됩니다. `REMNIFOLD_API_URL`과 `REMNIFOLD_API_KEY`로 프로세스 단위 재정의를 지정할 수 있습니다. 비대화형 설정에서는 `--api-key-stdin`이 표준 입력에서 키를 읽습니다.

연결과 로컬 상태는 Codex나 Claude Code와는 별개인 앱 자체의 구성 폴더에 저장됩니다.

| 플랫폼 | 연결(`remnifold.json`)과 서버 상태 | 캡처 및 recall 상태 |
| --- | --- | --- |
| Windows | `%APPDATA%\Remnifold` | 같은 폴더 |
| macOS | `~/Library/Application Support/Remnifold` | 같은 폴더 |
| Linux | `$XDG_CONFIG_HOME/remnifold` (기본값 `~/.config/remnifold`) | `$XDG_STATE_HOME/remnifold` (기본값 `~/.local/state/remnifold`) |

`REMNIFOLD_HOME`은 둘 다 재정의하며(연결은 `<REMNIFOLD_HOME>/remnifold.json`, 상태는 `<REMNIFOLD_HOME>/remnifold/` 아래, 앱은 `<REMNIFOLD_HOME>/app`), `setup --config-dir <directory>`는 연결을 다른 위치에 기록합니다. 이전 버전은 이 파일들을 Codex 홈(`CODEX_HOME`, 기본값 `~/.codex`)에 보관했습니다. 새 폴더에 연결이 없으면 첫 실행 시 유효한 기존 연결을 새 폴더로 복사하고 이전 캡처 및 recall 상태를 읽으며, 새 상태는 새 폴더에만 기록합니다. 기존 파일은 삭제되지 않으며, `server-setup`은 `--apply`가 있을 때만 기존 상태를 옮깁니다.

연결과 공간 탐색을 확인하려면 `whoAmI` MCP 도구를 실행하세요.

기존 Codex 세션을 가져오려면 `remnifold import --limit 10`으로 제한된 배치를 미리 확인하세요. 정리된(sanitized) 기록을 업로드하려면 `--apply`를 추가합니다. 미리 보기는 원격 메모리에 아무것도 기록하지 않으며, 가져오기를 반복해도 결정적(deterministic) 캡처 식별자를 재사용합니다. `--codex-home`은 Codex 세션을 읽어 올 위치를 지정합니다.

## 서버 생성 또는 업데이트

패키지에는 Worker 번들과 마이그레이션이 포함되어 있습니다. 프로세스 환경 변수 `CLOUDFLARE_API_TOKEN` 또는 `--token-stdin`으로 Cloudflare API 토큰을 제공한 뒤 기존 리소스를 미리 확인하세요.

```sh
remnifold server-setup --account-id <Cloudflare-account-id>
```

기본값은 인증된 읽기 전용 탐색입니다. 토큰에는 계정 읽기, Workers scripts, D1, Vectorize 권한이 필요하며, 도메인이나 라우트를 변경하려면 해당하는 Zone, Routes, Custom Domain 권한도 필요합니다. Setup은 Wrangler OAuth 자격 증명을 가져오지 않습니다. 토큰과 메모리 API 키는 외부에 노출되지 않도록 관리하세요.

기존 설치의 경우 `--config <absolute-existing-jsonc-path>`를 전달하면 구성을 덮어쓰지 않고 읽기만 합니다. Setup은 리소스 이름을 `remnifold`, `kagayoi-memory`, `cloudflare-supermemory` 순으로 재사용하며, 명시한 `--worker-name`, `--database-name`, `--vector-index-name`이 우선합니다. 계획이 기존 Worker, D1 데이터베이스, Vectorize 인덱스를 선택하는지 확인하세요. 구성된 ID와 벡터 차원/메트릭은 일치해야 합니다. 기존 바인딩, cron 일정, 라우트는 유지되며, 충돌이 있으면 설정이 중단됩니다. 이 업그레이드를 위해 대체 서버를 만들거나 메모리를 복사하지 마세요.

프로비저닝이나 배포를 하려는 경우, 선택한 메모리 키를 `REMNIFOLD_API_KEY`로 제공하고 계획을 적용하세요.

```sh
remnifold server-setup --account-id <Cloudflare-account-id> --apply --location apac
```

미리 보기와 같은 구성 및 이름을 사용하세요. `--apply`는 누락된 리소스를 만들고, 제공된 키를 Worker 시크릿으로 설치하고, 추가형(additive) D1 마이그레이션을 적용하고, 내장된 Worker를 배포하고, 엔드포인트를 확인합니다. 기존 서버라면 의도치 않게 키가 교체되지 않도록 기존 키를 제공하세요. 키를 제공하지 않으면 Setup은 시크릿을 그대로 두고 인증된 검증을 건너뛴 것으로 보고하며, 헬스 체크만으로는 마이그레이션과 MCP 접근이 검증되지 않습니다. 키를 제공하면 세션 검증, 원격 MCP 초기화, `search_memory`, `getDocument`, `add_memory` 탐색이 가능해집니다.

클라이언트를 연결하거나 업데이트하기 전에 서버 업데이트와 마이그레이션을 먼저 적용하세요. Setup은 재개 가능한 구성을 저장소 밖의 앱 구성 폴더에 `server/config.json`으로 저장합니다([서버에 연결](#서버에-연결) 참조). 다른 외부 위치를 선택하려면 `--state <absolute-json-path>`를 사용하세요. 업데이트를 위해 이 상태를 보관하고, 생성된 ID나 자격 증명은 절대 커밋하지 말며, 실패한 단계를 바로잡은 뒤 같은 명령을 다시 실행하세요. 기존 사용자 지정 엔드포인트에는 `--base-url`을, 모든 옵션을 보려면 `--help`를 사용하세요. Cloudflare 사용 요금은 서버를 호스팅하는 계정에 청구됩니다.

1.0.22에서는 클라이언트를 업데이트하기 전에 해당 릴리스의 `server-setup --apply`로 서버를 업데이트하세요. 마이그레이션 `0013_memory_created_index.sql`은 일 단위 범위 정리를 위한 인덱스를 추가하며, 저장된 기록이나 ID는 변경하지 않습니다.

1.0.21에서는 클라이언트를 업데이트하기 전에 해당 릴리스의 `server-setup --apply`로 서버를 업데이트하세요. 마이그레이션 `0010_capture_variants.sql`, `0011_consolidation_recall_opt_out.sql`, `0012_organization_retry_fairness.sql`은 검증된 캡처 변형을 추가하고, recall에서 제외된 소스가 포함된 과거 체크포인트를 무효화하며, 이후의 기존 방식 보류 시간을 1시간으로 줄입니다. 원본 기록과 ID는 유지되며, 영향을 받은 체크포인트는 적격한 소스로부터 다시 생성될 수 있습니다.

## 메모리 동작

- 완료된 사용자 요청과 최종 답변은 소스 폴더 출처 정보와 함께 공유 `memories` 컬렉션에 저장됩니다. 첨부된 스킬 정의와 브라우저/페이지 상태 봉투(envelope)는 저장되는 요청 텍스트에서 제외되며, 최종 답변은 그대로 보존됩니다. 결정적 캡처 식별자 덕분에 재시도해도 중복을 만들지 않고 이미 저장된 같은 기록을 재사용합니다.
- 내용 카테고리를 찾으려면 `listTopics`를 사용하고, 그 `topic`을 `listDocuments` 또는 `listMemories`에 전달하세요. 목록에는 이전 폴더 공간과 공유 컬렉션 양쪽의 기록이 모두 포함됩니다. 현재 주제 라벨이 없는 기록(분류에 실패했거나, 비활성화되었거나, 의도적으로 건너뛴 기록 포함)을 보려면 `topic: "__unclassified__"`를 사용하세요.
- 수동 `add_memory`는 공유 컬렉션에 저장합니다. 기록을 읽은 뒤 잊게 하려면 `add_memory`를 `action: "forget"`, 해당 `documentId`, 소스 `containerTag`와 함께 호출하세요. 호환성을 위해 저장된 내용을 그대로 지정하는 방식도 계속 지원됩니다. Codex 플러그인에서는 MCP 클라이언트가 제공하는 절대 경로 `sourceFolder` 또는 단일 워크스페이스 루트가 출처 정보를 추가하며, 원격 MCP 도구는 대신 명시적인 `projectId`를 받습니다. 명시적인 `containerTag`는 저장 위치를 재정의합니다. 플러그인은 자신의 설치 폴더를 사용자의 프로젝트로 사용하지 않습니다. `listSpaces`는 호환성과 진단을 위해 물리적 공간을 노출합니다.
- 프롬프트 recall은 주제별로 여러 공간을 검색합니다. 짧은 후속 질문은 같은 세션의 이전 사용자 메시지에서 주제를 복원할 수 있으며, 큰 도구 출력이 사이에 있어도 가능합니다. 이력 복원에는 한도가 있으며, 구체적인 주제가 없으면 주제 검색을 실행하지 않습니다. 자동 recall은 캡처된 각 턴과 부분의 사용 가능한 최신 리비전을 선택하고, 수동 검색은 과거 리비전도 유지합니다. 전역 검색은 이전의 탐색 한도를 넘는 공간까지 포함하며, 이전 서버는 공간 100개로 제한되는 호환 경로를 사용하고 탐색이 불완전하면 이를 알립니다. 부분 검색은 사용할 수 없는 공간과 문서 인덱스를 보고하면서 성공한 결과는 유지합니다. 초기 세션 이벤트는 recall 상태를 재설정할 뿐입니다. 비어 있지 않은 일반 프롬프트마다, 주제 일치와는 별개로 프로젝트 전반에서 최근 완료된 작업을 최대 두 건, 출처 프로젝트, 날짜, 문서 ID와 함께 표시합니다. 현재 작업과 중복된 작업 기록은 제외됩니다. 이 요약은 별도로 표시되므로, 관련성을 암시하지 않고 이전 작업을 떠올리게 하는 용도로 쓸 수 있습니다. `remnifold.json`의 `maxRecentTasks`를 0에서 3 사이의 정수로 설정하세요. 0이면 이 최근 작업 맥락이 비활성화됩니다. 이 목록 모드가 없는 이전 서버에서는 계속 주제 recall이 제공됩니다.
- 수동 검색은 저장된 프로젝트 이름과도 일치합니다. 프로젝트 이름만으로 검색하면 일치하는 소스 프로젝트와 더 최신의 소스 기록을 우선하며, 프로젝트와 주제를 함께 검색할 때는 여전히 주제 포괄 범위를 우선합니다.
- 자동 맥락에는 제목, 설명, 섹션 이름, 타임스탬프, 문서 ID가 포함됩니다. 인덱스에 없는 세부 정보가 있으면 일치하는 원문의 짧은 구절이 노출될 수 있으며, 이는 재사용하기 전에 확인해야 하는 과거의 원본 텍스트입니다. Codex는 필요할 때 `getDocument`로 전체 기록을 엽니다.
- 주입되는 `<remnifold-index>` 블록, 검색 결과, recall 알림의 고정 문구는 그것을 작성하는 환경의 언어를 따릅니다. 순서는 `REMNIFOLD_LANGUAGE`, 다음으로 `LC_ALL`, `LC_MESSAGES`, `LANG`, 그다음으로 OS 표시 언어(Windows 사용자 UI 언어, macOS `AppleLanguages`)입니다. 일본어 환경에서는 일본어 문구를, 그 밖의 환경에서는 영어 문구를 사용합니다. 저장된 제목, 설명, 발췌문은 번역되지 않고 저장된 그대로 표시됩니다.
- 주제 recall은 기본적으로 최대 세 건의 기록을 반환합니다. 자동 recall과 수동 검색의 한도를 바꾸려면 `remnifold.json`의 `maxMemories`를 1에서 5 사이의 정수로 설정하세요. 연결을 업데이트할 때 Setup은 기존 값을 유지합니다. 자동 맥락은 3,000 UTF-16 코드 단위로도 제한되므로, 그보다 적은 기록만 들어갈 수 있습니다.
- 자동 맥락은 최근 작업을 주제 인덱스와 별도로 추적하며, 세션과 트랜스크립트 정보를 사용할 수 있을 때는 이후 프롬프트에서 변경되지 않은 항목을 생략합니다. 업데이트된 기록은 다시 나타날 수 있고, 컴팩션이나 맥락 초기화는 중복 억제를 재설정합니다. 로컬 recall 상태는 대화 텍스트를 복사하지 않고 선택한 Codex 홈의 `remnifold/recall-state/` 아래에 유지되며, 상태를 사용할 수 없어도 일반 recall은 계속 활성 상태입니다. 쿼리 매칭과 상태의 세부 사항은 [DESIGN.md](https://github.com/1llum1n4t1s/remnifold/blob/main/DESIGN.md#recall)에 설명되어 있습니다.
- 서버는 매일 03:00 JST에 프로젝트를 점검합니다. 프로젝트에 통합되지 않은 기록이 20건 이상 있거나, 통합되지 않은 기록이 있고 마지막으로 성공한 체크포인트로부터 3일이 지난 경우(첫 체크포인트 이전에는 가장 오래된 대기 기록으로부터) Workers AI에 통합 체크포인트 생성을 요청합니다. 프로젝트에서 처음으로 점검이 필요해지면 서버는 기존의 전체 백로그를 제한된 크기의 AI 배치로 연속해서 자동 처리하므로, 오래된 기록을 위한 수동 백필 명령은 필요하지 않습니다. 원본 기록은 유지되며 체크포인트에서 링크됩니다. 이전 체크포인트는 대체되고, 소스를 변경하거나 잊게 하면 그에 의존하던 체크포인트가 무효화됩니다.
- 매일의 점검을 기다리지 않고 프로젝트 체크포인트를 한 배치만큼 즉시 진행하려면 `consolidate_memory` MCP 도구를 사용하세요. 원격 MCP에는 저장된 출처 ID인 `projectId`가 필요합니다. Codex 플러그인은 워크스페이스 루트가 정확히 하나일 때 현재 프로젝트를 추론하거나, `sourceFolder` 또는 `projectId`를 명시적으로 받을 수 있습니다. 한 번 호출할 때 처리하는 새 기록은 최대 40건이므로, 백로그가 더 크면 반복해서 호출하세요. 수동 실행에도 통합되지 않은 소스 기록이 최소 한 건은 있어야 합니다.
- 자동 recall은 주제 관련성이 같다면 현재 체크포인트를 우선하고, 선택된 체크포인트가 이미 다루는 원본의 반복을 피합니다. 체크포인트에 없는 구체적인 일치 세부 정보는 여전히 해당 원본을 노출합니다. 수동 검색은 계속 원본 기록을 노출합니다.
- 체크포인트의 내용과 메타데이터는 직접 편집할 수 없으며, 업데이트하면 HTTP 409가 반환됩니다. 대신 원본 기록을 편집하고 체크포인트를 다시 생성하세요. 재보강(re-enrichment)과 삭제는 계속 사용할 수 있습니다.
- 캡처는 구성된 메모리 API 키와 일반적인 시크릿 패턴을 가리고, 확인되지 않은 기록은 다시 시도합니다. 로컬 캡처 상태는 앱 상태 폴더의 `capture-state/` 아래에 유지되며, Codex 홈에 있는 이전 버전의 영수증도 계속 읽을 수 있습니다.

캡처 실패에는 예외 텍스트나 자격 증명을 출력하지 않고 고정된 `stage`, `cause`, `action` 코드가 포함됩니다. `stage=receipt; cause=invalid`처럼 `action=repair-required`인 결과는 해당 로컬 캡처 상태를 복구해야 하며, 재시도만으로는 계속 실패합니다. `action=retry-or-repair`는 일반적인 재시도 동작을 유지하면서 지속적인 연결 또는 응답 문제를 조사할 수 있게 합니다. API 도달 가능 여부를 별도로 확인하려면 압축을 푼 플러그인 폴더에서 다음을 실행하세요.

```sh
remnifold status
```

주제는 기존 Workers AI 보강 단계에서 기록의 내용으로부터 도출됩니다. 분류는 비동기로 이루어집니다. 실질적인 결과가 없는 짧은 확인 응답과 이와 비슷한 정보량이 적은 기록은 수동 검색에서는 계속 사용할 수 있지만 자동 recall과 보강에서는 제외되며, 명시적인 주제 라벨은 그대로 유지됩니다. 폴더 이름은 소스 메타데이터로 유지됩니다. 기존 기록은 원래의 문서 ID와 저장 위치를 유지하며 같은 주제 브라우저에 표시됩니다.

주제 탐색이나 통합을 사용하는 클라이언트를 설치하기 전에 서버를 업데이트하고 마이그레이션을 적용하세요. 기존 기록을 분류하려면 기존 메모리 연결을 사용해 플랫폼 실행 파일로 제한된 배치를 미리 확인하세요.

```sh
remnifold topics-setup --limit 10
```

기본 배치 크기는 10이며 `--limit`은 1–50을 받습니다. 원격 보강을 의도할 때는 `--apply`를 추가해 그 유한한 배치를 큐에 넣으세요. 이는 기존 Workers AI 과금을 사용하고, 파생된 AI/벡터/사실 보강을 새로 고치며, 영향을 받는 프로젝트 체크포인트를 무효화하면서 옵트아웃과 리비전 가드는 유지합니다. 결과는 완료된 분류가 아니라 수락된 작업을 보고합니다. 처리가 끝나기를 기다린 뒤에 다음 배치를 선택하고, `getDocument`와 `listTopics`로 결과를 확인하세요.

API 키는 서버 설치 하나에 있는 기록에 대한 접근 권한을 부여합니다. 신뢰 경계가 다르면 설치를 분리하세요. 마스킹은 의도치 않은 시크릿 캡처를 줄여 주지만, 자신의 백엔드에 저장하기로 한 내용은 직접 검토하세요.

## 메모리 정리

앱을 설치하고, `0009_memory_organization.sql`의 정리 스키마와 `0013_memory_created_index.sql`의 범위 정리 인덱스를 포함한 [현재 서버 마이그레이션](#서버-생성-또는-업데이트)을 적용한 뒤에는 [organize-memory 스킬](skills/organize-memory/SKILL.md)로 필요할 때 메모리를 정리할 수 있습니다. 예를 들어 `$organize-memory 7`이나 "최근 7일 동안의 메모리를 정리해 줘"처럼 요청합니다. 일수(1–3650)는 서버 시간 기준으로 최근 N일 안에 생성된 메모리를 선택하며, 실행은 그 범위에 처리할 작업이 더 없거나 선택적인 배치 상한(기본값 3, 최대 20)에 도달할 때까지 배치를 반복합니다. 일수를 지정하지 않으면 전역 큐에서 배치 하나를 가져옵니다. 정리는 더 이상 Codex 예약 작업으로 등록되지 않으며, [setup-remnifold](skills/setup-remnifold/SKILL.md)는 이전 버전이 남긴 작업을 일시 중지하기만 합니다.

각 실행은 현재 설치된 실행 파일을 사용합니다. 배치 하나는 부분 또는 보류된 작업을 포함해 대상 기록을 최대 20건, 소스 48,000 UTF-16 단위까지 처리합니다. 이는 완료된 기록 수나 전체 읽기/토큰 양을 보장하는 값이 아닙니다. 후보 요약과 표적 증거 읽기는 이에 더해 처리됩니다. 조회는 배치당 두 번으로 제한됩니다. 해결되지 않은 작업은 대량 탐색으로 확장되지 않고 보류되며, 상한에서 멈춘 범위 실행은 다시 실행하면 됩니다.

CLI를 직접 사용하려면 설치된 실행 파일을 실행하세요.

```text
remnifold memory-organize prepare [--days N]
remnifold memory-organize lookup --line
remnifold memory-organize commit --line
remnifold memory-organize sources <summary-document-id> [cursor]
```

이 명령들은 기존 소유자 API 키 연결을 사용하며, 원격 MCP OAuth 자격 증명으로는 정리를 승인할 수 없습니다. Codex를 예약하지도, 서버를 배포하거나 마이그레이션하지도 않습니다. `prepare`는 30분짜리 리스를 만듭니다. `--days N`은 전역 탐색 큐를 이동시키지 않고 최근 N일 안에 생성된 메모리로 범위를 제한하며, 범위 응답은 각각 `range.cursor`/`range.exhausted`를 반환합니다. 다음 호출에서는 null이 아닌 커서를 `--range-cursor <cursor>`로 전달하며, 이전 서버는 대체 동작 없이 이를 거부합니다. 이 명령은 소스 ID/리비전/청크, 이어서 처리할 메모, 후보 요약을 반환하며, 읽기 전용 연결 확인이 아닙니다. `lookup`은 표준 입력에서 `{"query":"a concise topic"}`을 읽습니다. `commit`은 `batchId`, `summaries`, `coverage`를 포함한 JSON 객체를 읽습니다. 실행 스킬의 정확한 필드 제한, 분할 규칙, 멀티파트 `groupProgress` 절차를 따르세요. 모든 청크와 중요한 주제가 표현되고 결론, 이유, 증거, 불확실성이 보존되었을 때만 `complete`를 선언하세요. 그렇게 할 수 없으면 부분 이어하기 또는 보류를 사용하세요. 이 판단은 Codex가 합니다. commit 영수증을 확인하고, 게시된 요약과 미완료 진행 상황을 구분하세요.

`prepare` 전에 안전한 표준 입력 경로와 전체 응답을 보관하고 페이지 단위로 읽을 방법을 마련하세요. Windows에서는 리스를 얻기 전에 실행 스킬의 로컬 합성 PTY 프로브를 따르세요. 모든 소스 청크, 이어하기 메모, 요약 후보는 같은 보관된 응답에서 읽으세요. 표시가 잘렸다면 다른 배치를 준비하지 말고 같은 호출의 응답에서 복구하세요. commit 전에 정확한 JSON을 메모리에서 검증하세요. 모든 coverage 항목에는 준비된 ID/리비전과 명시적인 `summaries` 배열이 필요하며, 부분 또는 보류 항목에는 `summaries:[]`를 포함합니다. 페이지 읽기, 사전 점검, 중단된 실행 이후의 복구는 [실행 스킬](skills/organize-memory/SKILL.md#one-batch)을 따르세요.

JSON 표준 입력에는 30초의 제한 시간과 이스케이프를 포함해 128,000바이트(UTF-8)의 한도가 있습니다. `--line`에는 간결한 JSON 한 줄과 끝의 개행이 필요하며, 이 옵션이 없으면 객체를 보낸 뒤 표준 입력을 닫으세요. 에코가 없는 입력 채널을 사용하고, 생성된 텍스트를 셸 인수와 명령 로그에 남기지 마세요. 지원하지 않는 서버에서는 저장/잊기 대체 동작 없이 명시적으로 중단됩니다.

Windows PTY 입력에서는 전용 PTY에서 `memory-organize lookup --line --private-stdin` 또는 `memory-organize commit --line --private-stdin`으로 네이티브 실행 파일을 직접 실행하세요. stderr에 `REMNIFOLD_STDIN_READY`가 나타날 때까지 기다린 뒤, 간결한 JSON 한 줄과 마지막 개행을 정확히 한 번만 보내고 그 뒤에는 어떤 바이트도 보내지 마세요. 터미널 변환에 대비한 여유를 두기 위해 페이로드와 개행의 합계를 127,000 UTF-8 바이트 이하로 유지하세요. 비공개 모드는 에코와 줄 편집을 끄고, 종결 문자 이후 원래의 콘솔 모드를 복원합니다. 준비 신호가 없거나 어느 단계든 실패하면 입력 전송을 멈추고 해당 PTY를 닫으세요. 그 PTY를 재사용하지 말고, 입력이 남아 있을 수 있는 동안에는 에코를 복원하지 마세요. 공유 콘솔이나 Git Bash 래퍼는 사용하지 마세요. 리디렉션된 표준 입력과 Windows 이외의 호스트는 `--private-stdin`을 거부하며, macOS/Linux에서는 에코가 없는 파이프에 줄 또는 EOF 입력을 사용하세요. 전체 입력 절차는 [실행 스킬](skills/organize-memory/SKILL.md#safe-stdin-and-failures)을 참조하세요.

원본은 삭제되지 않습니다. 완전히 요약에 반영된 원본은 그 요약이 유효한 동안에만 일반 검색과 주제 recall에서 빠지며, 원본의 ID와 본문은 직접 조회할 수 있고, 최근 작업 인식은 기존의 적격 조건을 유지합니다. 충돌하는 캡처는, 서버가 서로 다른 단일 파트 v2 캡처 키와 정확한 본문 해시를 독립적인 증거로 검증하지 않는 한, 계속 보이는 상태로 보류됩니다. 이 검증은 어느 버전이 가장 최신인지를 확정하지 않으며, 해결되지 않은 모순은 요약에 남겨야 합니다. 일반적인 보류는 1시간 뒤에 적격 상태가 되지만, 해결되지 않은 캡처 충돌은 자동 재시도를 계속 막습니다. 한도가 있는 큐는 오래된 처리 대상 작업, 이어하기, 아직 손대지 않은 기록 사이에서 진행을 나눠 가지며, 한 번의 실행으로 아카이브 전체가 완료되었다고 볼 수는 없습니다. 변경하거나 잊게 하면 의존하는 요약이 무효화되고 남아 있는 원본이 다시 노출됩니다. 이전 프로젝트 체크포인트는 전체 기록으로 처리될 때까지 정보를 계속 노출할 수 있습니다. AI가 활성화된 경우, 이미 시맨틱 검색을 뒷받침하는 원본은 검증된 요약 임베딩/벡터가 준비될 때까지 사라지지 않고 기다립니다. AI가 비활성화된 경우에는 어휘 기반 커버리지로 충분하며, AI를 활성화하면 시맨틱 대체물이 준비될 때까지 원본이 다시 나타날 수 있습니다. 벡터 복구는 비동기이며, 15분 간격의 cron이 완료 기한은 아닙니다. 요약 임베딩은 기존 Workers AI 과금을 사용하며, 산문은 Codex가 생성합니다.

`sources`는 읽기 전용이며 기록된 소스 ID, 공간, 날짜를 한 번에 최대 50개씩 페이지로 보여 줍니다. 본문은 getDocument로 가져오고, 체크포인트의 자체 소스 미리 보기/커서를 따라가 기록된 구성원에 도달하세요. 서로 다른 출처 경로는 같은 ID를 반복할 수 있습니다. 현재의 네이티브 및 원격 MCP getDocument 응답에는 소스 미리 보기와 선택적 `sourceCursor` 페이지 이동도 포함되며, 이 필드가 없는 이전 네이티브 플러그인은 CLI를 사용할 수 있습니다. 부모가 바뀌면 커서는 충돌로 중단됩니다. [정리 설계](DESIGN.md#memory-organization)에 커버리지, 무효화, 벡터 복구가 자세히 설명되어 있습니다.

## 1.x 이름에서 업그레이드

설치되어 있는 `kagayoi-memory`, 1.x `cloudflare-supermemory`, 또는 `remnifold` Codex 플러그인을 제거하고 독립 실행형 앱을 설치하세요. 클라이언트는 구성 폴더에서, 그다음 이전 Codex 홈에서 `remnifold.json`을 먼저 읽고, 이어서 `kagayoi-memory.json`과 `supermemory.json`을 읽습니다. `REMNIFOLD_API_URL`과 `REMNIFOLD_API_KEY`가 일반적인 환경 변수이며, 이전의 `KAGAYOI_MEMORY_*`, `SUPERMEMORY_API_URL`, `SUPERMEMORY_CODEX_API_KEY`, `CLOUDFLARE_MEMORY_API_KEY` 리더는 호환성을 위해 남아 있습니다. `remnifold setup`을 실행하면 이전 파일을 유지한 채 일반 `remnifold.json`을 기록합니다. 기존 캡처 영수증과 recall 상태는 이전 제품 폴더에서 계속 읽을 수 있습니다. 서버 설정은 `remnifold`, `kagayoi-memory`, `cloudflare-supermemory` 순으로 이름이 붙은 리소스를 재사용하며, 명시한 리소스 이름이 항상 우선합니다. 로컬 앱을 바꾸는 것만으로 Worker가 배포되거나 원격 데이터베이스가 이동하지는 않습니다. 기존 문서 ID, 캡처 ID, 프로젝트 출처 정보, 보존된 `sm_*` 메타데이터는 그대로 유지됩니다. 저장소 호환성의 세부 사항은 [DESIGN.md](https://github.com/1llum1n4t1s/remnifold/blob/main/DESIGN.md#storage-and-provenance)에 있습니다.

Remnifold는 자체적인 셀프 호스팅 API를 구현합니다. 호스팅형 Supermemory 서비스와는 독립적이며, Codex의 내장 로컬 메모리 파일을 읽거나 수정하지 않습니다.

아키텍처와 저장소 구성은 저장소의 [DESIGN.md](https://github.com/1llum1n4t1s/remnifold/blob/main/DESIGN.md)를 참조하세요. 기여자용 제약, 검증, 패키징 명령은 [AGENTS.md](https://github.com/1llum1n4t1s/remnifold/blob/main/AGENTS.md)에 있습니다.

[MIT License](LICENSE)에 따라 라이선스가 부여됩니다.
