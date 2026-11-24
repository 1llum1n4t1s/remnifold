[English](README.md) | [日本語](README.ja.md) | [简体中文](README.zh-CN.md) | [繁體中文](README.zh-TW.md) | [한국어](README.ko.md) | [Español](README.es.md) | [Français](README.fr.md) | [Deutsch](README.de.md) | [Português (Brasil)](README.pt-BR.md)

# Remnifold

一个自托管的 Cloudflare 记忆服务器，提供远程 MCP 访问，并附带独立的本地应用，让 Claude Code 和 Codex 能够跨项目文件夹回忆有价值的实现历史。

应用会把已完成的工作保存到共享集合中，并将其所在的项目文件夹作为来源信息保留。你可以按内容主题浏览记录，也可以询问某个主题，例如“Chrome 扩展”：回忆功能会在一次服务器请求中跨记忆空间搜索，注入简明的索引，并让助手在复用实现之前通过 MCP 打开相应的源记录。

## 系统要求

远程 MCP 客户端无需在本地安装任何东西，即可连接已部署的服务器。本地应用是适用于你的操作系统和 CPU 的单个原生可执行文件；它的 MCP 服务器、钩子、连接设置、历史导入和安装程序都不需要 Node.js、Codex 或 Claude Code 的插件系统。普通用户通过 Setup 安装程序安装，无需使用命令行，并且会自行保持更新。面向高级用户的严格安装方式 `remnifold install` 需要已认证的原生 GitHub CLI。向 Claude Code 注册时，如果有 Claude Code CLI 就使用它，否则会打印出需要执行的确切命令。

服务器的设置与维护使用同一个原生可执行文件，并需要你自己的 Cloudflare 账户，且已启用 Workers、D1、Vectorize 和 Workers AI。使用平台安装包不需要安装 Node.js、包管理器、Wrangler 或 Rust。

## 连接远程 MCP 客户端

在服务器按下文方式更新并验证之后，使用 `https://your-worker.example.workers.dev/mcp` 作为 Streamable HTTP MCP 端点。兼容的客户端可以通过 OAuth 2.1 发现、公开的动态客户端注册和 PKCE S256 进行认证。在服务器的授权页面输入现有的记忆 API 密钥，并批准该命名客户端。如果完成页面没有自动返回客户端，请点击其中的链接继续。OAuth 令牌仅对 MCP 有效；现有的 REST 客户端继续使用 API 密钥。支持配置 Bearer 凭据的客户端也可以直接使用记忆 API 密钥。请妥善保管密钥和令牌。

[Grok Bot 的 Connect apps](https://docs.x.ai/grok-bot/overview) 和 [Grok connectors](https://docs.x.ai/grok/connectors) 提供了应用连接与自定义 MCP 连接的途径。客户端支持 OAuth 时请选择 OAuth；Grok Bot 是否允许手动输入 Bearer 密钥尚未验证。

对于 ChatGPT，请按照[私有插件设置](https://developers.openai.com/plugins/build/plugins)操作：选择 **+ Add custom MCP server**，输入 `/mcp` URL 和认证信息，然后选择 **Create as a plugin**。这种私有连接无需提交到公共目录。[ChatGPT dots](https://learn.chatgpt.com/docs/dots/computers-and-apps) 使用受支持、已安装并已启用的插件；请在你的账户中确认该插件出现在 dots 中并能正常工作。在 dots 中直接输入任意 MCP URL 的方式尚未验证。

服务器会要求 AI 搜索相关上下文、在需要时打开完整记录，并保存持久的成果。工具的使用由客户端和模型决定，因此不保证一定会自动搜索或保存。需要时，请明确要求 AI 去搜索或保存。这种远程集成不会取代本地 stdio 服务器或钩子；请避免在同一个客户端中重复注册远程服务器。

## 安装本地应用

### Setup 安装程序（推荐）

在 Windows x64 上，下载 [`RemnifoldApp-win-Setup.exe`](https://remnifold-updates.kagayoi.com/velopack/RemnifoldApp-win-Setup.exe)（由发布者签名，无需管理员权限）并运行。下文所述的 macOS 和 Linux Setup 安装包从 1.0.32 之后的第一个版本起，与 Windows 版同时发布。Setup 会为你的用户把应用安装到 `%LOCALAPPDATA%\RemnifoldApp`，将其注册到 Claude Code 和 Codex（仅限能找到命令行工具的那些），并在终端中打开首次运行向导，说明下一步：连接你的服务器（`remnifold setup`）。其检查不需要 GitHub 账户。在 macOS（Apple Silicon）上，打开 `RemnifoldApp-osx-Setup.pkg`（已使用发布者的 Developer ID 签名并经过公证）。它会安装 `Remnifold.app`（选择“仅为我安装”时会放在 `~/Applications`；为所有用户安装则放在 `/Applications`，此时应用更新可能需要管理员密码），并启动应用一次，从而将其注册到 Claude Code 和 Codex。如果之后才安装某个客户端的命令行工具，下次启动 Remnifold 时会注册它。运行 `"/Applications/Remnifold.app/Contents/MacOS/remnifold" setup --base-url <URL>`（如果装在 `~/Applications`，请相应调整路径）来连接你的服务器。在 Linux（x64 或 ARM64）上，Setup 是单个 AppImage（`RemnifoldApp-linux-x64.AppImage` 或 `RemnifoldApp-linux-arm64.AppImage`）：把它放在你想长期保存的位置（例如 `~/Applications`），赋予可执行权限并不带参数运行一次，这会把该 AppImage 注册到 Claude Code 和 Codex；之后如果移动了文件，下次启动时会注册新的位置。运行 `<path-to-AppImage> setup --base-url <URL>` 来连接你的服务器。AppImage 不带代码签名，因此在 Linux 上，应用信任来自更新站点的 HTTPS 以及更新源中的校验和。

更新是自动的。在 MCP 会话运行期间，应用每天最多检查一次更新源（检查失败也会等待一天），并在后台下载较新的版本，不会打断任何操作。新版本会在你下次登录时应用（Windows：`Run` 项；macOS：LaunchAgent `~/Library/LaunchAgents/com.kagayoi.RemnifoldApp.update.plist`；Linux：自启动项 `~/.config/autostart/com.kagayoi.RemnifoldApp.update.desktop`），并且仅在没有其他 Remnifold 进程运行时才会应用，因此不会关闭已打开的会话。应用前，应用会验证下载包中的每个可执行文件都由发布者签名（macOS：发布者的 Developer ID；Linux：没有签名，但下载文件夹 `/var/tmp/velopack/RemnifoldApp` 必须属于你，且其他用户不可写）。若要关闭此功能，请在应用的状态文件夹（`%APPDATA%\Remnifold`、`~/Library/Application Support/Remnifold`、Linux 上的 `$XDG_STATE_HOME/remnifold`（默认为 `~/.local/state/remnifold`），或 `<REMNIFOLD_HOME>/remnifold`）中的 `app-update.json` 里设置 `"auto_update": false`；诊断信息会写入同一位置的 `app-update.log`。如果你已经在使用下面的严格安装方式，Setup 会把 Claude Code 和 Codex 的注册切换到自身，并使用相同的连接和记忆状态；严格安装的文件会被保留，因此不再需要时请自行删除 `%LOCALAPPDATA%\Programs\Remnifold`。如果 Setup 期间某个注册未能完成（例如客户端 CLI 响应缓慢），会在你下次登录时自动重试。请在 Windows 设置中卸载；Claude Code 和 Codex 的注册以及登录项会随之移除，而你的连接和记忆状态会被保留。之后若要回到严格安装方式，请用它自己的 `connect claude-code --apply` 和 `connect codex --apply` 重新注册。若只想移除注册而不卸载，请运行 `remnifold connect claude-code --remove --apply` 和 `remnifold connect codex --remove --apply`。

macOS 没有卸载程序。在把 `Remnifold.app` 移到废纸篓之前，请先移除它的注册和登录项（不带 `--apply` 时，该命令只会显示将要移除的内容）：

```sh
"/Applications/Remnifold.app/Contents/MacOS/remnifold" uninstall-setup --apply
```

你位于 `~/Library/Application Support/Remnifold` 的连接、配置和记忆状态会被保留；已下载的更新缓存位于 `~/Library/Caches/velopack/RemnifoldApp`。

Linux 同样没有卸载程序。在删除 AppImage 之前，请以相同方式运行它（路径按你实际保存的位置调整）：

```sh
~/Applications/RemnifoldApp-linux-x64.AppImage uninstall-setup --apply
```

你位于 `~/.config/remnifold` 和 `~/.local/state/remnifold` 的连接、配置和记忆状态会被保留；已下载的更新缓存 `/var/tmp/velopack/RemnifoldApp` 可以删除。

### 严格安装（高级）

严格安装已冻结在最终版本 1.0.32。更新的版本只通过上面的 Setup 安装程序（Windows、macOS、Linux x64 和 ARM64）发布，因此 `remnifold install` 只能安装到 1.0.32 为止；已安装的版本和 `--verify-only` 仍可正常使用。要升级到更新的版本，请运行 Setup 安装程序，它会接管 Claude Code 和 Codex 中的注册。

已发布的原生安装包由 [Remnifold updates](https://remnifold-updates.kagayoi.com) 提供。版本索引为 `releases/<version>/index.json`；其清单会为下列每个目标选择一个以哈希命名的归档。在运行下载的可执行文件之前，请通过 GitHub 认证发布来源；仅凭 R2 URL 或提交状态链接并不能确立其来源。

| 平台 | Rust 目标 |
| --- | --- |
| Windows x64 | `x86_64-pc-windows-msvc` |
| Mac Apple Silicon | `aarch64-apple-darwin` |
| Linux x64 | `x86_64-unknown-linux-gnu` |
| Linux ARM64 | `aarch64-unknown-linux-gnu` |

不支持 Mac Intel。

应用按用户安装，无需管理员权限：

| 平台 | 应用文件夹 | 固定的可执行文件 |
| --- | --- | --- |
| Windows | `%LOCALAPPDATA%\Programs\Remnifold` | `bin\remnifold.exe` |
| macOS | `~/Library/Application Support/Remnifold/app` | `bin/remnifold` |
| Linux | `$XDG_DATA_HOME/remnifold`（默认为 `~/.local/share/remnifold`） | `bin/remnifold` |

`remnifold install` 会在写入任何内容之前，验证发布提交、CI 发布者日志是否成功、R2 清单、归档的大小与摘要以及安装包来源。它把每个版本只解包一次到 `versions/<version>`（已安装的版本绝不会被不同的字节覆盖），在 `receipts/` 中保留一份回执以便日后离线验证，并替换固定的可执行文件。注册始终指向固定的可执行文件，因此更新无需重新注册。在 Windows 上，正在运行的 MCP 服务器会一直使用旧的可执行文件，直到重启；此时结果会报告 `"restartRequired": true`。

### 首次安装

在 Windows 上，发布工作流会把已签名的 bootstrap 可执行文件放到 R2 中带版本号的不可变键下。它从 1.0.30 版开始提供；更早的版本请从可信源码构建第一个可执行文件。由于 `install` 只接受严格安装的版本，请将版本设为 1.0.30 到 1.0.32 之间的某个版本。选择一个已存在的可信私有目录，确保其他用户无法在验证与执行之间替换下载的文件。替换下面带引号的版本和路径，然后在 PowerShell 中运行：

```powershell
$version = '<published-version>'
$bootstrap = 'C:\absolute\trusted\private\path\remnifold-bootstrap.exe'
Invoke-WebRequest -MaximumRedirection 0 -Uri "https://remnifold-updates.kagayoi.com/velopack/remnifold-bootstrap-$version-x86_64-pc-windows-msvc.exe" -OutFile $bootstrap
$signature = Get-AuthenticodeSignature -LiteralPath $bootstrap
if ($signature.Status -eq 'Valid' -and $signature.SignerCertificate.Subject -like 'CN=Open Source Developer Yuichiro Shinozaki*' -and $signature.TimeStamperCertificate) { & $bootstrap install --version $version --apply } else { Write-Error 'Signature verification failed. Do not run this file.' }
```

下载不得被重定向，并且文件在执行之前必须通过 Authenticode 检查（状态为 `Valid`，签名者为 `CN=Open Source Developer Yuichiro Shinozaki`，带有 RFC 3161 时间戳）；检查失败则不要运行。该签名只能证明是谁签名，并不绑定产品、版本或发布是否被接受。`install` 仍需要已认证的原生 GitHub CLI，并自行验证发布（参见 [DESIGN.md](DESIGN.md#release-installation)）。不要用未经验证的 R2 辅助程序代替，也不要绕过失败的验证。

在 macOS 和 Linux 上，请获取并审阅来自可信来源的 [Bash 引导脚本](scripts/bootstrap-plugin.sh)，并将其保存在工作目录之外。它需要原生 GitHub CLI、curl、tar，以及 `sha256sum` 或 `shasum`：

```sh
bash ./bootstrap-plugin.sh --version '<published-version>' --apply
```

不带 `--apply` 时，它只验证发布；`--download-only` 会返回已验证的归档而不运行它。

### 更新、验证与注册

```sh
remnifold install --version <published-version>                 # 仅验证发布
remnifold install --version <published-version> --apply         # 安装或更新
remnifold install --version <published-version> --verify-only   # 离线重新检查已安装的版本
remnifold connect claude-code                                    # 显示计划中的注册内容
remnifold connect claude-code --apply                            # 注册 MCP 服务器和钩子
remnifold connect claude-code --remove --apply                   # 仅移除 Remnifold 的 MCP 条目和钩子
```

请使用应用文件夹中固定的可执行文件（或把它的 `bin` 文件夹加入 `PATH`）。`install` 在 Windows 上使用 `%LOCALAPPDATA%\remnifold` 下的工作文件夹，在其他系统上使用 `~/.cache/remnifold`；`--work-root <absolute-path>` 可选择应用、配置和仓库文件夹之外的另一个文件夹。当见证日志过期后，全新安装会以失败告终（fail closed）；保留的回执仍然支持 `--verify-only`。仍然需要通过 GitHub 访问 release 的源元数据。

`connect claude-code --apply` 使用 `claude mcp add --scope user remnifold -- <fixed executable> mcp` 注册 MCP 服务器（当 Claude Code CLI 不在 `PATH` 上时，则打印该命令；可传入 `--cli <path>` 来指定它），并把 `SessionStart`、`UserPromptSubmit`、异步的 `Stop` 以及 `PreToolUse` 钩子合并到 Claude Code 的 `settings.json`（`$CLAUDE_CONFIG_DIR` 或 `~/.claude`）中；`recallMode` 为 `off` 时不注册 `SessionStart` 和 `UserPromptSubmit`，并移除 Remnifold 已有的这两项。只会替换已有的 Remnifold 钩子条目；首次更改会保留 `settings.json.remnifold-backup`。格式错误的设置文件保持不变。`connect <tool> --remove`（先预览；`--apply` 才会写入）只移除 Remnifold 自己的 MCP 条目和钩子，保留其他所有设置；如果带 `--executable <path>`，则只移除指向该可执行文件的条目。`connect codex --apply` 使用 `codex mcp add` 注册 MCP 服务器；Codex 不提供在插件之外注册钩子的受支持方式，因此自动回忆和捕获通过 Claude Code 运行。注册后请开始一个新会话。

`Stop` 捕获既读取 Claude Code 的对话记录，也读取 Codex 的 rollout。它只保存每条用户请求和最终的助手文本，跳过思考内容、工具调用与结果、请求副本以及其他会话的行。后台通知之后的回复会作为单独的延续轮次保存，这样已保存的轮次可以保留各自的捕获键。Claude Code 的记录带有 `sm_source: "claude-code"`。为避免出现两套记忆系统，请用 `"autoMemoryEnabled": false` 关闭 Claude Code 内置的自动记忆；Remnifold 不会读取或导入那些文件。在本仓库内部，源码中的 `.mcp.json`（`cargo run`）优先于用户级注册；若要改用已安装的应用，请在未被跟踪的 `.claude/settings.local.json` 中添加 `"disabledMcpjsonServers": ["remnifold"]`。在不改变已保存轮次的前提下，它还会把三类会话事件作为单独的记录保存：AI 生成的压缩摘要、你在 AI 工作期间发送的消息，以及你对 AI 提问（AskUserQuestion）的回答。它们只在 `Stop` 时保存，因此压缩刚结束时只能检索到上一次 `Stop` 为止的记录。

此前的 Codex 插件命令 `plugin-update` 和 `plugin-bootstrap` 已被淘汰，它们会返回 `USE_REMNIFOLD_INSTALL`。请改为安装独立应用；之后即可从 Codex 中移除旧插件。

### 连接到你的服务器

```sh
remnifold setup --base-url https://your-worker.example.workers.dev
```

如果尚未提供 API 密钥，请在隐藏的提示处输入。Setup 会在写入连接之前验证一次已认证的会话。若要更新现有连接，请使用 `--force`；无关的设置会被保留。`REMNIFOLD_API_URL` 和 `REMNIFOLD_API_KEY` 可以提供进程级的覆盖值。对于非交互式设置，`--api-key-stdin` 会从标准输入读取密钥。

连接和本地状态保存在应用自己的配置文件夹中，独立于 Codex 和 Claude Code：

| 平台 | 连接（`remnifold.json`）与服务器状态 | 捕获与回忆状态 |
| --- | --- | --- |
| Windows | `%APPDATA%\Remnifold` | 同一文件夹 |
| macOS | `~/Library/Application Support/Remnifold` | 同一文件夹 |
| Linux | `$XDG_CONFIG_HOME/remnifold`（默认为 `~/.config/remnifold`） | `$XDG_STATE_HOME/remnifold`（默认为 `~/.local/state/remnifold`） |

`REMNIFOLD_HOME` 会同时覆盖两者（连接位于 `<REMNIFOLD_HOME>/remnifold.json`，状态位于 `<REMNIFOLD_HOME>/remnifold/` 下，应用位于 `<REMNIFOLD_HOME>/app`）；`setup --config-dir <directory>` 会把连接写到别处。早期版本把这些文件保存在 Codex 主目录中（`CODEX_HOME`，默认为 `~/.codex`）。当新文件夹中没有连接时，首次运行会把有效的旧连接复制过去，并读取早期的捕获与回忆状态，新状态只会写入新文件夹。旧文件绝不会被删除；`server-setup` 只有在带 `--apply` 时才会移动其旧状态。

运行 `whoAmI` MCP 工具以验证连通性和空间发现。

若要导入现有的 Codex 会话，请先用 `remnifold import --limit 10` 预览一个有限的批次。当你想上传经过清理的记录时，再加上 `--apply`。预览不会写入远程记忆；重复导入会复用确定性的捕获标识。`--codex-home` 用于选择读取 Codex 会话的位置。

## 创建或更新服务器

安装包中包含 Worker 打包文件和迁移。通过进程环境变量 `CLOUDFLARE_API_TOKEN` 或 `--token-stdin` 提供 Cloudflare API 令牌，然后预览现有资源：

```sh
remnifold server-setup --account-id <Cloudflare-account-id>
```

默认是经过认证的只读发现。该令牌需要账户读取、Workers 脚本、D1 和 Vectorize 权限；更改域名或路由还需要相应的 Zone、Routes 或 Custom Domain 权限。Setup 不会导入 Wrangler 的 OAuth 凭据。请妥善保管令牌和记忆 API 密钥。

对于已有的安装，请传入 `--config <absolute-existing-jsonc-path>` 来读取其配置而不覆盖它。Setup 按 `remnifold`、`kagayoi-memory`、`cloudflare-supermemory` 的顺序复用资源名称；显式指定的 `--worker-name`、`--database-name` 和 `--vector-index-name` 优先。请确认计划选中了现有的 Worker、D1 数据库和 Vectorize 索引。配置中的 ID 以及向量维度/度量必须一致。现有的绑定、cron 计划和路由会被保留；出现冲突时 Setup 会停止。升级时不要创建替代服务器，也不要复制记忆。

当你打算进行创建或部署时，请通过 `REMNIFOLD_API_KEY` 提供所选的记忆密钥，并应用该计划：

```sh
remnifold server-setup --account-id <Cloudflare-account-id> --apply --location apac
```

请使用与预览相同的配置和名称。`--apply` 会创建缺失的资源，把提供的密钥安装为 Worker 机密，应用增量式的 D1 迁移，部署内置的 Worker，并检查端点。对于已有的服务器，请提供其现有密钥，以免意外替换它。如果没有提供密钥，Setup 会保留原有机密，并报告已跳过认证验证；仅凭健康检查并不能验证迁移和 MCP 访问。提供密钥后，会启用会话验证、远程 MCP 初始化，以及对 `search_memory`、`getDocument` 和 `add_memory` 的发现。

请先应用服务器更新和迁移，再连接或更新客户端。Setup 会把可恢复的配置存放在仓库之外、应用配置文件夹中的 `server/config.json`（参见[连接到你的服务器](#连接到你的服务器)）；`--state <absolute-json-path>` 可选择另一个外部位置。请保留此状态以便后续更新，绝不要提交生成的 ID 或凭据，并在修正失败的步骤后重新运行同一命令。对于已有的自定义端点，请使用 `--base-url`；使用 `--help` 查看所有选项。Cloudflare 的用量由托管服务器的账户承担。

对于 1.0.22，请先使用该版本的 `server-setup --apply` 更新服务器，再更新客户端。迁移 `0013_memory_created_index.sql` 为按日期范围整理添加了一个索引；它不会更改任何已存储的记录或 ID。

对于 1.0.21，请先使用该版本的 `server-setup --apply` 更新服务器，再更新客户端。迁移 `0010_capture_variants.sql`、`0011_consolidation_recall_opt_out.sql` 和 `0012_organization_retry_fairness.sql` 会添加经过验证的捕获变体，使包含被排除出回忆的来源的历史检查点失效，并把今后的旧式推迟缩短为一小时。原始记录和 ID 会被保留；受影响的检查点可以从符合条件的来源重新生成。

## 记忆行为

- 已完成的用户请求和最终回答会保存在共享的 `memories` 集合中，并附带来源文件夹信息。附加的技能定义以及浏览器/页面状态信封不会进入保存的请求文本；最终回答则完整保留。确定性的捕获标识使重试复用同一条已存储的记录，而不是创建重复项。你对 AI 提问输入的回答和轮次中途发送的消息也会被保存。电子邮件地址和电话号码会被遮盖；对询问个人信息（姓名、地址、电话、邮箱、银行卡、密码等）的问题所作的自由输入回答会被替换。对其他问题的回答或中途消息（不做联系方式遮盖）中输入的个人信息仍会被保存，压缩摘要也可能重述你说过的内容。压缩摘要是 AI 写的线索，不是授权依据，不会被自动回忆（`recallable: false`），但手动检索可以找到。打码的只有位数结构合理的电话号码和邮箱（不含 `git@host`），无分隔符的国内号码等会保留；对问题的用词判断可能因措辞而漏掉，选项式回答会保留。已忘记的事件记录在重新发送时（例如收据丢失后）可能重新出现。
- 使用 `listTopics` 查找内容类别，然后把 `topic` 传给 `listDocuments` 或 `listMemories`。列表同时包含旧版文件夹空间和共享集合中的记录。使用 `topic: "__unclassified__"` 可查看没有当前主题标签的记录，包括分类失败、已禁用或被有意跳过的记录。
- 手动的 `add_memory` 会保存到共享集合。若要在阅读后忘记某条记录，请调用 `add_memory`，并传入 `action: "forget"`、它的 `documentId` 以及来源 `containerTag`；为保持兼容，仍然支持使用精确的已存储内容。使用 Codex 插件时，由 MCP 客户端提供的绝对路径 `sourceFolder` 或单个工作区根目录会补充来源信息；远程 MCP 工具则接受显式的 `projectId`。显式的 `containerTag` 会覆盖存储位置。插件绝不会把自己的安装文件夹当作用户的项目。`listSpaces` 为兼容和诊断而公开物理空间。
- 提示词回忆会按主题跨空间搜索。较短的后续提问可以从同一会话中更早的用户消息里恢复主题，即使中间隔着大量工具输出。历史恢复是有界的；没有具体主题时，不会执行主题搜索。自动回忆会为每个捕获的轮次和部分选择最新可用的修订版本，而手动搜索则保留历史修订版本。全局搜索会包含超出旧版发现上限的空间；旧版服务器使用上限为 100 个空间的兼容路径，并在发现不完整时给出报告。部分搜索会报告不可用的空间和文档索引，同时保留成功的结果。初始的会话事件只会重置回忆状态。默认情况下（`recallScope` 为 `session`），自动回忆只注入当前会话的记录：它按主题搜索，并在应用结果数量上限之前，只保留已存储的 `sessionId` 与钩子的 `session_id` 相同的条目；没有会话 ID 的记录或没有会话 ID 的提示词都不会注入。它不会请求其他会话的近期任务，因此 `maxRecentTasks` 不起作用。要查看其他会话，请使用手动的 `search_memory` 工具；传入 `sessionId` 可限定到某个会话。`"current"` 表示当前会话，仅在 Claude Code 中可用（由其 `PreToolUse` 钩子解析，但仅当直接注册了 `mcp__remnifold__search_memory` 时；经由 Toolport 等网关时请明确指定会话 ID；不支持 Codex）。除非 `includeProfile` 为 true，结果不含容器列表。在 `remnifold.json` 中把 `recallScope` 设为 `all` 可恢复原有行为：跨所有会话的主题回忆，外加最多两个来自其他会话的最近已完成任务，带有来源项目、日期和文档 ID，并单独标注，以便提示过去的工作而不暗示相关性。在 `all` 模式下，`maxRecentTasks` 接受 0 到 3 的整数；设为 0 会禁用近期工作上下文。没有这种列表模式的旧版服务器会继续提供主题回忆。`recallScope` 的其他值按 `session` 处理。在 `remnifold.json` 中把 `recallMode` 设为 `off` 可完全停止自动回忆：无论 `recallScope` 如何，都不会注入索引，也不会发送回忆搜索，但每个轮次仍会被捕获，`search_memory` 等手动工具也继续可用。设为 `off` 时，`connect claude-code` 和 Setup 安装也不会在 Claude Code 的 `settings.json` 中注册 Remnifold 的 `SessionStart`、`UserPromptSubmit` 钩子，并会移除已有的这两项；用于保存的 `Stop` 钩子和用于读取授权的 `PreToolUse` 钩子会保留。更改 `recallMode` 后，Setup 安装会在下一次 `mcp` 启动、无参数启动或登录时重新同步这些钩子，Claude Code 从再下一个会话起生效；现有安装也按同样方式迁移：当 `recallMode` 为 `off` 时，首次更新到此版本或首次启动 `mcp` 时会移除回忆用的钩子。通过 strict `install` 方式（冻结于 1.0.32）注册的仍保留四个钩子，`off` 时回忆用钩子会立即返回；用当前版本的可执行文件运行 `connect claude-code --apply` 即可按同样规则重新注册。插件包自带的钩子是固定的，只会直接返回、不做任何事。默认值为 `off`，在把 `recallMode` 设为 `direct` 之前不会自动回忆；其他非空值也按 `direct` 处理。 记录除会话 ID 外还会按客户端类型（取自记录的 `sm_source`，即 Codex 或 Claude Code）匹配，因此不会注入另一个客户端的相同 ID；能识别当前客户端时，未保存客户端类型的记录也不会被注入。 对于 Claude Code，恢复（resume）时复制进对话记录的旧会话 ID（从记录文件开头读取，连同当前 ID 最多 8 个）也视为同一会话，因此恢复之前的记录同样会被注入，但仅限原始轮次时间不晚于该 ID 最后一条被复制行的记录；原会话在分叉之后或并行保存的记录绝不会被注入，没有时间的记录也会被排除。Codex 仍只匹配当前 ID。 服务器迁移 `0015_memory_search_projection_session.sql` 会添加一个索引，使该会话过滤保持高速；未应用时返回的记录相同，只是更慢。
- 手动搜索同样会匹配已存储的项目名称。仅按项目名称搜索时，优先返回匹配的来源项目和较新的来源记录；项目与主题组合搜索仍然优先保证主题覆盖。
- 自动上下文包含标题、描述、章节名称、时间戳和文档 ID。一小段匹配的原文片段可以暴露索引中没有的细节；它仍然是历史的源文本，复用之前需要核实。Codex 会在需要时用 `getDocument` 打开完整记录。
- 注入的 `<remnifold-index>` 块、搜索结果和回忆通知中的固定措辞，遵循写入它们的环境所使用的语言：依次为 `REMNIFOLD_LANGUAGE`，然后 `LC_ALL`、`LC_MESSAGES` 和 `LANG`，再然后是操作系统的显示语言（Windows 用户界面语言、macOS 的 `AppleLanguages`）。日语环境使用日语措辞，其他环境使用英语。已存储的标题、描述和摘录按保存时的样子显示，不会翻译。
- 主题回忆默认最多返回三条记录。在 `remnifold.json` 中把 `maxMemories` 设为 1 到 5 的整数，可以更改自动回忆和手动搜索的上限；更新连接时，Setup 会保留已有的值。自动上下文还限制在 3,000 个 UTF-16 代码单元以内，因此实际可容纳的记录可能更少。
- 自动上下文会把近期任务与主题索引分开跟踪，并且在会话和对话记录信息可用时，于后续提示词中省略未变化的条目。已更新的记录可以再次出现，而压缩或清空上下文会重置重复抑制。本地回忆状态保存在所选 Codex 主目录下的 `remnifold/recall-state/` 中，不复制对话文本；状态不可用时，正常回忆仍保持启用。查询匹配与状态的细节见 [DESIGN.md](https://github.com/1llum1n4t1s/remnifold/blob/main/DESIGN.md#recall)。
- 服务器每天 03:00（JST）检查各个项目。当一个项目至少有 20 条未整合的记录，或者存在未整合的记录且距离上一次成功的检查点已过去三天（在第一个检查点之前，则从最旧的待处理记录算起）时，它会请求 Workers AI 创建一个整合检查点。在一个项目第一次到期运行时，服务器会通过连续的、有界的 AI 批次自动处理完整的现有积压，因此旧记录不需要手动回填命令。原始记录会被保留，并由检查点链接。较旧的检查点会被取代，而更改或忘记某个来源会使依赖它的检查点失效。
- 使用 `consolidate_memory` MCP 工具，可以立即把一个项目的检查点推进一个批次，而无需等待每日检查。远程 MCP 需要 `projectId`，即已存储的来源 ID。Codex 插件可以在恰好只有一个工作区根目录时推断当前项目，也可以显式接受 `sourceFolder` 或 `projectId`。每次调用最多处理 40 条新记录；如果仍有较大的积压，请重复调用。手动执行仍然要求至少有一条未整合的来源记录。
- 在主题相关性相同的情况下，自动回忆优先选择当前的检查点，并避免重复已被所选检查点涵盖的原始记录。检查点中没有的具体匹配细节仍然会暴露其原始记录。手动搜索继续暴露原始记录。
- 检查点的内容和元数据不能直接编辑：更新会返回 HTTP 409。请编辑原始记录并重新生成检查点。重新补充处理（re-enrichment）和删除仍然可用。
- 捕获会对已配置的记忆 API 密钥和常见的机密模式进行脱敏，并重试未被确认的记录。本地捕获状态保存在应用状态文件夹下的 `capture-state/` 中；早期版本保存在 Codex 主目录中的回执仍然可以读取。

捕获失败会带有固定的 `stage`、`cause` 和 `action` 代码，不会打印异常文本或凭据。`action=repair-required` 的结果，例如 `stage=receipt; cause=invalid`，需要修复相应的本地捕获状态；仅靠重试会一直失败。`action=retry-or-repair` 会保留正常的重试行为，同时允许调查持续存在的连接或响应问题。若要单独检查 API 的可达性，请在解压出的插件文件夹中运行：

```sh
remnifold status
```

主题是在现有的 Workers AI 补充处理（enrichment）步骤中，根据记录内容推导出来的。分类是异步的。没有实质结果的简短确认以及类似的低信息量记录，仍可用于手动搜索，但会被排除在自动回忆和补充处理之外；显式的主题标签仍会保留。文件夹名称作为来源元数据保留。现有记录保持其原始的文档 ID 和存储位置，并出现在同一个主题浏览器中。

请先更新服务器并应用其迁移，再安装使用主题浏览或整合功能的客户端。若要对旧记录分类，请使用平台可执行文件并沿用现有的记忆连接，预览一个有限的批次：

```sh
remnifold topics-setup --limit 10
```

默认批次为 10；`--limit` 接受 1–50。当确实要进行远程补充处理时，加上 `--apply` 把这一有限批次加入队列。这会使用现有的 Workers AI 计费，刷新派生的 AI/向量/事实补充数据，并使受影响的项目检查点失效，同时保留各项退出设置（opt-out）和修订保护。结果报告的是已接受的作业，而不是已完成的分类。请等待处理结束后再选择下一批；用 `getDocument` 和 `listTopics` 检查来验证结果。

API 密钥授予对一个服务器安装中所有记录的访问权限。对于不同的信任边界，请使用各自独立的安装。脱敏能降低意外捕获机密的风险；请自行审查你选择存入自己后端的内容。

## 记忆整理

安装应用并应用[当前的服务器迁移](#创建或更新服务器)之后（包括 `0009_memory_organization.sql` 中的整理结构，以及 `0013_memory_created_index.sql` 中的范围整理索引），可以通过 [organize-memory 技能](skills/organize-memory/SKILL.md)按需整理记忆，例如 `$organize-memory 7`，或者“整理最近 7 天的记忆”。天数（1–3650）按服务器时间选取最近 N 天内创建的记忆；运行会重复处理批次，直到该范围内没有到期的工作，或达到可选的批次上限（默认 3，最多 20）。不带天数时，会从全局队列中取一个批次。整理不再注册为 Codex 的计划任务；[setup-remnifold](skills/setup-remnifold/SKILL.md) 只会暂停早期版本遗留的任务。

每次运行都使用当前已安装的可执行文件。一个批次最多处理二十条目标记录和 48,000 个 UTF-16 源单元，包括部分处理或推迟的工作。这些并不是已完成记录的数量，也不是读取量或令牌总量的保证：候选摘要和有针对性的证据读取是额外的。每个批次的查找（lookup）最多两次调用。未解决的工作会被推迟，而不是扩展为大规模探索；因达到上限而停止的范围运行，直接再运行一次即可。

直接使用 CLI 时，运行已安装的可执行文件：

```text
remnifold memory-organize prepare [--days N]
remnifold memory-organize lookup --line
remnifold memory-organize commit --line
remnifold memory-organize sources <summary-document-id> [cursor]
```

这些命令使用现有的所有者 API 密钥连接；远程 MCP 的 OAuth 凭据不能授权整理。它们既不会调度 Codex，也不会部署或迁移服务器。`prepare` 会创建一个三十分钟的租约；`--days N` 把范围限制在最近 N 天内创建的记忆，且不会移动全局发现队列；每个范围响应都会返回 `range.cursor`/`range.exhausted`，下一次调用把非空的游标作为 `--range-cursor <cursor>` 传入，而旧版服务器会拒绝它，而不是回退处理。它会返回来源 ID/修订版本/分块、延续备注和候选摘要；它不是只读的连通性检查。`lookup` 从标准输入读取 `{"query":"a concise topic"}`。`commit` 读取一个包含 `batchId`、`summaries` 和 `coverage` 的 JSON 对象。请遵循执行技能中确切的字段限制、分区规则和多部分 `groupProgress` 流程。只有当每个分块和重要主题都已体现，并保留了结论、原因、证据和不确定性时，才声明 `complete`；做不到时，请使用部分延续或推迟。这项判断由 Codex 做出。请检查提交回执，并区分已发布的摘要和未完成的进度。

在执行 `prepare` 之前，先确立一条安全的标准输入路径，以及保留并分页查看其完整响应的方法；在 Windows 上，获取租约之前请先按执行技能中的本地合成 PTY 探测操作。请从同一份保留下来的响应中读取每个来源分块、延续备忘和摘要候选。显示被截断时，应从那次调用中恢复，而不是再准备一个批次。提交之前，在内存中验证确切的 JSON：每个 coverage 条目都需要它已准备好的 ID/修订版本，以及显式的 `summaries` 数组，对于部分处理或推迟的条目，要写成 `summaries:[]`。关于分页、预检以及中断运行后的恢复，请遵循[执行技能](skills/organize-memory/SKILL.md#one-batch)。

JSON 标准输入有三十秒的期限，以及包含转义在内的 128,000 字节 UTF-8 上限。`--line` 要求一行紧凑的 JSON 及其结尾换行符；不带它时，在对象之后关闭标准输入。请使用不回显的输入通道，并避免把生成的文本放进 shell 参数和命令日志。不受支持的服务器会明确停止，不会退而使用保存/忘记的替代方案。

对于 Windows 的 PTY 输入，请在专用的 PTY 中直接启动原生可执行文件，并使用 `memory-organize lookup --line --private-stdin` 或 `memory-organize commit --line --private-stdin`。等待标准错误上出现 `REMNIFOLD_STDIN_READY` 之后，再恰好发送一行紧凑的 JSON 及其最后的换行符，其后不得再有任何字节。载荷加换行符应不超过 127,000 个 UTF-8 字节，为终端的转换预留空间。私有模式会禁用回显和行编辑，并在终止符之后恢复原来的控制台模式。如果没有出现就绪信号，或任一步骤失败，请停止发送输入并关闭该 PTY；不要复用它，也不要在输入可能仍有残留时恢复回显。既不要使用共享控制台，也不要使用 Git Bash 包装器。重定向的标准输入和非 Windows 主机会拒绝 `--private-stdin`；在 macOS/Linux 上，请使用不回显的管道，并以行结束或 EOF 结束输入。完整的输入流程见[执行技能](skills/organize-memory/SKILL.md#safe-stdin-and-failures)。

不会删除任何原始记录。已被完整体现的原始记录，只有在其摘要仍然有效期间，才会离开常规搜索和主题回忆；原始记录的 ID 和正文仍可直接获取，而近期任务感知保持其现有的资格。除非服务器验证出不同的单部分 v2 捕获键和精确的正文哈希，并将其视为独立证据，否则相互冲突的捕获会保持可见并被推迟。这种验证并不能确定哪个版本最新；未解决的矛盾必须保留在摘要中。普通的推迟在一小时后重新具备资格，而未解决的捕获冲突会继续阻止自动重试。有界的队列在较早的到期工作、延续和未处理的记录之间共享进度；一次运行并不能确立整个存档已经完成。更改或忘记会使依赖的摘要失效，并让仍然存在的原始记录重新可见。较早的项目检查点在作为整条记录被处理之前，仍可能暴露信息。启用 AI 时，已经支撑语义搜索的原始记录会等到摘要的嵌入/向量经过验证之后才会消失。禁用 AI 时，词法覆盖就足够了；启用 AI 后，在语义替代物就绪之前，原始记录可能会再次出现。向量恢复是异步的，十五分钟一次的 cron 并不是完成的截止时间。摘要嵌入使用现有的 Workers AI 计费；正文由 Codex 生成。

`sources` 是只读的，最多分页返回五十个已记录的来源 ID、空间和日期。请用 getDocument 获取正文；沿着检查点自身的来源预览/游标可以到达其已记录的成员。不同的来源路径可能重复出现同一个 ID。当前的原生和远程 MCP getDocument 响应也包含来源预览和可选的 `sourceCursor` 分页；缺少这些字段的旧版原生插件可以使用 CLI。如果游标的父级发生变化，游标会以冲突停止。[整理设计](DESIGN.md#memory-organization)详细描述了覆盖、失效和向量恢复。

## 从 1.x 的名称升级

请移除已安装的 `kagayoi-memory`、1.x 的 `cloudflare-supermemory` 或 `remnifold` Codex 插件，并安装独立应用。客户端先在其配置文件夹中，再在原来的 Codex 主目录中，依次读取 `remnifold.json`、`kagayoi-memory.json` 和 `supermemory.json`。`REMNIFOLD_API_URL` 和 `REMNIFOLD_API_KEY` 是常规的环境变量；为兼容起见，仍保留对原有的 `KAGAYOI_MEMORY_*`、`SUPERMEMORY_API_URL`、`SUPERMEMORY_CODEX_API_KEY` 和 `CLOUDFLARE_MEMORY_API_KEY` 的读取。运行 `remnifold setup` 可写入常规的 `remnifold.json`，同时保留之前的文件。现有的捕获回执和回忆状态仍可从原来的产品文件夹读取。服务器 Setup 复用名为 `remnifold`、然后 `kagayoi-memory`、再然后 `cloudflare-supermemory` 的资源；显式指定的资源名称始终优先。更改本地应用不会部署 Worker，也不会迁移远程数据库。现有的文档 ID、捕获 ID、项目来源信息和持久化的 `sm_*` 元数据保持不变。存储兼容性的细节见 [DESIGN.md](https://github.com/1llum1n4t1s/remnifold/blob/main/DESIGN.md#storage-and-provenance)。

Remnifold 实现的是它自己的自托管 API。它独立于托管的 Supermemory 服务，也不会读取或修改 Codex 内置的本地记忆文件。

有关架构和仓库布局，请参阅仓库的 [DESIGN.md](https://github.com/1llum1n4t1s/remnifold/blob/main/DESIGN.md)。贡献者需遵守的约束、验证和打包命令见 [AGENTS.md](https://github.com/1llum1n4t1s/remnifold/blob/main/AGENTS.md)。

基于 [MIT 许可证](LICENSE)授权。
