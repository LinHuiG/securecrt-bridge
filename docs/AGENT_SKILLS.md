# Agent Skill 安装路径

0.1.7 使用客户端多选列表。自动检测只看配置目录、支持的环境变量和 PATH 入口；不启动客户端、不扫描会话内容。已发现的客户端与历史安装位置默认选中，没有发现的客户端可手动勾选。未知 Agent 使用“其他客户端 / 添加目录…”。

安装器写入标准目录包 `securecrt-bridge/SKILL.md`，该文件只包含 `name`、`description` 和通用 CLI 操作说明。没有客户端专有 frontmatter，不写入令牌、远程密码或客户端审批配置。

目录依据（2026-10-14 核对）：

- [DeepSeek Harness skill-filesystem](https://github.com/deepseek-ai/deepseek-harness/blob/master/packages/skill/skill-filesystem/README.md)：`DSH_HOME` 或 `~/.dsh` 的 `skills` 子目录。需加载 filesystem skill provider；自定义 provider 配置可能改变目录。
- [Claude Code Skills](https://code.claude.com/docs/en/skills)：用户级 `~/.claude/skills`。使用非默认配置目录时，安装界面可手动修正路径。
- [Cursor Skills](https://cursor.com/help/customization/skills)：`~/.cursor/skills`。
- [Copilot Agent Skills](https://docs.github.com/en/copilot/concepts/agents/about-agent-skills)：用户级 `~/.copilot/skills`。
- [Gemini CLI Skills](https://github.com/google-gemini/gemini-cli/blob/main/docs/cli/using-agent-skills.md)：用户级 `~/.gemini/skills`。
- [OpenCode Skills](https://opencode.ai/docs/skills/)：用户配置目录下的 `opencode/skills`。
- Codex 延续既有 `CODEX_HOME/skills` 或 `~/.codex/skills` 安装约定。

这是路径及文件格式适配，未声称在所有客户端上完成实际调用验证。Skill 加载、终端工具和审批仍由各客户端管理。用户可在客户端中重新加载技能或开启新会话确认。

`installation.json` 的 `skill_dirs` 数组记录本次选择；空数组表示跳过。缺少该字段的旧版记录按 `skill_dir` 单目录迁移。安装日志另外保存曾管理过的 Skill 目录及文件哈希，用于部分安装恢复、取消选择后的清理和卸载。只删除哈希仍一致的自有文件，不递归删除客户端目录；用户修改的文件保留并提示。
