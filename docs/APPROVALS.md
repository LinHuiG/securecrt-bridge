# AI 客户端审批配置

桥接令牌用于认证本机调用方，租约用于协调会话写入；AI 宿主的审批决定哪些工具操作可以执行。这是三个不同的机制。

## 同一会话批准一次

0.1.8 增加 `securecrt-session.exe SESSION_ID --owner CHAT_ID` 长连接入口。首次启动时批准在指定终端完成当前任务，后续通过该进程的 stdin 发送 JSON 行，无需为每条命令再启动 CLI。在 Codex 中用 `exec_command` 的 `tty: true` 保留进程，保存返回的 `session_id`，后续用 `write_stdin` 发送以换行结尾的 JSON。启动时不要接 `Out-String`。专用控制台 EXE 保留 PowerShell 标准输入；GUI EXE 的 `attach` 兼容入口仅供主动连接 stdin/stdout 管道的客户端使用。

支持 `screen`、`claim`、`exec`、`input`、`result`、`interrupt`、`acknowledge`、`close`。写入异步返回请求 ID，通过 `result` 查询完成状态。终端 ID、所有者和当前 Broker 凭据在启动时固定，输入不能覆盖。关闭或 EOF 尝试释放租约；未知结果的占用不会自动清除。Broker 重启、终端重连或通道退出后需重新建立通道。

此方式复用已批准的交互进程，不修改全局规则或永久放行任意远程命令。宿主若对后续输入另有强制审批，仍以宿主规则为准；不支持持久 stdin 的客户端继续使用独立 CLI 调用。审批被拒绝时，不改用其他传输绕过拒绝。任务范围之外的操作仍须取得相应授权。

## 只读查询长期允许

`status`、`health`、`sessions`、`screen`、`result`、`doctor`、`detect` 只用于本机信息查询。用户可以为这些固定子命令配置持久允许规则，后续变换会话或请求 ID 时无需逐一保存规则。

Codex 的用户规则位于其配置目录的 `rules/` 中。以下示例中的 EXE 路径需要替换为实际安装路径：

```python
prefix_rule(
    pattern = [
        "C:\\Tools\\SecureCRT\\SecureCRTBridge\\securecrt-bridge.exe",
        ["status", "health", "sessions", "screen", "result", "doctor", "detect"]
    ],
    decision = "allow",
    justification = "用户允许持续查询现有 SecureCRT 会话、屏幕和结果"
)
```

安装器不会自动修改 Codex 全局审批设置。规则需要用户选择后安装，重新启动 Codex 后加载。可用 `codex execpolicy check --rules <规则文件> -- <EXE路径> screen <会话ID>` 校验。

规则按命令参数匹配。使用解析后的绝对 EXE 路径直接调用；若外面包了一整段不断变化的 PowerShell 脚本，宿主可能只看到 `powershell -Command <整段文本>`，无法匹配上述规则。不要为解决这个问题允许任意 PowerShell 命令。组织级限制和当前会话的权限设置仍然优先。

## 远程命令与写入

`exec` 可以执行任意 shell 命令，不能仅因为某次用于查看日志就整体长期允许。`input`、`interrupt`、配置和安装操作也不属于只读查询。

具体写入应在用户已授权的操作、目标与范围内执行。同一任务中已明确授权的同类操作可复用该授权；切换服务器、扩大数据范围或改变操作后果时重新判断。skill 中的这项约定减少重复提问，但不会改变宿主的强制审批。

命令前缀规则不能自动理解所有 shell 命令的业务含义，因此不提供一个把任意 `exec` 自动归为“读”或“同类写”的通用开关。

官方说明：[Codex Rules](https://learn.chatgpt.com/docs/agent-configuration/rules)。
