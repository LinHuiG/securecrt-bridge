SecureCRT Bridge

双击标准 NSIS 安装包，依次通过欢迎、安装目录、SecureCRT 配置、Skill 选择和安装页。
安装前请关闭 SecureCRT。已有版本显示升级／卸载选择页。
可自动检测路径，也可浏览选择程序目录、SecureCRT.exe 及包含 Sessions 的配置目录。
点击安装后显示进度和详细日志；失败原因以中文展示。
程序支持 Windows x64 / SecureCRT x64，保留原 SecureCRT 和齐治客户端。
安装后从齐治点击 SSH 或重新连接 SecureCRT，会话通过登录脚本自动接入。
窗口不需要保持前台。每个标签页有独立会话 ID，可分别操作多个 SSH 会话；
同一会话的 AI 写入串行处理，不同会话可以并行。
本地服务随 Windows 登录启动，也可由终端接入模块启动。托盘可暂停和退出。
退出服务后，当前标签页不会立即重启它；新建或重新连接 SSH 会再次启动。
退出与暂停分别处理。主动暂停会保存；普通退出不会让下次启动自动变成暂停。

本程序及接入模块由 Rust 编译，不需要安装 Python、Node 或其他运行库。
所有 API 仅绑定 127.0.0.1，并要求令牌。数据与备份保存在用户
AppData\Local\SecureCRTBridge；不要分享 connection.json。

在命令行使用：
  securecrt-session.exe SESSION_ID --owner CHAT_ID
  上述命令保留进程，首次批准后通过 stdin 发送 JSON 行，完成后发送 {"op":"close"}。
  例如 {"op":"exec","command":"hostname"}，再用 result 请求获取执行结果。
  在 Codex 用交互进程启动，后续复用 write_stdin；不要加 Out-String。
  以下为兼容的独立调用：
  securecrt-bridge.exe doctor
  securecrt-bridge.exe sessions
  securecrt-bridge.exe screen SESSION_ID
  securecrt-bridge.exe claim SESSION_ID --owner CHAT_ID
  securecrt-bridge.exe exec SESSION_ID --owner CHAT_ID --command "hostname; pwd"
  securecrt-bridge.exe release SESSION_ID --owner CHAT_ID
  securecrt-bridge.exe interrupt SESSION_ID --owner CHAT_ID --job ORIGINAL_REQUEST_ID

你和 AI 共用终端，请轮流操作。暂停仅取消未发送命令。
自己的未知任务停在续行提示符时，可用 interrupt 发送一次 Ctrl+C；
读取新的屏幕快照并确认普通提示符后，再 acknowledge 解锁。
恢复不会自动重跑原命令，也不会抢占尚在采集输出的任务。exec 只适用于 POSIX shell。

升级/修复：关闭 SecureCRT，重新运行安装包或执行 repair。
卸载：关闭 SecureCRT，从 Windows 已安装的应用卸载，或执行 uninstall --gui。
卸载只恢复仍保持本程序安装值的配置，保留用户后续修改和原始备份。
标准 Uninstall.exe 在临时目录执行清理，保留用户修改的文件和原始备份。

现有 ActiveX 登录脚本会先运行，然后接入桥接。SecureCRT 内置 Python 登录脚本
无法通过 ActiveX 引擎串联，该类会话保持原设置，并在安装结果中列为 skipped。
显式 /SCRIPT 参数覆盖会话登录钩子，此类启动方式需要另行配置。
其他线程使用用户级 securecrt-bridge skill 自动发现安装路径和会话。
skill 不改变 AI 所在沙箱的权限，也不保存远程账号密码。
安装器默认勾选检测到的 Agent，可多选 Codex、DeepSeek Harness 等客户端。
可修改目录、添加其他客户端的 skills 目录；全部取消勾选并清空额外目录即跳过。
列表及安装日志显示完整的 SKILL.md 路径；同一路径只安装一次。
升级时取消选择的旧 Skill 仅在未被用户修改时移除。卸载按安装记录处理所有目录。
命令行可用 --skills none 跳过，或重复 --skill-dir 指定 securecrt-bridge 完整子目录。

Copyright (c) 2026 LinHuiG
本项目采用 GNU Affero General Public License v3.0（AGPL-3.0-only）。
本程序不提供任何担保；你可以依照该协议再分发和修改本程序。协议全文见安装目录中的 LICENSE.txt。
源代码：https://github.com/LinHuiG/securecrt-bridge
第三方组件许可见 THIRD_PARTY_LICENSES.txt。
