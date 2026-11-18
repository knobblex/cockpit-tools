# Release Process

第 1–8 节描述上游 release workflow；本 Fork 的同步和发布见[第 9 节](#upstream-sync)。行为以对应 workflow 和 `scripts/release/` 下的脚本为准；如果两者与本文不一致，应先修正文档或 workflow，再发版。

## 共享开发工作区

- **固定入口**：本机开发、修复和发版统一使用 `/private/var/www/antigravity-cockpit-tools` 的本地 `main`。写入前用 `git rev-parse --show-toplevel` 和 `git branch --show-current` 核对。除非用户明确指定，否则不新建分支或工作树，也不在当前会话附带的其他工作区继续写代码。
- **现有改动**：如果固定目录还在别的分支，先保留现场。迁移时把原目录、其他工作树以及归档中尚未合入的改动去重收拢后，再统一到本地 `main`；不要为切换而自动 stash、重置或覆盖其他会话的改动。
- **并行写入**：不同会话可以并行改互不重叠的范围。同一文件、共享逻辑或中英文更新日志有并行改动时，先核对已有内容，再错开写入，保留他人改动。暂存和提交只选本次授权的范围。
- **开发交接**：已完成的用户可感知改动写进共用的中英文待发日志，不另建发布清单。还未完成或只是实验的改动，交接时标明对应文件和状态。例外使用其他工作区时，交接也注明改动所在的工作区、分支、提交或可恢复的快照。日常开发留在本地，不自动提交或推送。

## 1. 发布前检查

标准发版在仓库根目录执行：

```bash
npm run release:preflight
```

当前 preflight 依次执行：

1. `node scripts/check_locales.cjs`
2. `node scripts/check_modal_sizing.cjs --strict`
3. `npm run typecheck`
4. `npm run build`
5. `cargo check`（`src-tauri`）
6. `cargo test --lib`（`src-tauri`，`RUST_TEST_THREADS=1`）

排障时可以跳过单项：

```bash
node scripts/release/preflight.cjs \
  --skip-locales \
  --skip-typecheck \
  --skip-build \
  --skip-cargo \
  --skip-cargo-test
```

标准发布不得使用 `--skip-*`。明确指定「快速发布」时不运行预检脚本，也不在正式构建前另跑翻译、弹框布局、类型检查、`cargo check` 或测试。普通分支 CI 仍独立运行，结果不阻塞正式 tag 构建。

## 2. 版本与标签

`package.json.version` 是发布 workflow 读取的版本。创建发布标签前先执行：

```bash
npm run sync-version
```

然后确认版本同步后的文件与 changelog 已提交。发布标签必须严格匹配：

```text
v<package.json.version>
```

例如 `package.json.version` 为 `1.3.40` 时，标签必须是 `v1.3.40`。workflow 会在版本或标签不一致时直接失败。

## 3. GitHub Actions 发布目标

当前 release workflow 构建并上传：

- Windows x64（MSI、NSIS 和便携包）
- Windows ARM64（NSIS）
- macOS Apple Silicon (`aarch64`)
- macOS Intel (`x86_64`)
- macOS Universal
- Linux `x86_64`
- Linux `aarch64`

Linux release 同时包含 AppImage、deb 和 rpm，以及可在无桌面环境运行的 x86_64 和 aarch64 独立 CLI 压缩包。独立 CLI 计入发布校验和。macOS Universal DMG 还会用于后续 Homebrew Cask 更新。

macOS 正式构建和 `warm-macos-release-cache.yml` 共用独立的依赖缓存，固定在同一 Rust 版本，并分别按 ARM 和 Intel 编译目标保存。预热在 `main` 的构建配置或依赖变更时后台运行，也可在 `main` 手动启动；用 Tauri 的正式编译参数，只编译，不签名、打包或发布。正式 tag 仅读取缓存，不等待预热；缓存缺失时直接编译。普通 PR CI 的 `ci` profile 缓存保持独立；Windows/Linux 继续使用原有缓存。

新推送不会取消正在编译的预热任务。同一版的 `main` 和 tag 紧接着推时，正式构建可能在缓存写入前就已经开始。需要缩短打 tag 后的等待时，可在准备版本期间让 `main` 预热先完成；这不是发版门槛。下一次发布在 macOS ARM 和 Intel 的任务摘要看 `Exact match`，再对照 Rust 编译耗时；`false` 只表示没有精确匹配，是否恢复了部分缓存以步骤日志为准。

Universal 不再从源码重编两套宿主。ARM 和 Intel release job 在本轮构建后通过 `scripts/release/macos-universal.cjs --mode stage` 上传宿主薄二进制，附带版本、commit、workflow run ID 和 SHA-256。Universal job 使用同一 run 的具名 artifact；`--mode merge` 校验身份、文件哈希和实际架构，合并宿主并逐片比对原始内容，再用 `tauri bundle` 打包。缺失或不匹配直接失败，不回退到其他版本或旧产物。

正式 workflow 只生成一次前端 `dist`，各平台使用同一份 artifact；`tauri.prebuilt-frontend.conf.json` 只覆盖构建钩子，不替换正式 Tauri 配置或更新地址。

ARM 和 Intel 用 `tauri build --bundles app`、Universal 用 `tauri bundle --bundles app`，保留已签名公证的 `.app` 及 Tauri 更新包。随后三个 job 都直接用 `scripts/release/inject-dmg-readme.cjs` 从 `.app` 生成最终 DMG，不先制作另一份镜像。安装说明、Finder 布局、卷图标和文件命名保持不变；仍在安装包生成后签名并公证最终镜像。

CI matrix 中的 Universal 源码构建仍保留，用于独立兼容性验证；安装排查步骤的唯一来源是 [`dmg-readme.txt`](../scripts/release/assets/dmg-readme.txt)，README 链接该文件。

macOS 构建使用 `npm run tauri -- build ...` 或 `node scripts/tauri.cjs build ...`（正式 workflow 同样使用后者），在构建结束或失败时清理本轮新增的 Tauri `bundle/macos/rw.*.dmg` 临时挂载，不碰构建前已有的卷及最终 DMG。直接使用 `npx tauri build` 会绕过这层清理。

DMG 重建与验证共用 `scripts/release/dmg-mounts.cjs` 管理本次创建的挂载。正常完成、验证失败及可处理的 `SIGINT` / `SIGTERM` 退出都会尝试卸载；仅卸载本次挂载，不强制推出用户磁盘。卸载失败时打包必须失败，并保留镜像和临时目录供排查，禁止带着挂载删除临时文件。系统强制结束进程（如 `SIGKILL`）或断电无法保证执行清理。

本地测试包统一使用以下入口打包已构建的 `.app`，不需要另写临时挂载脚本：

```bash
node scripts/release/package-local-dmg.cjs \
  --app "target/release/bundle/macos/Cockpit Tools.app" \
  --output ".tmp/local-packages/Cockpit-Tools_<version>_arm64_test.dmg"
```

该入口保留原 App 签名，校验镜像、挂载后逐文件比较内容和符号链接，并验证 App 签名；全部通过且卸载完成后才替换输出文件并写入 `.sha256`。`--stage <directory>` 可替代 `--app`，用于复用已核验的完整暂存目录。脚本不会代替宿主构建、签名或 Apple 公证，也不会自动推出用户安装时另行打开的 DMG。

上游 Tauri release build 使用仓库配置的 updater signing secrets：

- `TAURI_SIGNING_PRIVATE_KEY`
- `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`

这两个密钥仅用于 Tauri updater artifact 签名，与 Apple Developer ID 签名不同，必须继续保留。Windows Authenticode 不在本次流程范围内。

### macOS Developer ID 与 Apple 公证

在 GitHub 仓库的 Settings → Secrets and variables → Actions 中配置以下 repository secrets。适用的凭据缺失时，对应发布任务失败，不降级为未签名包：

| Secret | 内容 | 适用任务 |
| --- | --- | --- |
| `APPLE_CERTIFICATE` | 包含私钥的 Developer ID Application `.p12` 文件的 Base64 | 上游、Fork |
| `APPLE_CERTIFICATE_PASSWORD` | `.p12` 导出密码 | 上游、Fork |
| `APPLE_SIGNING_IDENTITY` | 完整 `Developer ID Application: … (TEAMID)` 证书名 | 上游、Fork |
| `APPLE_TEAM_ID` | 与签名证书一致的 10 位团队 ID | 上游、Fork |
| `APPLE_ID` | 公证用 Apple 账号邮箱 | 上游 |
| `APPLE_PASSWORD` | Apple App 专用密码，不是账号登录密码 | 上游 |
| `APPLE_API_KEY` | App Store Connect API 私钥 `.p8` 文件的 Base64 | Fork |
| `APPLE_API_KEY_ID` | 该 API 私钥的 Key ID | Fork |
| `APPLE_API_ISSUER` | 该 API 私钥的 Issuer ID | Fork |

上游的 `macos-signing.cjs prepare` 在 runner 临时目录创建独立钥匙串、导入证书并验证公证凭据。构建步骤通过 Tauri 完成应用签名、公证和票据附加，开启 hardened runtime；不默认放宽 entitlement。自动更新包继续由 Tauri 生成与签名。

随后直接生成包含安装说明的 DMG，再由 `macos-signing.cjs finalize` 签名并提交最终 DMG 公证，等待上限 30 分钟。校验覆盖原应用、解压后的自动更新应用和最终 DMG 内的应用，要求正确 Developer ID 团队、hardened runtime、有效公证票据及 Gatekeeper 认可；最终 DMG 本身也需通过签名、票据和 Gatekeeper 检查。全部通过后才进入资产暂存、上传与校验和生成。Universal 合并后仍由 Tauri 重新签名，不能复用合并前的签名。

上游每个 macOS job 上限 120 分钟，结束时 `if: always()` 恢复钥匙串搜索列表并删除本轮临时钥匙串；导入后的 `.p12` 临时文件立即删除。Apple 返回拒绝、超时或凭据错误时停止发布，不移除隔离属性绕过校验。首次公证可能等待较久，超时应检查 Apple submission 状态后重试，不能当作已通过。

本流程不会补签已经发布的历史安装包。只有实际完成签名与公证的产物才能标注为已验证。证书、私钥、专用密码均不得提交到仓库或写入日志。

普通本地构建及 Fork Build Matrix 的中间产物使用 [`bundle.macOS`](../src-tauri/tauri.conf.json) 配置的完整临时签名（ad-hoc），上传前执行 `codesign --verify --deep --strict`。正式 Fork Release 还须完成[云端签名与公证](#upstream-sync)后才能公开；安装排查统一见[随包说明](../scripts/release/assets/dmg-readme.txt)。

## 4. Release assets 与 updater manifests

Windows、macOS Apple Silicon/Intel 和 Linux 构建 job 会先通过 `scripts/release/stage_release_assets.cjs` 规范化允许上传的 release assets，再用 `scripts/release/build_target_latest_json.cjs` 生成各 updater target 的 manifest。macOS Universal job 只规范化并上传 Universal DMG，不生成 updater manifest；该 DMG 供后续 Homebrew Cask 更新使用。

所有平台完成后，`finalize-legacy-latest` job 会下载 release assets，并用：

```bash
node scripts/release/build_merged_latest_json.cjs
```

生成完整 legacy `latest.json`。workflow 会检查其中至少包含：

- `darwin-aarch64`
- `darwin-x86_64`
- `windows-x86_64`
- `windows-x86_64-nsis`
- Linux x86_64 / aarch64 的 AppImage、deb、rpm targets

随后会通过 `scripts/release/verify_published_updater_manifests.cjs` 对公开 updater 状态做端到端验证。

## 5. SHA256SUMS

`finalize-legacy-latest` job 复用同一次下载的 release assets，在生成完整 `latest.json` 后计算 SHA-256，并一并上传两个文件：

```text
SHA256SUMS.txt
```

不要依赖文档中不存在的 `npm run release:checksums` 命令。当前 checksum 的权威实现位于 release workflow 本身；`scripts/release/gen_checksums.cjs` 是可单独调用的脚本，但不是 `package.json` 中的标准 npm script。

## 6. Homebrew Cask

release workflow 会下载已发布的 Universal DMG，计算 SHA-256，然后更新：

```text
Casks/cockpit-tools.rb
```

该更新通过自动创建的 PR 提交，而不是本地 `npm run release:github-and-cask`。当前 `package.json` 没有这个 npm script，因此不要按旧文档中的本地一键脚本操作。

## 7. 发版顺序

0. **确定范围并收齐**：未指定只发某项时，「发版」和「快速发版」都包含本项目所有已完成、尚未公开发布的改动，不限于当前会话。以最近一个仍公开的正式版为基线，对照本地 `main` 的提交、未提交改动和待发日志；如仍有分散开发的遗留改动，由发版会话核对对应分支、工作区和归档快照，只合入尚未包含的最终实现。只有同一功能存在冲突且无法从代码和历史判断取舍时，才询问具体冲突。
1. 更新 `package.json` 版本。
2. 按最近公开正式版到本次准备发布的实际代码差异，更新 `CHANGELOG.md` 与 `CHANGELOG.zh-CN.md`，不把未合入内容写成已发布。
3. 执行 `npm run sync-version`。标准发布按 `AGENTS.md` 执行本地检查；明确指定快速发布时跳过这些检查，不等待普通 CI 或缓存预热。
4. 在共享的本地 `main` 提交已收齐的改动。打 tag 前对照最终提交和待发日志，告知用户本次包含什么、哪些已知改动未纳入及原因。范围明确时无需再次审批。
5. 从核对过的 `main` 提交创建 `v<version>` 标签，先推送 `main`，再推送标签。
6. 分支和 tag 到达远端即报告「已触发构建」。只有正式 Release 和 `latest.json` 可用时才报告「用户可更新」。Homebrew Cask 在发布后异步更新。

```bash
npm run sync-version
npm run release:preflight
```

4. 提交并合并发布所需改动。
5. 从期望发布的 commit 创建 `v<version>` 标签，先推送分支，再推送标签。
6. 检查 GitHub Actions 的 release workflow 完整成功。
7. 检查 GitHub Release 的平台 assets、target manifests、`latest.json` 和 `SHA256SUMS.txt`。
8. 检查 Homebrew Cask 自动 PR 的版本和 SHA-256 是否与 Universal DMG 一致。

报告发布进度时应区分“分支与标签已推送”和“安装包已公开可下载”；后者还须核实构建、资产上传、更新清单和公共下载校验结果。

快速发布不强制清理本地 Rust/Tauri 编译产物。仅讨论发布、修改规则或验证代码不授权提交、推送或触发发布。

## 8. 草稿发布与公开校验

当前 workflow 先创建 draft Release。全部平台构建、完整 manifests、checksums 和草稿资产校验完成后，`publish-release` 才公开 Release 并设置为 latest，随后检查公共下载地址；公开发布后不再补齐缺失平台来绕过校验。

GitHub 公共 latest 路由可能晚于 Release 状态变更。如果公开校验仍返回旧版本，应核对 Release API、公共下载重定向与本轮清单内容，保留版本、签名和资产归属校验；确认传播完成后可单独重跑发布校验 job。

同版本撤回重发必须经用户明确授权，先核对并清理旧同版本 Release 和标签，再从新提交重建标签。发布后应额外比较公开清单与新 Release 资产的内容哈希，避免同版本旧缓存仅凭版本号通过检查。已安装同版本的用户需要重新下载安装，不会因同版本号重发而自动升级。

<a id="upstream-sync"></a>

## 9. Fork 上游同步与更新包

[`upstream-sync.yml`](../.github/workflows/upstream-sync.yml) 是同步入口及上游仓库、目标分支、定时周期的唯一配置源；仅在该文件指定的 Fork 默认分支运行。签名工作流及其控制脚本也须位于默认分支，GitHub 才会在发布入口完成后触发签名。同步任务定期查询上游最新正式 Release，忽略草稿和预发布版，不跟随尚未发版的 `main` 提交。定时任务按 GitHub 调度执行，可能延迟；公开仓库长期无活动时可能被停用，见 [GitHub schedule 规则](https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows#schedule)。

[`upstream_sync.cjs`](../scripts/release/upstream_sync.cjs) 校验上游版本标签、提交及其 `package.json.version`，并与本 Fork 的 `package.json.forkUpstream` 基线核对，拒绝上游降级或同版本替换。已合入的提交直接跳过；新版本先从当前定制分支创建 `sync/upstream-*` 候选，正常合并上游，保留本地修改。候选引用记录该上游提交与本地基线的尝试；候选准备或构建失败、取消时不启动晋升，已有尝试默认不重复打包。候选引用保留作排障证据，不用于正式发布。

合并成功后，以不可变候选 SHA 调用现有 [Build Matrix](../.github/workflows/build-matrix.yml)，执行前置检查、定制席位轮换回归和全部平台构建。候选代码只有只读仓库权限和只读缓存，不继承签名 Secrets。控制脚本始终来自本次运行的默认分支提交，不能由候选代码替换。所有构建成功后，再核对候选身份及定制分支基线，以原子比较方式仅快进更新；构建期间有其他提交则停止，不覆盖新工作。

晋升成功后调用 [`fork-publish.yml`](../.github/workflows/fork-publish.yml)，复用本次所有平台的构建产物，先创建绑定到已验证源码提交的 Release 草稿并上传中间资产。发布入口成功后，独立的 [`🍎 Fork Apple signing`](../.github/workflows/fork-apple-sign.yml) 工作流从默认分支运行可信控制脚本，不执行候选代码。它绑定对应的构建运行和源码，校验定制分支提交、更新地址和公钥，在 GitHub Actions 完成 Apple 签名、公证和产物验证，并使用 `FORK_UPDATER_PRIVATE_KEY` 签名和验证更新包及校验和清单。签名后的资产上传并验证后仍保留草稿，不自动公开。

发布版本的唯一来源是 `package.json.version`，标签为 `fork-v<version>`；上游来源单独记录在 `package.json.forkUpstream`。Fork 版本以在上游补丁号末尾追加 `1` 为基础，例如上游 `1.3.78` 对应 Fork `1.3.781`；补丁号按十进制规范化，不保留前导零。同一上游基线的后续修订可继续递增 Fork 补丁号。同步新上游时，[版本策略](../scripts/release/fork-version.cjs)取该映射结果与当前 Fork 补丁号加一中的较大者，确保客户端只升级到更高版本。首次发布或重试构建时，在默认分支 Actions → **📦 Fork updater release** 手动运行。它对定制分支当前提交重新执行 Build Matrix；分支变化、版本降级或任一平台失败均停止。已有草稿只能续传指向相同提交的资产，公开版本不会被覆盖。

Fork 发布任务需要配置 `FORK_UPDATER_PRIVATE_KEY` 和[表中标为 Fork 的 Apple 签名及公证凭据](#macos-developer-id-与-apple-公证)。Fork 公证使用 App Store Connect API 私钥；签名任务在 runner 的临时钥匙串中导入证书，并在结束时清理。证书私钥、公证凭据和更新私钥保存在 GitHub Actions Secrets 中，不得提交到仓库或写入日志。签名后的 ARM、Intel 和 Universal App 及最终 DMG 需完成 Developer ID 签名、公证、票据附加和 Gatekeeper 校验；更新包、来源记录、清单和校验和也须通过验证。

任务运行中或失败时，草稿可能包含尚未签名或仅部分替换的资产，不能公开。发布入口成功只表示中间草稿就绪；确认对应的 **🍎 Fork Apple signing** 工作流成功后，核对签名产物、Apple 公证记录、更新清单与校验和，再手动公开 Release。公开后还须核对公开下载和 updater 清单；签名工作流不会执行公开后的验证。

本 Fork 不发布 Homebrew Cask；更新包使用本 Fork 的独立公钥和 Release 地址，旧版上游 App 需先手动安装 Fork 包才能进入此通道。Tauri 更新签名与 Apple 签名、公证分别校验，Windows Authenticode 不在此流程范围内。

在 Actions 运行摘要查看同步及发布结果；构建中的 **Artifacts** 是临时测试包，名称与保留期以 Build Matrix 为准，公开的安装包与更新清单位于本 Fork 的 Releases。部分平台已有产物不代表整次构建或发布成功。GitHub 的定时调度可能延迟；自动更新仍取决于 App 内的更新设置。

失败后，先查看失败 job 和候选引用，修复冲突或权限问题；需要重试同一同步基线时，在默认分支 **Run workflow** 勾选 **retry**。若晋升推送中断，先核对远端分支是否已更新，不凭连接错误判定未成功。晋升成功但草稿构建失败时，手动运行 **📦 Fork updater release**。签名失败时，检查 Actions 日志及 Apple 公证状态：若尚未替换草稿资产，可在对应签名工作流中重跑；若已替换资产，重新运行发布入口恢复完整中间草稿，再等待新的签名工作流。公开后的校验按[第 8 节](#8-草稿发布与公开校验)所述核对版本、清单和公共下载。手工更新定制分支后会形成新的基线。

云端同步和草稿任务使用内置 `GITHUB_TOKEN`，不要求个人 PAT；若上游涉及工作流变更而 GitHub 拒绝推送，则停止并要求人工处理，不跳过这些变更或扩大权限。失败通知遵循 GitHub 账号的 Actions 通知设置，本流程不额外发送邮件或创建 Issue。

回归检查复用 `npm run test:release`，包含临时 Git 仓库中的合并、冲突、重试、版本校验与并发更新测试。它们不访问真实账号或修改远端分支。
