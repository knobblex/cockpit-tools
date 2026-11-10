# Antigravity 上游工具

入口为 **设置 → 网络 → 本地工具 · Antigravity 上游服务**。HTTP/SSE 转发运行在 Cockpit 的 Rust 进程内；CLIProxyAPI 作为子进程负责反重力认证和上游协议转换。不再运行 Python，不依赖 Codex CLI 或 launchd。

## 使用

1. 准备已有授权账号的 CLIProxyAPI v8 状态目录及本机密钥。本工具复用现有授权，不另建账号登录系统。
2. 选择 CLIProxyAPI 可执行文件，填写原状态目录、端口和默认模型，选择是否“随 Cockpit 启动”，保存配置。
3. 如果显示“外部服务”，先停止原服务再启动。旧 Python 版本迁移时须先卸载它的 launchd 作业，防止自动重启抢占端口；工具不会按端口杀进程。

“刷新状态”通过本机密钥查询 `/v1/models`，不生成回复、不验证额度。默认模型必须在返回列表中。页面显示服务 PID、最近退出码、模型和日志路径；修改配置需先停止。停止操作需要确认，会中断在途请求，但保留账号和密钥。

## 生命周期与隐私

已保存配置默认随 Cockpit 启动，可取消该选项。正常完全退出 Cockpit 会关闭 Rust 监听器并结束、回收自己启动的 CLIProxyAPI；关闭窗口但驻留托盘时继续运行。手动停止取消当前运行周期的自动重试，不修改下次启动选项。后端异常退出后按 30 秒退避重试，不重放失败请求。操作系统强制杀死宿主无法执行正常退出清理，不保证回收子进程。

工具配置和生命周期日志保存在用户目录 `.cockpit_tools/local-tools/antigravity-upstream/`。旧配置中的 Python 路径字段在下次保存或准备启动时移除。账号、密钥保持在原目录；后端配置 `backend.generated.yaml` 由 Rust 生成，禁用远程管理、额外 credits、插件和请求日志。工具不修改用户的 Codex 配置。日志超过 1 MiB 时保留一份前次日志。

公开入口与内部后端均只监听回环地址。请求需本机密钥，健康检查和内部转发禁用代理及重定向，不记录请求正文。支持 HTTP Responses、Chat Completions 和 SSE，不支持 WebSocket 或 chunked 上传。模型别名由 [`config.rs`](../../../src-tauri/src/local_tools/config.rs) 统一定义：已配置的 Gemini、Claude 和 GPT OSS 对外名称省略强度后缀，由 CLIProxyAPI 映射到真实上游 ID。旧配置和旧名称请求按此表归一化，保留请求中的强度参数；未知模型名不截断。Gemini 和 Claude 请求将系统/开发者消息中的已知 Codex/GPT 身份句修正为各自实际模型家族，避免上游将身份冲突返回为 429；保留用户文字、工具定义和其余指令。Codex 模型目录关闭 WebSocket 偏好。错误状态、正文和 `Retry-After` 原样返回。

配置在各 Cockpit profile 间共享；公开端口和原状态目录的文件锁防止多个宿主同时启动。状态目录被占用时不会覆盖后端配置或停止其他进程。更换后端文件后需停止、再启动。

## 独立 Linux 服务

[`crates/antigravity-server`](../../../crates/antigravity-server/) 提供脱离桌面宿主的 `prepare` 和 `serve` 入口，直接复用上述 Rust 配置生成和 HTTP/SSE 转发实现。CLIProxyAPI 与转发层分别由 systemd 托管；仅 CLIProxyAPI 通过配置中的本地代理访问上游，内部转发仍禁用环境代理。dev 配置启用独立出站适配：默认 daily 上游使用本地 IPv4 解析，通过原代理连接；收到服务端 TLS 握手数据前连接失败时尝试其余解析地址。TLS 仍由 CLIProxyAPI 与 Google 端到端校验，不解密请求，不重放应用数据，不更改共享代理。目录暂时为空不关闭转发监听；重启转发层会联动重启后端，单独重启后端不会停止转发层。

[`deploy/`](../../../crates/antigravity-server/deploy/) 保存首次安装器、两个 unit 和 Nginx 路径模板。先构建 Linux 转发二进制，准备经过官方 SHA-256 校验的 CLIProxyAPI、一个可刷新的 Antigravity 账号 JSON，并与这些模板一起放入 root 私有暂存目录；账号文件命名为 `account.json`，两份可执行文件命名为 `cockpit-antigravity-server`、`cliproxyapi`，执行 `python3 install.py`。安装器针对已有 dev 主机和 Nginx 配置，拒绝覆盖已有部署；新增路径校验失败时恢复原 Nginx 并停用新增服务。

服务以专用用户运行，状态及独立访问密钥位于 `/var/lib/antigravity-upstream/`，配置位于 `/etc/antigravity-upstream/config.json`。后端配置由 `prepare` 生成，不手改模型映射。安装回执 `/etc/antigravity-upstream/deployment.json` 是实际入口、二进制摘要和 Nginx 备份位置的真源。监听器均使用回环地址，Nginx 只公开 `/antigravity/v1/`，关闭此路径访问日志和流式缓冲。账号不会在后续运行中与本机自动同步。

运行 `systemctl status antigravity-backend antigravity-relay` 查看状态；转发层日志用 `journalctl -u antigravity-relay` 查看。回退时停止并禁用这两个 unit，从安装回执指定的备份恢复 Nginx，执行 `nginx -t` 后 reload；保留私有账号目录以免丢失已刷新的授权。

## 更新边界与检查

前端实现位于本目录，Rust 实现在 [`src-tauri/src/local_tools/`](../../../src-tauri/src/local_tools/)。既有宿主文件只接入 `SettingsPageView.tsx` 的卡片及 `lib.rs` 的模块、命令与生命周期钩子。不改业务路由、主项目依赖清单、全局翻译文件或打包资源；上游重构时仍须核对这两个文件。本页是工具行为唯一说明，不复制模型清单或维护施工待办。

从仓库根目录运行：

```sh
cargo test --manifest-path tests/antigravity-upstream/Cargo.toml --locked
node node_modules/tsx/dist/cli.mjs --test src/local-tools/antigravity/AntigravityUpstreamTool.test.ts
cargo test --manifest-path crates/antigravity-server/Cargo.toml --locked
npm run typecheck
```

独立 Rust 测试使用随机回环端口和模拟 CLIProxyAPI 子进程，不使用真实账号或消耗模型额度；覆盖身份修正、流式首事件、取消、鉴权、错误保留、配置迁移和生命周期。桌面宿主以 macOS 为当前验证环境；独立服务已在 Linux x86_64 验证 HTTPS 鉴权、真实生成、Responses SSE、OAuth 刷新和后端重启恢复。其余桌面平台尚未集成验证。
