# DFlow

[English](README.md) · **简体中文** · [更新日志](CHANGELOG.zh-CN.md)

<img src="assets/dflow.svg" alt="DFlow 图标" width="72">

**用现有的 ChatGPT 订阅在电脑上听写。** DFlow 可直接使用 Codex CLI 中的 ChatGPT 登录信息，支持 ChatGPT Plus 和 Pro 账号，无需另购 DFlow 订阅或配置 OpenAI API 密钥。

你可以通过全局快捷键在托盘应用中听写，也可以在便笺中听写、编辑和保存文字。两款应用均将音频发送到 ChatGPT 进行转写，并可根据后续语音修正前面的文字。

## 安装或升级

DFlow 适用于 Windows。在发布页的 **Assets** 下下载所需的程序，保存到可写入的文件夹。**DFlow 听写**（`dflow-dictation.exe`）将转写结果复制到剪贴板，**DFlow 便笺**（`dflow.exe`）将转写结果写入便笺。[GitHub 发布页](https://github.com/DysonFabienSun/dflow-dictation/releases)

如果你已在本机用 ChatGPT 账号登录 Codex CLI，DFlow 会直接使用该登录信息。首次登录或登录过期时，运行 `codex login`，在浏览器中使用 ChatGPT 账号登录。[Codex 登录指南](https://learn.chatgpt.com/docs/auth)

升级前请退出两款应用，再替换程序文件。保留 `config` 和 `cache` 文件夹可保留设置和便笺。旧版的 `dflow-tray.exe` 已更名为 `dflow-dictation.exe`。如果之前启用了**开机自启动**，请在新版本的**设置**中重新开启。

## 快速上手

### DFlow 听写（`dflow-dictation.exe`）

1. 启动 `dflow-dictation.exe` 后，托盘应用会显示七秒钟的快捷键提示。点**关闭提示**可提前关闭，也可在**设置**中关闭**启动提示**。点击托盘图标可打开**设置**，也可选择**复制上次转写结果**。
2. 按 **Ctrl+Shift+/** 开始听写。默认快捷键使用美式键盘上的斜杠（问号）键。有了转写结果后，预览会立即显示，最多三行。
3. 两秒内再次按快捷键会丢弃录音。两秒后再次按下会停止录音，并在转写完成时复制全文。也可以点击预览上方的**取消**或**停止并复制**，或随时在托盘菜单中选择**取消录音**。

按 **Ctrl+V** 将转写结果粘贴到任意应用。在**设置**中可以修改快捷键或开启**开机自启动**。

### DFlow 便笺（`dflow.exe`）

在 Windows Terminal 的 PowerShell 7 中运行 `.\dflow.exe`。使用方式与记事本类似，编辑时会自动保存。按一下 **Ctrl+Space** 开始或停止听写。

按 **Esc** 或 **Ctrl+/** 打开**命令列表**。用 `/help` 查看快捷键，或用 `/clear` 清空便笺。

DFlow 会给最近一次听写、粘贴或由代理写入的文字加下划线。有新的下划线时，首次输入 **/** 会像 **Ctrl+/** 一样打开命令列表。用 `/lower`（`/lowercase` 的简写）将这段文字转为小写，也可以先手动选择要转换的文字。选择文字或修改便笺会清除原来的下划线。含连续大写字母的词会保留原来的大小写，例如 NASA 和 OpenAI。

## 设置

将两个程序放在同一个文件夹，就能共享麦克风、Codex 登录文件和网络代理设置。保存设置时，DFlow 会在程序目录中创建 `config/dflow.toml`。放在不同的文件夹可分别配置。

可在托盘的**设置**窗口或便笺的 `/settings` 菜单中修改共享设置。托盘应用会在每次录音前读取共享听写设置。通过托盘修改共享设置或直接编辑配置文件后，请重启便笺。

在任一应用的**界面语言**中，可以选择**跟随系统语言设置**、**English** 或**简体中文**。界面语言将在重启后生效。也可以在配置文件的 `[ui]` 中将 `language` 设为 `auto`、`en` 或 `zh-CN`。

- **`/device`** 选择麦克风。未指定设备时使用系统默认麦克风。
- **`/provider`** 查看 Codex 登录状态，必要时可在这里指定登录文件。自动查找时，如果设置了 `CODEX_HOME`，使用 `$CODEX_HOME/auth.json`，否则使用 `%USERPROFILE%\.codex\auth.json`。
- **`/proxy`** 配置连接 ChatGPT 所用的网络代理。代理默认关闭，预设地址为 `http://127.0.0.1:10808`。用 `/proxyon` 或 `/proxyoff` 可从下一次听写起启用或关闭代理。地址须以 `http://`、`https://` 或 `socks5://` 开头，包含主机名，可指定端口。

便笺默认在按住快捷键一秒后进入按住听写模式，松开即停止。要缩短这个时间，请在配置文件的 `[audio]` 中设置 `hold_delay_ms = 250`，然后重启便笺。

## 连接本地代理

正常打开 DFlow，然后将本地 MCP 客户端配置为通过标准输入输出启动同一个程序，并传入 `--mcp`。在 PowerShell 中运行下列 Codex CLI 命令时，请换成你本机的 DFlow 绝对路径。

```powershell
codex mcp add dflow -- 'C:\Tools\DFlow\dflow.exe' --mcp
```

开启新的代理会话，请它在便笺末尾追加文字，或在指定行（例如 `## 下一步`）下方插入文字。`insert_after_line` 需要完全匹配且只出现一次的行文字、要插入的新文字，以及由 `read_note` 返回的修订编号。代理也可以列出窗口、在光标处插入文字、替换便笺或主动将便笺复制到剪贴板。如果打开了多个窗口，请传入目标窗口 ID。替换便笺时也需要传入修订编号。每次编辑都可以撤销。菜单打开、正在听写或等待转写完成时，编辑工具会返回忙碌状态。

代理只能访问当前系统登录会话中的便笺。启动时加上 `--no-agent-access` 可禁止代理访问该窗口。

## 从源码构建

在已安装 Rust 和 Cargo 的 Windows 电脑上，从仓库根目录运行：

```powershell
cargo build --release --locked --bins
```

运行 `target\release\dflow-dictation.exe` 将听写结果复制到剪贴板，或在 Windows Terminal 中运行 `.\target\release\dflow.exe` 打开便笺。
