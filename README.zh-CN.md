# DFlow

[English](README.md) · **简体中文** · [更新日志](https://github.com/DysonFabienSun/dflow-dictation/releases)

<img src="assets/dflow.svg" alt="DFlow 图标" width="72">

**使用现有 ChatGPT 订阅的 Windows 听写工具。** DFlow 沿用 Codex CLI 的 ChatGPT 登录，支持 Plus 和 Pro 等订阅，无需另购 DFlow 订阅或配置 OpenAI API 密钥。

DFlow Dictation 通过全局快捷键录音，将转写结果复制到剪贴板。DFlow Scratchpad 把结果写入便签，编辑时自动保存。两款应用均使用 ChatGPT 的语音转文字服务，并可根据后续语音修正先前的转写结果。

## 安装与升级（Windows）

从 [GitHub 发布页](https://github.com/DysonFabienSun/dflow-dictation/releases)的 **Assets** 下载所需的程序，放入有写入权限的文件夹。

- **DFlow Dictation**（`dflow-dictation.exe`）将转写结果复制到剪贴板，供其他应用粘贴。
- **DFlow Scratchpad**（`dflow.exe`）在便签中听写和编辑，适合整理长段文字。

使用云端听写前，安装 Codex CLI，运行 `codex login` 登录 ChatGPT，并在 Windows 中允许访问麦克风。DFlow 使用 Codex 已保存到文件中的登录信息。使用自动整理时，请在 PATH 中安装新版 Codex CLI。[Codex 登录说明](https://learn.chatgpt.com/docs/auth)

升级前先退出两款应用，再替换 `.exe` 文件。设置和便签内容分别存放在 `config` 和 `cache` 文件夹中，升级时保留这两个文件夹。

从旧版 `dflow-tray.exe` 升级后，改用 `dflow-dictation.exe` 启动。如果旧版已启用**开机自启动**，在新版设置中重新启用。

## 快速上手

首次交互启动时，包括从没有听写授权设置的版本升级，选择**启用云端听写**或**继续使用，暂不启用听写**。启用后，DFlow 会使用现有 Codex 登录，将麦克风录音发送到 OpenAI 的 `chatgpt.com`。关闭云端听写时，仍可在便签中编辑文字。

在**听写设置**中检查登录状态、选择麦克风，并按需配置代理。代理默认关闭。选择**测试听写**，说完后停止录音，转写结果会显示在设置窗口中。测试可以跳过，结果不会写入便签或剪贴板。

启用听写前，可以选择**检查 ChatGPT 可用性**，查看能否连接 `chatgpt.com` 以及 DFlow 能否找到 Codex CLI。即使代理处于关闭状态，也可以用**检查代理**测试所填地址。启用后，运行**测试听写**以确认账号权限。

### DFlow Dictation（`dflow-dictation.exe`）

1. 启动 `dflow-dictation.exe`。
2. 按 **Ctrl+Shift+/** 开始听写（默认使用美式键盘上的斜杠键）。转写结果会实时显示在预览条中，最多显示三行。
3. 录音满 2 秒后，再按一下快捷键停止录音。DFlow 会将完整转写复制到剪贴板。在目标应用中按 **Ctrl+V** 粘贴。

录音未满 2 秒时，再按一下快捷键会取消录音。录音期间，按住快捷键或预览条右上方的按钮 750 毫秒也会取消录音，橙色进度条会显示按住的进度。预览条上方还提供**取消**和**停止并复制**按钮。

启用听写后，启动时会显示 7 秒的快捷键提示。选择**关闭提示**可提前关闭，也可以在**设置**中关闭**启动提示**。选择预览条上方或托盘菜单中的**复制上次转写结果**，可在下一次录音期间复制上一次完成的转写。全局快捷键和**开机自启动**都可以在设置中修改。

### DFlow Scratchpad（`dflow.exe`）

在 Windows Terminal 中用 PowerShell 7 运行 `.\dflow.exe`。便签会随编辑自动保存。按一下 **Ctrl+Space** 开始听写，再按一下结束。

按 **Esc** 或 **Ctrl+/** 呼出命令栏。输入 `/help` 查看快捷键，或输入 `/clear` 清空便签。

DFlow 会给最近一次听写、粘贴或由本地智能体写入的文字加上下划线。出现新的下划线后，首次按 **/** 会呼出命令栏。选中文字或修改便签内容会清除原有的下划线。

`/lower`（`/lowercase` 的简写）将选中的文字或带下划线的文字转为小写。NASA、OpenAI 等含连续大写字母的词保留原有的大小写。

## 配置与设置

两款程序放在同一文件夹时，共用听写授权、麦克风、Codex 登录、代理和自动整理设置。需要分别配置时，将两个 `.exe` 放在不同文件夹。保存设置时，DFlow 会在程序目录下创建 `config/dflow.toml`。

从托盘菜单或便签的 `/settings` 菜单打开**听写设置**，可以更改听写授权或重新测试。两款程序在每次录音前读取共享配置。从另一款应用修改设置后，重新打开设置窗口。

在任一应用的设置中打开**界面语言**，选择跟随系统、英文或简体中文。手动编辑配置文件时，在 `[ui]` 下将 `language` 设为 `auto`、`en` 或 `zh-CN`。应用重启后加载新的语言设置。

- **`/device`** 选择麦克风。未指定设备时，使用 Windows 当前的默认输入设备。
- **`/provider`** 检查 Codex 登录凭据，也可手动指定认证文件路径。设置了环境变量 `CODEX_HOME` 时，DFlow 读取 `$CODEX_HOME/auth.json`，否则读取 `%USERPROFILE%\.codex\auth.json`。
- **`/proxy`** 配置听写代理。代理默认关闭，预设地址为 `http://127.0.0.1:10808`。输入 `/proxyon` 启用代理，或输入 `/proxyoff` 关闭代理，从下一次听写起生效。代理地址支持 `http://`、`https://` 和 `socks5://`，须包含主机地址，端口可省略。

便签默认在按住快捷键 1 秒后进入按住说话模式，松开按键结束录音。若要将触发时间缩短到 250 毫秒，在配置文件的 `[audio]` 下设置 `hold_delay_ms = 250`，保存后重启便签。

## ChatGPT 语音词典和自动整理

在**听写设置**中打开 **ChatGPT 语音词典**，可查看、添加或移除词条的标准写法。修改也会影响 ChatGPT 中的听写。词条数量和长度上限由服务端提供。

**听写后自动整理**默认关闭。启用后，DFlow 通过 Codex App Server 将完整的听写片段和账号词表发送给 GPT-6 Luna。便签中的其他文字保留在本机，听写和整理可一起撤销。

每次整理会发起一次 GPT-6 Luna 请求，额外消耗 ChatGPT 套餐中 Codex 的 token 额度。用量取决于转写文本和词表，可能消耗积分或产生费用。[Codex 用量和计费说明](https://help.openai.com/en/articles/11369540-using-codex-with-your-chatgpt-plan)

便签显示**正在整理**时，按 **Esc** 可保留原始转写。在 DFlow Dictation 中，可以再次按下听写快捷键，也可以在预览条上方或托盘菜单中选择**跳过整理**。整理失败或超过 12 秒时也会保留原始转写。托盘程序在整理开始时复制原始转写，整理成功后再将结果写入剪贴板。

语音转写和词典管理依赖 ChatGPT 的私有接口，实际可用性取决于账号权限。接口或登录验证方式变更时，这些功能可能中断。

## 接入本地智能体（MCP）

先启动 DFlow Scratchpad，再将本地 Windows MCP 客户端配置为通过 stdio 启动同一个 `.exe`，并传入 `--mcp`。下面是在 PowerShell 中为 Codex CLI 添加 DFlow 的示例。将示例路径替换为本机 `dflow.exe` 的绝对路径。

```powershell
codex mcp add dflow -- 'C:\Tools\DFlow\dflow.exe' --mcp
```

开启新的智能体会话后，可以让智能体在便签末尾追加内容，或在指定行（如 `## Next steps`）下方插入内容。`insert_after_line` 需要提供目标行的完整原文、要插入的文本，以及 `read_note` 返回的修订号（revision）。目标行在便签中必须只出现一次。

MCP 工具还支持列出窗口、在光标处插入文字、替换整篇便签或主动将便签复制到剪贴板。有多个窗口时，需要指定目标窗口 ID。替换便签时也需要提供修订号。每次修改都可以撤销。面板打开、正在听写或等待转写完成时，编辑工具会返回忙碌状态（`busy`）。

智能体接口仅限当前 Windows 登录会话访问。启动时传入 `--no-agent-access` 可关闭该窗口的智能体访问。

## 从源码编译

在 Windows 上安装 Rust 和 Cargo 后，在项目根目录运行以下命令。

```powershell
cargo build --release --locked --bins
```

编译完成后，运行 `target\release\dflow-dictation.exe` 以使用剪贴板听写。在 Windows Terminal 中运行 `.\target\release\dflow.exe` 以打开便签。

项目维护者可参考[发布指南](.github/RELEASING.md)。
