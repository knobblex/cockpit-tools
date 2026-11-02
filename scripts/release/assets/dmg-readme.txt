Cockpit Tools 安装与常见问题
================================================================

一、安装
把 Cockpit Tools 拖到 Applications，再从“应用程序”打开。

二、签名与安全提示
本 Fork 当前正式发布流程要求 Developer ID 签名和 Apple 公证。
普通本地构建、CI 测试包及旧版 Fork 包可能只有临时签名（ad-hoc）；
具体状态以该版本发布说明为准，历史安装包不会自动补签。
请确认安装包来源，并核对发布页的 SHA256SUMS.txt。

如果提示“应用已损坏”或无法验证开发者，先核验已安装的 App：

   codesign --verify --deep --strict "/Applications/Cockpit Tools.app"

核验失败时重新下载或重新构建，不要跳过错误。核验通过且确认来源后，
对于临时签名的测试或历史包，可尝试打开一次，再到“系统设置”→
“隐私与安全性”中为此 App 选择“仍要打开”。无需关闭 Gatekeeper。

正式签名包仍有问题时，请从对应发布页重新下载、核对 SHA256SUMS.txt，
并反馈版本、芯片架构和完整提示。
官方发布页：https://github.com/jlcodes99/cockpit-tools/releases
Fork 发布页：https://github.com/knobblex/cockpit-tools/releases


Cockpit Tools - install and troubleshooting
================================================================

1. Install
Drag Cockpit Tools to Applications, then open the installed app.

2. Signatures and security warnings
Current Fork releases require Developer ID signing and
Apple notarization. Local builds, CI test packages and older Fork releases may
use only an ad-hoc signature; check the release notes for that version.
Historical packages do not become signed retroactively. Confirm the source
and verify SHA256SUMS.txt from the release page.

If macOS reports damage or an unverified developer, verify the installed app:

   codesign --verify --deep --strict "/Applications/Cockpit Tools.app"

If verification fails, download or build a corrected installer. After verifying
the signature and source of an ad-hoc test or historical build, try opening it once,
then select Open Anyway in System Settings → Privacy & Security for this app.
Gatekeeper can remain enabled.

For problems with signed releases, download from the matching release page, verify
SHA256SUMS.txt, and report the version, architecture and complete message.
Official releases: https://github.com/jlcodes99/cockpit-tools/releases
Fork releases: https://github.com/knobblex/cockpit-tools/releases
