Cockpit Tools 安装与常见问题
================================================================

一、安装
把 Cockpit Tools 拖到 Applications，再从“应用程序”打开。

二、签名与安全提示
官方正式发布流程要求 Developer ID 签名和 Apple 公证。本地及 Fork
测试包默认使用完整临时签名（ad-hoc），没有 Developer ID 或公证。
请确认安装包来源，并核对随包校验和；历史安装包不会自动补签。

如果提示“应用已损坏”或无法验证开发者，先核验已安装的 App：

   codesign --verify --deep --strict "/Applications/Cockpit Tools.app"

核验失败时重新下载或重新构建，不要跳过错误。核验通过且确认来源后，
对于本地或 Fork 临时签名包，可尝试打开一次，再到“系统设置”→
“隐私与安全性”中为此 App 选择“仍要打开”。无需关闭 Gatekeeper。

官方签名包仍有问题时，请从官方发布页重新下载、核对 SHA256SUMS.txt，
并反馈版本、芯片架构和完整提示。
官方发布页：https://github.com/jlcodes99/cockpit-tools/releases
Fork 发布页：https://github.com/knobblex/cockpit-tools/releases


Cockpit Tools - install and troubleshooting
================================================================

1. Install
Drag Cockpit Tools to Applications, then open the installed app.

2. Signatures and security warnings
Official releases require Developer ID signing and Apple notarization.
Local and Fork test builds default to a complete ad-hoc signature without
Developer ID or notarization. Confirm the source and verify the supplied
checksum. Historical packages do not become signed retroactively.

If macOS reports damage or an unverified developer, verify the installed app:

   codesign --verify --deep --strict "/Applications/Cockpit Tools.app"

If verification fails, download or build a corrected installer. After verifying
the signature and source of an ad-hoc local or Fork build, try opening it once,
then select Open Anyway in System Settings → Privacy & Security for this app.
Gatekeeper can remain enabled.

For problems with official signed releases, download again, verify
SHA256SUMS.txt, and report the version, architecture and complete message.
Official releases: https://github.com/jlcodes99/cockpit-tools/releases
Fork releases: https://github.com/knobblex/cockpit-tools/releases
