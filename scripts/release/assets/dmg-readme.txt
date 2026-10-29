Cockpit Tools 安装与常见问题
================================================================

一、安装
把 Cockpit Tools 拖到窗口右侧的 Applications 文件夹，再从应用程序打开。

二、如果提示“应用已损坏，无法打开”
此提示可能来自文件损坏、签名无效或系统安全检查，不能据此直接断定原因。
先确认安装包来自你信任的项目构建，再在终端核验已安装的 App：

   codesign --verify --deep --strict "/Applications/Cockpit Tools.app"

如果签名核验失败，请重新下载修复后的安装包，不要跳过该错误。
如果安装位置不同，把 App 拖入终端以获得实际路径，替换命令中的路径。

三、签名核验通过，但 macOS 仍阻止打开
当前默认使用完整的临时签名（ad-hoc），尚未接入 Apple Developer ID 和公证。
先尝试打开一次，再到“系统设置”→“隐私与安全性”中为此 App 选择“仍要打开”。
核验通过且确认来源后，也可只清除此 App 的下载隔离标记：

   sudo xattr -rd com.apple.quarantine "/Applications/Cockpit Tools.app"

此操作不关闭系统的 Gatekeeper。升级后如果再次被拦截，应重新核验新版本。


Cockpit Tools - install and troubleshooting
================================================================

1. Install
Drag Cockpit Tools to Applications, then open the installed app.

2. If macOS says the app "is damaged and can't be opened"
This can indicate damaged files, an invalid signature, or a macOS security check.
Confirm the installer came from a project build you trust, then verify the installed app:

   codesign --verify --deep --strict "/Applications/Cockpit Tools.app"

If verification fails, download a corrected installer; do not bypass that failure.
For a different install location, drag the app into Terminal to obtain its actual path.

3. If verification passes but macOS still blocks the app
The default build uses a complete ad-hoc signature, without Apple Developer ID or notarization.
Try opening it once, then use System Settings → Privacy & Security → Open Anyway for this app.
After verifying the signature and source, you can also remove only this app's download quarantine:

   sudo xattr -rd com.apple.quarantine "/Applications/Cockpit Tools.app"

This leaves Gatekeeper enabled. Verify a new version again if an upgrade is blocked.
