---
name: securecrt-bridge
description: Use an existing SecureCRT SSH terminal through its local bridge, sharing the user's visible tab. Use when the user requests work in an already connected SecureCRT or QiZhi SSH session.
---

# SecureCRT shared terminal

This bridge operates the user's actual SecureCRT tab. Commands and output are visible to them. It does not establish a separate SSH connection or retrieve login credentials.

Discover the installed CLI with PowerShell:

```powershell
$scbInstall = Get-Content -LiteralPath "$env:USERPROFILE\AppData\Local\SecureCRTBridge\installation.json" -Raw | ConvertFrom-Json
$scb = Join-Path $scbInstall.install_dir 'securecrt-bridge.exe'
& $scb status | Out-String
& $scb sessions | Out-String
```

If the service is stopped, `& $scb start` starts it. If no session appears, the user must open the authorized SSH connection in SecureCRT/QiZhi. `doctor` reports installation and hook diagnostics without displaying the secret token. Do not change COM registration, reset sessions, or substitute the old prototype scripts to troubleshoot.

Select the intended session by its returned opaque `id`, caption, and screen contents. Tab indexes and captions are mutable and are not identifiers. If the intended host remains ambiguous, ask before executing. A caption alone does not prove the remote host identity.

The window need not be in the foreground. Each actor retains its actual tab object, so switching tabs does not redirect a command. Different session IDs may be used concurrently with independent leases; writes to one session are serialized. Do not use focus changes or tab indexes to choose a target.

```powershell
& $scb screen SESSION_ID | Out-String
& $scb claim SESSION_ID --owner CHAT_ID | Out-String
& $scb exec SESSION_ID --owner CHAT_ID --command 'hostname; pwd; id' --timeout 15 | Out-String
& $scb release SESSION_ID --owner CHAT_ID | Out-String
```

Use a unique, stable owner string for this chat. A lease prevents other chats from concurrently writing. Do not steal or release another chat's lease. Release after the task; renew using `claim` if it expires. The user may type in the same tab, so coordinate turn-taking when needed. Read the screen before writing; `exec` requires an idle POSIX shell prompt, not a password prompt, editor, database client, or running program.

`exec` wraps a command in `sh -c`, captures output and exit code, and leaves the parent shell directory/environment unchanged. Use a single command such as `cd /path && ...` for directory-sensitive work. To avoid quoting issues, write a UTF-8 command file inside the workspace and use `--file PATH`. `input --text TEXT` sends literal terminal input (including explicit CR/control characters) to the existing shell; it does not detect command completion. Use it only when interactive input is actually needed and authorized.

Every write prints a `request_id` before dispatch. A timeout/disconnect may mean the command ran. Query `result REQUEST_ID` and inspect `screen`; never blindly repeat a write after an unknown result. Reusing an id with the identical payload deduplicates within the same service lifetime. Service restart loses in-memory results; an unknown id is not evidence that a command was never sent. Outputs are limited to 1 MiB per command and retained in memory for 30 minutes.

An unknown outcome holds the session busy. Once the user has interrupted it or you have verified that the shell is idle, `acknowledge SESSION_ID --owner CHAT_ID` clears this hold. Acknowledging does not interrupt or retry the command.

`pause [SESSION_ID]` and `resume [SESSION_ID]` control bridge input. Pause cancels queued commands; it does not stop a command already running remotely. The user can interrupt in their terminal using Ctrl+C. Do not resume a user-paused bridge without their intent to do so.

The API only listens on 127.0.0.1 and uses a local bearer token. Prefer the CLI; never display, copy into a skill, or commit connection.json. Local sandbox filesystem/network restrictions still apply; the skill grants no extra access and no extra authority to mutate remote systems. Operate only within the user's task authorization.
