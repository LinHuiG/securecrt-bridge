SecureCRT Bridge

双击安装包，在窗口中选择“安装 / 升级”或“卸载桥接”。安装前请关闭 SecureCRT。
可自动检测路径，也可浏览选择安装目录及包含 Sessions 的配置目录。
点击开始后，窗口显示进度条和每一步执行结果；失败原因会留在日志中。
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
  securecrt-bridge.exe doctor
  securecrt-bridge.exe sessions
  securecrt-bridge.exe screen SESSION_ID
  securecrt-bridge.exe claim SESSION_ID --owner CHAT_ID
  securecrt-bridge.exe exec SESSION_ID --owner CHAT_ID --command "hostname; pwd"
  securecrt-bridge.exe release SESSION_ID --owner CHAT_ID

你和 AI 共用终端，请轮流操作。暂停仅取消未发送命令；远程已运行的命令
需要在终端 Ctrl+C。超时不会自动重试或自动 Ctrl+C。exec 只适用于 POSIX shell。

升级/修复：关闭 SecureCRT，重新运行安装包或执行 repair。
卸载：关闭 SecureCRT，从 Windows 已安装的应用卸载，或执行 uninstall --gui。
卸载只恢复仍保持本程序安装值的配置，保留用户后续修改和原始备份。
被占用的 EXE 可能在退出卸载器后仍需删除。建议从独立安装包执行 uninstall。

现有 ActiveX 登录脚本会先运行，然后接入桥接。SecureCRT 内置 Python 登录脚本
无法通过 ActiveX 引擎串联，该类会话保持原设置，并在安装结果中列为 skipped。
显式 /SCRIPT 参数覆盖会话登录钩子，此类启动方式需要另行配置。
其他线程使用用户级 securecrt-bridge skill 自动发现安装路径和会话。
skill 不改变 AI 所在沙箱的权限，也不保存远程账号密码。
