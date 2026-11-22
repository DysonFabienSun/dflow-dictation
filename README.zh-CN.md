# DFlow

[English](README.md) · **简体中文** · [更新日志](https://github.com/DysonFabienSun/dflow-dictation/releases)

&lt;img src=&quot;assets/dflow.svg&quot; alt=&quot;DFlow 图标&quot; width=&quot;72&quot;&gt;

**Windows 下的 ChatGPT 语音听写工具，直接用你手头现有的订阅。** DFlow 会复用你在 Codex CLI 里登录好的 ChatGPT 账号（Plus、Pro 等订阅均支持），不用单独给 DFlow 买订阅，也不用折腾 OpenAI API Key。

平时既可以用全局快捷键在系统托盘里随唤随说，也可以打开自带的便签（Scratchpad）边说边改、自动存盘。两款工具都走 ChatGPT 的语音转文字服务，说话过程中还会结合后面的上下文，实时润色前面已经出来的字句。

## 安装与升级（Windows）

前往 [GitHub Releases](https://github.com/DysonFabienSun/dflow-dictation/releases)，在 **Assets** 里按需下载可执行文件（单下其中一个或两个全下都行），放进一个有写入权限的文件夹：

- **DFlow Dictation**（`dflow-dictation.exe`）：听写完直接进剪贴板，切到任意软件就能粘贴。
- **DFlow Scratchpad**（`dflow.exe`）：随写随存的听写便签，适合整理长段文字。

如果你的电脑已经用 Codex CLI 登录过 ChatGPT，DFlow 会直接沿用，无需重复登录。首次使用或登录过期时，跑一遍 `codex login`，按提示在浏览器里登录 ChatGPT 账号即可（详见 [OpenAI 官方 Codex 登录说明](https://learn.chatgpt.com/docs/auth)）。

升级新版本前，记得先把两个程序都退出来，再替换新的 `.exe` 文件。原来的 `config` 和 `cache` 文件夹别删，留着就能保住之前的配置和便签内容。如果你以前用的是旧版 `dflow-tray.exe`，现在改用 `dflow-dictation.exe` 启动就行；之前开过开机自启的话，记得去新版的设置里重新勾上。

## 快速上手

### DFlow Dictation（`dflow-dictation.exe`）

1. 启动 `dflow-dictation.exe` 后，屏幕会弹出持续 7 秒的快捷键小提示。不想等可以直接点**关闭**，以后不想再看也能去**设置**里关掉**显示启动提示**。平时点任务栏托盘图标，就能进**设置**或者一键**复制上次听写结果**。
2. 按下 **Ctrl+Shift+/** 即可开始听写（默认键位是美式键盘上的斜杠/问号键）。刚识别出字就会跳出实时预览条，内容变多时会自动拉高，最多显示三行。
3. 开始录音 2 秒内再按一次快捷键，直接作废本次录音；超过 2 秒后再按，就会结束录音，等识别完自动把全文复制到剪贴板。不习惯用快捷键的话，也可以直接点预览条上方的**取消**或**停止并复制**，或者随时在托盘菜单里点**取消录音**。

识别完切回任意软件，按 **Ctrl+V** 就能直接粘贴。快捷键和**开机自启动**都可以在**设置**里调整。

### DFlow Scratchpad（`dflow.exe`）

在 Windows Terminal 里用 PowerShell 7 运行 `.\dflow.exe`。用起来跟记事本差不多，边改边自动存盘。按一下 **Ctrl+Space** 开始听写，再按一下结束。

按 **Esc** 或 **Ctrl+/** 呼出命令栏，输入 `/help` 查看快捷键，输入 `/clear` 一键清空便签。

每次听写进来、粘贴进来，或者由本地智能体写入的最新一段文字，DFlow 都会自动标上下划线。下划线还在的时候，直接敲一个 **/** 就能像按 **Ctrl+/** 一样唤出命令栏。这时输入 `/lower`（`/lowercase` 的简写）能把这段标线文字转成小写；你也可以手动选中任意文字再执行转换。只要重新选中文字或动了便签内容，之前的下划线就会自动消掉。像 NASA、OpenAI 这种带连续大写字母的词会自动保持原样，不会被误转。

## 配置与设置

把两个 `.exe` 放进同一个文件夹，麦克风、Codex 登录凭据和代理配置就会自动互通；改过任意设置后，程序会在同级目录生成 `config/dflow.toml`。要是想让两边各用各的配置，把它们分开放两个文件夹就行。

共享配置在托盘的**设置**窗口或便签的 `/settings` 菜单里都能改。托盘程序每次录音前都会重新读一遍最新配置；便签这边如果是在托盘里改的设置，或者你手动改了配置文件，需要重启一下便签才会生效。

两款应用的设置里都能调**界面语言**，支持跟随系统、英文或简体中文，改完重启生效。手动改配置文件的话，在 `[ui]` 下面把 `language` 设为 `auto`、`en` 或 `zh-CN` 即可。

- **`/device`**：切换麦克风。不指定的话，默认跟着 Windows 当前的默认输入设备走。
- **`/provider`**：检查 DFlow 有没有顺利找到你的 Codex 登录凭据，找不到时也能在这里手动指定认证文件路径。自动查找规则为：若设置了环境变量 `CODEX_HOME`，就读取 `$CODEX_HOME/auth.json`；否则读取 `%USERPROFILE%\.codex\auth.json`。
- **`/proxy`**：给听写请求配代理（可选）。默认关闭，预设地址为 `http://127.0.0.1:10808`。想临时开关，输入 `/proxyon` 或 `/proxyoff`，下一次听写就会生效。代理地址支持 `http://`、`https://` 和 `socks5://` 协议，填好主机地址即可，端口可省略。

便签里的“按住说话”默认要长按 1 秒才触发。如果觉得偏慢，可以在配置文件的 `[audio]` 下面加上 `hold_delay_ms = 250`，保存后重启便签就能变得更跟手。

## 接入本地智能体（MCP）

先正常打开 DFlow，然后在本地 Windows MCP 客户端里配置用 stdio 方式启动同一个 `.exe`，并带上 `--mcp` 参数。如果用 PowerShell 搭配 Codex CLI，填上本地 DFlow 的绝对路径就行：

```powershell
codex mcp add dflow -- 'C:\Tools\DFlow\dflow.exe' --mcp
```

新开一个智能体会话后，就可以让它往 DFlow 里追加内容，或者插到某一行（比如 `## Next steps`）下面。其中 `insert_after_line` 工具需要提供该唯一行的完整原文、要插入的新内容，以及通过 `read_note` 拿到的版本号（revision）。这套工具还支持列出所有窗口、在当前光标处插入、替换整篇便签以及显式复制便签内容。同时开了多个窗口时，需要指定目标窗口 ID；替换整篇便签也同样要带版本号。智能体做的每一步修改都能撤销。注意，如果此时开着设置面板，或者正在听写、刚结束听写还没收尾，编辑工具会返回忙碌（busy）状态。

智能体接口仅限你当前的 Windows 登录会话本地访问。要是某个窗口不想开放给智能体操作，启动时加上 `--no-agent-access` 参数就能关掉。

## 从源码编译

电脑上装好 Rust 和 Cargo 后，在项目根目录运行这条命令即可：

```powershell
cargo build --release --locked --bins
```

编译完成后，运行 `target\release\dflow-dictation.exe` 就能用剪贴板听写；想用便签的话，在 Windows Terminal 里执行 `.\target\release\dflow.exe`。

项目维护者可参考[发布指南](https://www.google.com/search?q=.github/RELEASING.md)。