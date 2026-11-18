# 野菜 · AI 小说工作台（阶段 7：智能体团队 + 动态 Prompt 编排）

oh-story-claudecode 的桌面外壳。写作流程（skill）跑在宿主 Agent 里，本应用负责：项目目录、编辑器、AI 子进程、钩子阻断可视化、模型提供商与体检。

当前状态：三栏布局 + 新建/打开项目 + 环境自检（阶段 2）、CodeMirror 编辑器 + 自动保存 + 快照 + 文件监听与冲突（阶段 3）、引擎适配器 + 脚本门禁 + Hook 拦截（阶段 4）、多提供商 + 动态环境注入 + 模型拉取 + 写完自动体检（阶段 5）、提供商/快照删除 + 列表交互（阶段 6）、六个智能体团队面板 + 按角色注入 system prompt（阶段 7）。

---

## 1. 运行环境

| 依赖 | 版本 | 用途 | 缺失后果 |
|---|---|---|---|
| Node.js | ≥ 18 | 前端构建 + oh-story 的检查脚本 | 构建失败；oh-story 提交正文时报「安装 Node.js 18 或更高版本…」 |
| Python | 3.x | oh-story 的 `storyctl.py` 等确定性脚本 | 章节检查/提交不可用 |
| bash（Git Bash） | 任意 | oh-story 的 8 个 `.sh` hook | Hook 阻断不生效 |
| 宿主 Agent CLI | `claude` / `codex` / `opencode` 任一 | AI 生成（阶段 4 才接入） | 只影响 AI 生成，不影响建项目与编辑 |
| Rust 工具链 | ≥ 1.77 | 编译 `src-tauri` | 无法 `npm run tauri dev` |
| C 工具链 | MSVC Build Tools 或 MinGW-w64 | Rust 链接器 | 同上 |

> 本机实测环境（2026-11-05）：Node v24.20.0 ✓、Python 3.14.6 ✓、Git 2.55（bash 位于 `E:\Program Files\Git\bin\bash.exe`，不在 PATH，应用会自动定位）✓、宿主 CLI 未安装（`engine` 项显示缺失，符合预期）。
> 本机没有 MSVC 构建工具，因此 Rust 采用 **`x86_64-pc-windows-gnu` + MinGW-w64** 组合。

---

## 2. 首次准备（只做一次）

```powershell
# 1) 前端依赖
cd "D:\桌面\大制作\小说\storydesk"
npm install

# 2) Rust 工具链（无 MSVC 时用 gnu host）
#    已装过 rustup 的话只需：
rustup default stable-x86_64-pc-windows-gnu

# 3) MinGW-w64（Rust gnu 目标的链接器），解压后把 bin 目录加入 PATH
#    应用开发时用下面的方式临时注入即可：
$env:PATH = "$env:USERPROFILE\.mingw64\mingw64\bin;" + $env:PATH
```

如果已经有 Visual Studio Build Tools（含 C++ 工作负载），把第 2、3 步换成默认的 `stable-x86_64-pc-windows-msvc` 即可，不需要 MinGW。

---

## 3. 启动

```powershell
cd "D:\桌面\大制作\小说\storydesk"
$env:PATH = "$env:USERPROFILE\.mingw64\mingw64\bin;" + $env:PATH   # 仅 gnu 工具链需要
npm run tauri dev
```

或者直接用封装好的脚本（自动注入 PATH、ASCII 构建目录，并做前置校验）：

```powershell
pwsh -File scripts\dev.ps1 -Task dev      # 开发（等价于 npm run tauri dev）
pwsh -File scripts\dev.ps1 -Task build    # 打包（等价于 npm run tauri build）
```

窗口规格：1200×800，最小 960×640（`src-tauri/tauri.conf.json`）。

### 3.0 没有 claude CLI 时怎么验引擎

`scripts\fixtures\claude-shim\` 是一个假的 `claude`（Node 脚本 + `.cmd` 壳），按 Claude Code 的
`--output-format stream-json` 格式吐 chunk，可以被 `ClaudeCodeAdapter` 当成真 CLI 跑通全链路：

```powershell
$env:PATH = "<repo>\scripts\fixtures\claude-shim;" + $env:PATH
"block" | Set-Content scripts\fixtures\claude-shim\mode.txt   # block | slow | ok
```

每次被调用都会往同目录 `args.log` 追加一行（记录 argv、prompt 行数、Key 是否存在、
Key 是否出现在参数里），用来核对「Key 只走环境变量」。测完把该目录从 PATH 里去掉即可；
装好真 CLI（`npm i -g @anthropic-ai/claude-code`）后同一个适配器直接生效。

`mode.txt` 三种模式的差别：

| 模式 | 行为 | 用来验 |
|---|---|---|
| `block` | 吐两段流后 `stderr` 写 `⛔ 写正文被拦截：…缺少细纲…` 并 `exit 2` | Hook 阻断卡片 |
| `slow` | 一直流式输出（最多 600 拍） | 「停止」是否真的 kill 进程树 |
| `ok` | 正常结束，且**真的写文件**：prompt 里带「细纲」就按 `项目目录：` + `第N章` 落盘 `大纲/细纲_第NNN章.md` | 智能体闭环（阶段 7） |

`ok` 模式的写入是**真写盘**，所以「点一下智能体 → 文件树上出现细纲」这条链路是端到端成立的，
不是 UI 假象。它认阿拉伯数字与中文数字两种章号（`第3章` / `生成第一章细纲`）。

### 3.1 构建目录必须是纯 ASCII（Windows + MinGW）

本项目源码在中文路径下（`D:\桌面\大制作\小说\storydesk`），而 **MinGW 的 `ld` / `windres` 不支持 UTF-16 路径**，直接在源码目录里 `cargo build` 会报：

```
ld.exe: cannot find ...\storydesk_lib-xxxx.rcgu.o: No such file or directory
```

解决办法只有一个：把构建产物放到纯 ASCII 目录（`scripts\dev.ps1` 已内置）：

```powershell
$env:CARGO_TARGET_DIR = "C:\storydesk-build"
```

不要用 `subst` 映射盘符绕过——同一物理目录的两种拼写会让 cargo 的指纹缓存互相打架。

源码里另外两处针对该环境的适配（勿随手改回去）：

- `src-tauri/build.rs` 先把 `icons/icon.ico` 复制到 `OUT_DIR`，再交给 `tauri_build`，否则 `windres` 读不了中文路径下的 ico。
- `src-tauri/Cargo.toml` 的 `[lib] crate-type` 只保留 `["rlib"]`；带上 `cdylib` 时 MinGW 的 `dlltool` 会报 `export ordinal too large: 92527`。

其他命令：

```powershell
npm run build       # 只构建前端（tsc --noEmit + vite build），用于快速校验
npm run typecheck   # 只跑 TypeScript 严格检查
npm run tauri build # 打安装包（NSIS），产物在 $env:CARGO_TARGET_DIR/release/bundle/
```

---

## 4. 阶段 2 验收自测

1. **三栏布局**：启动后左（目录）/ 中（编辑器占位）/ 右（AI 面板占位），中间两条分隔条可拖拽（双击复位），左右栏可折叠。
2. **新建项目**：欢迎页 →「新建项目」→ 选目录（内置目录选择器，不动用 fs 插件权限）+ 填书名 →「创建项目」。磁盘上应出现：
   ```
   <你选的目录>/长篇/<书名>/{设定,大纲,正文,追踪,对标,参考资料,.story}
   <你选的目录>/.active-book          # 内容为 `长篇/<书名>`（相对路径，无尾随换行）
   <你选的目录>/.storydesk/{snapshots,runs,logs,prompts}
   ```
3. **环境自检**：设置 →「环境自检」，应看到 Node/Python/bash/宿主 CLI 四项的版本、可执行文件绝对路径、缺失项的安装指引；「重新检测」可重跑。
4. **无权限报错**：前端控制台不应出现 `not allowed` / `permission denied` / `plugin not found`。能力清单只有 `core:default`，没有 `fs:` / `shell:` / `http:` / `dialog:`。
5. **文件操作只走 invoke**：`grep -r "from \"@tauri-apps/plugin" src/` 应为空；所有磁盘访问都经 `src/lib/ipc.ts` → Rust 命令。

路径守卫自测（可选）：在 DevTools 里执行
```js
await window.__TAURI_INTERNALS__.invoke("validate_path_cmd", {
  workspaceRoot: "<你的项目目录>",
  target: "../etc/passwd",
});
// 期望拒绝：{ kind: "pathEscape", ... }
```

### 4.1 自动化验收（CDP）

WebView2 支持开远程调试端口，于是可以在不装 Playwright 的情况下驱动界面（脚本已放在 `scripts/`）：

```powershell
# 带着调试端口启动
$env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = "--remote-debugging-port=9222"
pwsh -File scripts\dev.ps1 -Task dev

# 另一个终端：在页面里执行任意 JS，并取回返回值
node scripts/cdp.mjs "document.title"
node scripts/cdp.mjs "[...document.querySelectorAll('button')].map(b=>b.textContent.trim())"

# 直接截 WebView 画面（不受其它窗口遮挡）
node scripts/shot.mjs "docs/phase2-workspace.png"
```

驱动 React 受控输入时必须用原生 setter，否则 `onChange` 不触发：

```js
Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value").set.call(el, "D:/路径");
el.dispatchEvent(new Event("input", { bubbles: true }));
```

### 4.2 本阶段实测结论（2026-11-05）

| 验收项 | 结果 | 证据 |
|---|---|---|
| ① `npm run tauri dev` 起窗口 + 三栏 | 通过 | vite `http://localhost:1420/` + `Running C:\storydesk-build\debug\storydesk.exe`，窗口 1215×837（阶段 2 当时品牌仍叫 StoryDesk，阶段 5 已更名「野菜 · AI 小说工作台」） |
| ② 新建项目落盘 | 通过 | `<工作区>/长篇/我的小说/{设定(含 3 个模板 md),大纲/大纲.md,正文,追踪,对标,参考资料,.story}`、`.storydesk/{snapshots,runs,logs,prompts/templates}`、`.active-book` = 19 字节 `长篇/我的小说`（末字节 0xB4，非换行） |
| ③ 环境自检显示状态 | 通过 | Node v24.20.0、Python 3.14.6、bash 5.3.15（`E:\Program Files\Git\cmd\../bin/bash.exe`，提示已自动定位）、宿主 CLI 缺失并给出安装命令 |
| ④ 无 Tauri 权限报错 | 通过 | 能力清单仅 `core:default`；建项目/开项目/选目录/环境自检全程只调用自定义命令，无 `not allowed` / `plugin not found` |
| ⑤ 文件操作只走 invoke | 通过 | `src/` 内无 `@tauri-apps/plugin-*` 引用，磁盘访问全部经 `src/lib/ipc.ts` → Rust 命令 |
| 附：重开保留 | 通过 | 关闭项目再「打开项目」选同一目录可还原；重启应用后自动恢复上次项目 |

### 4.3 阶段 3 验收自测（编辑器 / 自动保存 / 冲突 / 快照 / 右键菜单）

1. **打开文件**：左栏点任一 `.md` → 中间出现 CodeMirror（Markdown 高亮、衬线字体、行宽 820px），顶部显示相对路径。
2. **自动保存**：改内容后停手 1 秒，状态栏变「保存中…」→「已保存 hh:mm:ss」；`Ctrl/Cmd + S` 立刻保存（编辑器内和窗口任意位置都生效）。
3. **字数统计**：状态栏「字数 N (中文 x / 西文 y / 字符 z)」——中文字符逐字计，`#`、`>` 等 Markdown 符号不计。
4. **外部修改**：
   - 编辑器没有未保存改动 → 静默跟随磁盘（内容直接刷新，无提示）；
   - 编辑器有未保存改动 → 顶部弹冲突条「磁盘版本已更新 → [重新加载] / [保留我的版本（另存为 .bak）]」，冲突未处理前**不会**自动保存覆盖磁盘。
   - 「保留我的版本」会把磁盘版本写成 `<文件名>.bak` 再写入自己的内容。
5. **快照**：每次覆盖保存前，旧内容存到 `.storydesk/snapshots/<sha1(相对路径)>/<ISO时间戳>.md`（含 `path.txt` 记录原相对路径），滚动保留最近 20 份。编辑器顶部「快照」下拉可查看并「恢复」。
6. **右键菜单**：目录上右键 → 新建章节（`正文/第NNN章_标题.md`，章号自动取 max+1）/ 新建 Markdown 文件；文件与目录上右键 → 重命名…、删除…（二次确认，实际移进 `.storydesk/trash/<时间戳>/<原相对路径>`）。

### 4.4 阶段 3 实测结论（2026-11-05）

| 验收项 | 结果 | 证据 |
|---|---|---|
| ① 点树加载 md | 通过 | 点 `设定/全书梗概.md` → `.cm-content` 出现，内容与磁盘一致（`# 全书梗概` + 模板段落） |
| ② 改内容 1s/`Ctrl+S` 落盘 | 通过 | paste 后 1.6s 状态栏「已保存」，磁盘文件含新增句；`Ctrl+S` 在 paste 后 150ms 内完成落盘 |
| ③ 字数统计 | 通过 | 状态栏 `字数 87 (中文 84 / 西文 3 / 字符 167)` —— 84 个汉字逐字计，`1. 2. 3.` 记 3 个西文词 |
| ④ 外部改动感知 | 通过 | 不 dirty 时外部追加 `<!-- 静默刷新测试 -->` 后编辑器自动刷新、无提示；dirty 时弹出「磁盘版本已更新」冲突条，点「保留我的版本」生成 `全书梗概.md.bak`（353B，含外部写入行）且主文件保留本地版本 |
| ⑤ 保存后生成快照 | 通过 | `.storydesk/snapshots/ca0452e5…/2026-11-05T01-58-06.486Z.md`（138B）等 4 份；下拉「恢复」后编辑器与磁盘同时回退到旧版本 |
| ⑥ 右键菜单落盘 | 通过 | 新建章节 → `正文/第001章_雨夜来客.md`（139B，含标题与提示）并在编辑器打开；重命名 → `第001章_雨夜归人`（无扩展名时自动补原扩展名，`全书梗概` → `全书梗概-改名测试.md`）；删除 → 移进 `.storydesk/trash/2026-11-05T02-03-37.563Z/长篇/我的小说/正文/…`，树同步移除 |

**阶段 3 踩坑记录**（都是实测撞出来的，别再踩）：

1. **Watcher 与 store 的路径拼写必须统一**：`canonicalize` 在 Windows 上产出 `\\?\D:\…`，而 `read_file` 返回的是去前缀的 `D:\…`，`samePath` 直接比对会永远不相等 → 外部改动永远感知不到。修法：Rust 侧 `clean_path()` 在发事件前去掉 `\\?\` / `\\?\UNC\`，前端 `samePath` 再做一次防御性归一。
2. **冲突期间必须挡住自动保存**：否则 1 秒后的 autosave 会把别人刚写进磁盘的内容直接盖掉，冲突条变成摆设。`save()` 里 `if (conflict) return false`，`handleDiskChange` 设冲突时 `clearAutosave()`。

### 4.5 阶段 4 验收自测（引擎 / 流式 / 阻断 / 取消 / Key）

1. **面板就绪**：右侧三栏第三栏是 AI 面板。顶部有引擎下拉（`Mock 引擎（内置）` / `Claude Code CLI`）、状态徽标、「门禁脚本」徽标（装了 storyctl.py 显示 `就绪`，否则 `缺失`）、章号。
2. **流式生成**：点「生成细纲」→ 中间区按 seq 递增出现 `status`（细灰行）/ `text`（正文）/ `tool_use`（折叠成一行，例如 `📖 读取文件：设定/文风.md`），状态徽标 `运行中` → `已完成`。
3. **Hook 阻断**：点「写本章正文」（第 1 章且缺 `大纲/细纲_第001章.md`）→ 状态 `被拦截`，出现红色阻断卡片：`缺少前置文件` + 原因 + `缺：大纲/细纲_第001章.md` + `<details>hook 原文（stderr / 脚本输出）</details>` + 按钮「一键生成第1章细纲 / 复制原因 / 收起」。
4. **一键补前置**：点「一键生成第N章细纲」→ 卡片收起并起一条 `outline` 流水线。
5. **取消**：运行中点「停止」→ 流停止增长、状态 `已取消`；Windows 下走 `taskkill /T /F /PID`，子进程树一并结束（Node 里再 spawn 的子进程也不会留下）。
6. **Key 不进前端/日志**：设置 → `AI 引擎` → 填 Key → 「保存」→ 徽标 `已保存`、`后端：windows-native`；运行时只以 `ANTHROPIC_API_KEY` 注入子进程环境，命令参数里没有 Key（可用 `args.log` 核对 `apiKeyInArgs:false`）。「清除」后凭据库不留残留。
7. **prompt 契约**：面板底部「送进引擎的 prompt」折叠块里必须逐字是那四行（`项目目录：` / `任务包：` / `先完整读取任务包…` / `交付后只回任务包开头要求的几项`）；填了「补充要求」才会多一行。

### 4.6 阶段 4 实测结论（2026-11-05）

| 验收项 | 结果 | 证据 |
|---|---|---|
| Mock 流式 | 通过 | 350ms 时已有 `工作区：`、`任务包：未生成…`、`📖 读取文件：设定/题材定位.md`；2.9s 后 `已完成` + 细纲正文 |
| Mock 模拟拦截 | 通过 | 第 1 章「写本章正文」→ `被拦截`，卡片逐字为「第 1 章缺少细纲（大纲/细纲_第001章.md）」 |
| 前置门禁（真脚本） | 通过 | 把 `storyctl.py` 等 8 个脚本放进 `.storydesk/tools/` 并在 `.storydesk/config.json` 写 `storyctlPath` 后，徽标变 `门禁脚本就绪`，点「写本章正文」被拦在引擎之前（`ranEngine:false`），原因为 storyctl 原话「这次没查成：第 1 章没有细纲，或者细纲不止一份。」 |
| 真实适配器（shim） | 通过 | 用 `scripts\fixtures\claude-shim` 当 claude：面板显示 `启动方式：node + fake-claude.mjs（已解开 .cmd 包装）`，`args.log` 记录 `hasStreamJson:true`、`promptLines:4`；`📖 读取文件` / `✍️ 写入文件` 折叠行正常；子进程 exit 2 → 同一张阻断卡片（hook 原文来自子进程 stderr） |
| 取消（真子进程） | 通过 | 慢速模式下点「停止」→ 状态 `已取消`，1.8s 后 `fake-claude` 的 node 进程已不存在 |
| Key 只走 env | 通过 | 存入后 `args.log` 显示 `apiKeyPresent:true` / `apiKeyInArgs:false`；清除后 `present:false` |
| 前置→引擎→Hook 顺序 | 通过 | 细纲补齐后再点「写本章正文」：门禁放行（无阻断）→ 引擎真启动 → 子进程 exit 2 → 阻断卡片 |

**阶段 4 踩坑记录**：

1. **`.cmd` 不能直接吃多行参数**：npm 在 Windows 上把 CLI 装成 `claude.cmd`，Rust 的 `Command` 把**含换行**的参数转给它时会失败（`batch file arguments are invalid`）。而我们的 prompt 天生四行。修法：`Launcher::for_program()` 读出 `.cmd` 里真正执行的 `*.js` 入口（`%dp0%` 前缀按脚本目录展开），改成 `node cli.js …` 启动；找不到就退回原程序并如实报错。
2. **`storyctl.py chapter check` 不是前置门禁**：它校的是**已经写出来的正文**（先报「没有细纲」，补上细纲立刻改报「没有正文」）。前置只该管「细纲在不在、够不够」，所以 `gate::precheck_prose()` 采用：装了 `guard-outline-before-prose.sh` 就按 PreToolUse 协议真跑 hook（stdin JSON / exit 2 + stderr），没装就本地按同口径检查（恰好一份 `大纲/细纲_第NNN章.md` 且去空白 ≥ 300 字）。去 AI 味那条流水线仍用 `chapter check`（那时正文已存在）。
3. **`chapter check --plain` 的失败文案和 hook 的 stderr 不是一个格式**：门禁走的是白话（`这次没查成：…`）或 JSON（`error_code: CHECK_FAILED`），硬套 `classify(stderr)` 会全落到 `unknown`。新增 `hook_parse::gate_block()` + `classify_gate()` 把两边都归一，卡片才显示得出「缺少前置文件 → 一键生成细纲」。
4. **脚本自己崩了不能当成「体检不通过」**：缺相邻模块时 Python 会打 traceback，之前被当成 hook 文案贴到卡片上。现在 `gate::run_python()` 检测到 `Traceback (most recent call last)` 就直接返回错误（附最后一行），界面显示「运行失败」而不是「被拦截」。
5. **Mock 的拦截判定要看 `kind`**：四行 prompt 里并没有「写本章正文」字样，早先靠 `prompt.contains("写本章正文")` 判定，结果永远不触发。改成 `ctx.kind == RunKind::Prose || prompt.contains(...)`。

### 4.7 阶段 5 验收自测（提供商 / 模型拉取 / 注入隔离 / 体检）

1. **全局名称**：窗口标题与 `document.title` 都是 `野菜 · AI 小说工作台`；关于页第一行同名，版本行 `v0.1.0 · 阶段 5 提供商与体检`。
2. **两个 Tab**：设置 → `AI 引擎` → 子选项卡为 `第三方模型提供商` / `自定义模型 API`（Radix Tabs）。
3. **自定义提供商**：`自定义模型 API` 填 Provider ID（小写字母）/ 显示名称 / API 地址 / 协议 / 密钥 → 点「获取可用模型」（请求中显示「获取中...」，`GET {base_url}/models`，Bearer 用当前输入或已存 Key）→ 弹带搜索框的模型选择弹窗 → 勾选 →「确认添加」→ 模型以 chips 出现 →「创建提供商」。
4. **持久化**：重启应用后提供商与模型目录仍在；`%APPDATA%\com.storydesk.app\settings.json` 里只有 id / 名称 / 地址 / 协议 / 模型，**没有 Key**，Key 在系统凭据库（`cmdkey /list` 可见 `LegacyGeneric:target=<provider-id>.storydesk`）。
5. **动态注入 + Key 隔离**：选中带 Key 的提供商后跑任意流水线，dev 控制台出现 `[storydesk] 环境注入：OPENAI_API_KEY, OPENAI_BASE_URL（provider: <id> / <name>）`（anthropic 是 `ANTHROPIC_API_KEY` / `ANTHROPIC_BASE_URL`，bedrock 是 `AWS_BEARER_TOKEN_BEDROCK`）；没配 Key 时**不注入任何变量**，提示「沿用 CLI 自身登录态」。用 shim 可核对 `args.log` 的 `apiKeyInArgs:false`。
6. **后置体检**：写完正文（`RunKind::Prose` 且正常退出）自动跑体检，界面底部追加卡片；正文不到 800 字时是「体检未通过：字数不足：N 字（建议 ≥ 800 字）」+「重新体检」+「本次 N 字」+ 可展开的体检输出；`ai_pattern` 类问题额外给「去 AI 味」按钮。
7. **无回归**：停止仍是 `已取消`；第 N 章缺细纲仍弹红色阻断卡片；prompt 折叠块仍是四行契约（补充要求才多一行）。

### 4.8 阶段 5 实测结论（2026-11-05）

| 验收项 | 结果 | 证据 |
|---|---|---|
| ① 更名为「野菜 · AI 小说工作台」 | 通过 | `document.title` 与窗口 `MainWindowTitle` 均为 `野菜 · AI 小说工作台`；关于页含同名 + `v0.1.0 · 阶段 5 提供商与体检`；`productName` 含中文与「·」不影响 `tauri dev` |
| ② 两个 Tab + 自定义提供商可保存 | 通过 | `[role=tab]` = `["第三方模型提供商","自定义模型 API"]`；建出 `mock-gateway / 本地 Mock 网关 / openai / 2 个模型`，重启后仍存在 |
| ③ 「获取可用模型」真拉到列表 | 通过 | 本地 mock 网关（`scripts/fixtures/mock-models-server.mjs`，`127.0.0.1:8791/v1/models`）返回 5 个 id，弹窗逐项列出并可勾选；服务端日志记 `{"method":"GET","url":"/v1/models","hasAuth":true,"authHint":"Bearer …-key"}` |
| ④ Key 不落 JSON、注入正确变量名 | 通过 | `settings.json` 无 `sk-mock`、无 `apiKey`；`cmdkey /list` 有 `LegacyGeneric:target=mock-gateway.storydesk`；dev 日志 `[storydesk] 环境注入：OPENAI_API_KEY, OPENAI_BASE_URL（provider: mock-gateway / 本地 Mock 网关）`；shim `args.log` 记 `promptLines:4 / hasStreamJson:true / apiKeyInArgs:false` |
| ⑤ 后置体检自动追加卡片 | 通过 | 正文 77 字节（26 字）→ 运行 `已完成` 后自动出现「体检未通过：字数不足：26 字（建议 ≥ 800 字）」；`invoke('gate_postcheck', …)` 返回 `{"ok":false,"issues":[{"code":"wordcount",…}],"chars":26}` |
| ⑥ 无回归 | 通过 | 第 2 章点「写本章正文」→ `⛔ 缺少前置文件 / 第 2 章缺少细纲（storyctl 原话：这次没查成：第 2 章没有细纲，或者细纲不止一份。）/ 缺：大纲/细纲_第002章.md`；运行中点「停止」→ 状态 `已取消` 且无 `fake-claude` node 进程残留 |

**阶段 5 踩坑记录**：

1. **Radix 的 Tabs / Select 不吃 `click()`**：触发器响应 `mousedown` 序列，CDP 里必须 `pointerdown → mousedown → mouseup → click` 依次派发才切页（`scripts/acc5.mjs` 的 `press()`）。
2. **设置弹窗与模型选择弹窗是嵌套的两个 `[role=dialog]`**：取内层要用「文本最短且含『确认添加』」定位；且弹窗在两次 CDP 调用之间会被组件重渲染关掉，填表 → 拉列表 → 勾选 → 创建必须在一次 `Runtime.evaluate` 里做完。
3. **体检卡片的旧文案会骗过等待条件**：`waitFor(alert 含"体检")` 会立刻命中上一张卡，判定新卡片要比对内容差异。
4. **`Set-Content` 在本机会静默失败**：写 shim 的 `mode.txt` 必须用 `[System.IO.File]::WriteAllText`，否则模式一直是旧值。
5. **后置体检放在命令层，不塞进适配器**：适配器只管「怎么跑 CLI」，门禁语义与引擎无关；`ai_start` 在 `RunOutcome::Done && kind == Prose` 后调 `gate::postcheck_prose`，再用 `ai://postcheck` 推给前端。命令层拿得到 workspace/book/chapter，适配器拿不到。

### 4.9 阶段 6 验收自测（提供商删除 / 快照删除）

1. **提供商表格**：设置 → `AI 引擎` → `第三方模型提供商` 底部「全部提供商」为真 `<table>`，六列为 `显示名称 / Provider ID / Key / 协议 / 模型 / 操作`；有 Key 是绿色 `有 Key`、没有是灰色 `无 Key`；当前行显示「当前」胶囊 + 删除按钮，其余行是「设为当前」+ 红色「删除」。
2. **删除提供商（含 Key 清理）**：新建一个带 Key 的提供商并设为当前 → 点它的「删除」→ 二次确认文案是「确定要删除提供商【名称】吗？其对应的 API Key 也将被永久清除。」→ 确认后行消失，`settings.json` 里该 id 不见了、`activeProviderId` 变成 `null`，`cmdkey /list` 里 `<id>.storydesk` 也没了（其它提供商的凭据不受影响）。
3. **内置提供商也能删**：删掉一个内置项（如 `moonshot`）后重启，它**不会**被补种回来（`AppSettings.removedBuiltins` 记住被删的内置 id）。
4. **快照面板**：编辑器工具栏「快照 N」是个 Popover：顶部提示「每次覆盖保存前的旧版本（最多留 20 份）」，没有快照时是「还没有快照。保存一次后就会出现。」；每行是 `时间戳 + 字节数 + 恢复 + 垃圾桶`（时间戳保留毫秒，同一秒的多份不会显示成一样）。
5. **删除快照**：点垃圾桶 → 二次确认「确定删除此版本快照吗？不可恢复。」→ 确认后 `.storydesk/snapshots/<sha1>/<时间戳>.md` 真的没了（`Test-Path` 为 False），面板列表同步少一条。
6. **防误触**：提供商删除与快照删除都必须过二次确认（`ConfirmDialog`，danger 样式）。
7. **无回归**：「获取可用模型」还能拉到列表；选中带 Key 的提供商跑流水线，dev 控制台仍是 `[storydesk] 环境注入：…`；shim 的 `args.log` 仍是 `promptLines:4`（`Launcher` 解 `.cmd` 没被破坏）；取消仍是 `已取消`；缺细纲仍弹阻断卡片。

### 4.10 阶段 6 实测结论（2026-11-05）

| 验收项 | 结果 | 证据 |
|---|---|---|
| ① 删除提供商（含当前项） | 通过 | 用 `scripts/acc6.mjs add-provider` 建 `验收网关 / acc6-gateway / 有 Key / 1 个模型` 并设为当前 → `delete-provider` 的确认文案逐字为「删除提供商确定要删除提供商【验收网关】吗？其对应的 API Key 也将被永久清除。取消删除」→ 删后 `remaining = openai,anthropic,moonshot,bedrock,mock-gateway`；`settings.json` 里 `activeProviderId` 为 `null`、`acc6-gateway` 已移除、无 `sk-acc6` 明文；`cmdkey /list` 已无 `acc6-gateway.storydesk`，`mock-gateway.storydesk` 仍在 |
| ② 列表 UI 对齐设计稿 | 通过 | `providers` 步读回 5 行：`OpenAI / openai / 无 Key / openai / 4 个 / [设为当前, 删除]`……`本地 Mock 网关 / mock-gateway / 有 Key / openai / 2 个 / [当前, 删除]`；截图 `docs/phase6-providers.png` |
| ③ 删除快照（物理删除） | 通过 | 连写 3 次后有 7 份；`delete-snapshot` 的确认文案「删除快照确定删除此版本快照吗？不可恢复。2026-11-05 03-23-13.573取消删除」；确认后该 `.md` 在磁盘上消失（`2026-11-05T03-23-13.558Z.md` 查不到），面板 `count` 从 7 降到 6；截图 `docs/phase6-snapshots.jpg` |
| ④ 二次确认 | 通过 | 两个删除入口都走 `ConfirmDialog`（danger），文案见上两行 |
| ⑤ 无回归 | 通过 | `acc5.mjs custom-fetch` 仍 `pickerOpened:true`（5 个 mock 模型）；shim 跑正文 `args.log` 记 `promptLines:4 / hasStreamJson:true / apiKeyInArgs:false`；删掉激活提供商后再跑提示「还没选择模型提供商，本次不注入任何环境变量」，把 `mock-gateway` 设回当前后又出现 `[storydesk] 环境注入：OPENAI_API_KEY, OPENAI_BASE_URL（provider: mock-gateway / 本地 Mock 网关）` |

**阶段 6 踩坑记录**：

1. **`Button` 没转发 ref，Popover 被摆到视口外**：`<PopoverTrigger asChild><Button/></PopoverTrigger>` 里 React 18 的函数组件拿不到 `ref`，Radix 量不到锚点，浮层落在 `(0, -215)` —— DOM 里有、截图和眼睛都看不见。改成 `forwardRef` 后 rect 变成 `(441, 88)`，正好挂在「快照」按钮下方。**任何 `asChild` 的 trigger 都必须是转发 ref 的组件**。
2. **确认弹窗是嵌套 dialog**：`ConfirmDialog` 渲染在设置弹窗**内部**，用「文本含『永久清除』」找弹窗会命中外层设置弹窗，把表格行里的「删除」当成确认按钮 → 必须取「文本最短的那个 dialog」。
3. **截带 Popover 的图要一条连接做完**：`Runtime.evaluate` 打开面板 + `Page.captureScreenshot` 分两次连接时面板会被关掉；`scripts/shot-popover.mjs` 用真实鼠标事件（`Input.dispatchMouseEvent`）点开面板，再用 `Page.startScreencast` 抓帧（WebView2 上 `captureScreenshot` 会让弹层先被关）。另外 trigger 是 toggle，**不能在截图前再点一次**，否则等于关掉。
4. **`localStorage["storydesk.lastWorkspace"]` 存的是裸路径字符串**（不是 JSON），`JSON.parse` 会报 `Unexpected token 'D'`。
5. **`Set-Content` 在本机会静默失败**（同阶段 5 第 4 条）：改 shim 的 `mode.txt`、改 `settings.json` 都用 `[System.IO.File]::WriteAllText` 或 Node 写，别用 PowerShell 文本管道。
6. **`delete_provider` 需要 `AppHandle`**：要定位 `settings.json` 就得拿 `app.path().app_config_dir()`，所以签名是 `delete_provider(app, provider_id)`（app 由 Tauri 注入，前端仍只传 providerId）；`delete_snapshot` 只收绝对路径，靠「路径里必须出现相邻的 `.storydesk` → `snapshots` 组件 + 后缀 `.md` + 必须是文件」做校验。

### 4.11 阶段 7 验收自测（智能体团队 / 动态 Prompt）

前置：`scripts\fixtures\claude-shim` 在 PATH 里，`mode.txt` 写 `ok`（否则智能体不会真的写文件）。

| # | 步骤 | 期望 |
|---|---|---|
| 1 | 打开项目 → 右栏「智能体团队」Tab | 六张卡片：📚资料收集员 / 🏗️大纲架构师 / 🔍对标拆解员 / ✍️主笔写手 / ⚙️设定管理员 / 🕵️连贯性追踪员 |
| 2 | 点「⚙️设定管理员」的「唤醒并操作」 | 自动切到「对话」Tab，输入框上方出现「当前聚焦目录：设定/ · 设定管理员」 |
| 3 | 点「🏗️大纲架构师」 | 提示变成「当前聚焦目录：大纲/ · 大纲架构师」，快捷指令换成「根据设定生成全书大纲 / 拆第一卷卷纲 / 生成第一章细纲」 |
| 4 | 引擎选 `Claude Code CLI` → 点「生成第一章细纲」 | 面板出现「启动方式：… 已解开 .cmd 包装」；跑完后 `大纲/细纲_第001章.md` 出现在**左侧文件树**（Watcher 自动刷新） |
| 5 | 看跑这条流水线时 `%TEMP%\storydesk-dev.err.log` | 有 `[storydesk] 智能体：大纲架构师（architect）` 与随后 10 行 prompt（含「角色约束」「目标目录 大纲/ 现有文件」） |
| 6 | 展开面板里的「送进引擎的 prompt」 | 前 6 行是智能体 system prompt，末尾**仍是原来的四行契约原文**（项目目录 / 任务包 / 先完整读取任务包… / 交付后只回…） |
| 7 | 切回「智能体团队」Tab | 刚才的卡片标着「操作中」，按钮变「继续指挥」 |

自动化：`node scripts/acc7.mjs team | wakeArchitect | wakeSettings | "send:architect:生成第一章细纲" | prompt | status | regression | "engine:claude"`

### 4.12 阶段 7 实测结论（2026-11-05）

| 验收项 | 实测 |
|---|---|
| 六张卡片 + Tabs | `acc7.mjs team` → `tabs: ["智能体团队","对话"]`、`cardCount: 6`；每张卡片文本形如 `🏗️大纲架构师architect负责：大纲/ · …自上而下搭结构…唤醒并操作` |
| 焦点目录联动 | `wakeSettings` → `当前聚焦目录：设定/· 设定管理员`；`wakeArchitect` → `当前聚焦目录：大纲/· 大纲架构师`；placeholder 同步为「告诉「大纲架构师」要做什么：例如「生成第一章细纲」」 |
| Prompt 注入（终端） | err.log 实录：`[storydesk] 智能体：设定管理员（settings）` → `你的核心职责是处理 设定/ 目录下的内容。` → `目标目录 设定/ 现有文件（共 3 个，先读需要的再动手）：` → `- 设定/全书梗概.md` `- 设定/文风.md` `- 设定/题材定位.md` |
| Prompt 注入（UI） | `acc7.mjs prompt` → 12 行；head 前 6 行为 system prompt，`tail` 末三行仍是契约原文 + `补充要求：生成第一章细纲`（`note` 追加在契约之后，四行本身没动） |
| 闭环落盘 | `send:architect:生成第一章细纲`（引擎 `claude` + shim `ok`）→ `大纲/细纲_第001章.md` 729 B 生成；左侧树 `hasOutline: true`（`大纲 / 大纲.md / 细纲_第001章.md`） |
| 无回归 | `acc7.mjs regression` → 引擎下拉 `mock` / `claude` 都在；`invoke('agent_specs')` 六项与 UI 卡片逐项比对 `mismatch: []`；`hasSend: true`、快捷指令 3 个 |

踩坑：

1. **`agent_specs` 是唯一权威，TS 别另写一套文案**：卡片上的 emoji/名称/目录都来自 Rust `models/ai.rs`，`lib/agents.ts` 只补 `kind` 与快捷指令；`acc7.mjs regression` 会拿两边的 role/name/dir 逐项比对，漂移会被抓出来。
2. **`AgentRole` 的 `as_str`/`parse` 不是死代码**：serde 自己处理（反）序列化，所以这俩必须有地方用——日志里打 `as_str()`、`agent_spec` 命令里用 `parse()`，否则 `cargo check` 会报 `never used`（本项目要求零警告）。
3. **上下文只发清单不发正文**：`lib/context.ts` 收的是「相对路径清单」（上限 40 条），后端再兜底扫目录。把正文塞进 prompt 会几十万字直接把上下文打满。
4. **切 Tab 后卡片不在 DOM 里**：Radix `TabsContent` 会卸载非活动内容，所以在「对话」页读 `[data-agent-role]` 得到 0 张卡片是正常的，验「操作中」状态要先 `openTab('team')`。
5. **shim 的章号要认中文数字**：prompt 里是「生成第一章细纲」，第一版只认 `\d+` 会落成 `细纲_第001章.md` 以外的错号；已补中文数字解析（`第一章 → 1`）。
6. **收尾记得把 `mode.txt` 调回 `block`**，并把 shim 目录从 PATH 里去掉，否则后续阶段的「阻断」验收会拿到 `ok` 模式的正常退出码。

### 4.13 阶段 7.5 验收自测（主从智能体 / 自定义角色）

前置：`C:\storydesk-shim` 已在 PATH 里，`mode.txt` 写 `ok`；引擎选 `Claude Code CLI`；工作区有书（`.active-book` 指向 `长篇/我的小说`）。

| # | 步骤 | 期望 |
|---|---|---|
| 1 | 设置 → 「智能体角色」 | 列出 6 个默认角色；每行有 Emoji、名称、单调小写 id、绑定目录 Badge；右侧是编辑表单（名称 / Emoji / 目录下拉 / 多行提示词） |
| 2 | 选中 `architect` → 改提示词 → 保存 | `master_agents` 里立即是新文本；跑一次派发后 `%TEMP%\storydesk-dev.err.log` 出现 `[storydesk] 角色提示词：[architect] <新文本>` |
| 3 | 「恢复默认 6 个角色」 | 列表回到出厂六项（`researcher/architect/benchmarker/writer/settings/tracker`） |
| 4 | 右栏面板 | **没有**「智能体团队 / 对话」双 Tab、**没有**卡片列表；只有一个「🧭 主 AI 总监」对话框 + 底部输入框 + 「下发任务」 |
| 5 | 输入「帮我写第一章正文」→ 下发 | 依次出现：用户气泡 → 「主 AI 正在思考… / 主 AI 正在拆解任务…」→ 计划卡（Badge 为「主 AI 规划」而不是「本地规则兜底」）→ 「📤 已将任务派发给 🏗️ 大纲架构师」→ 该步流式日志 → 「📤 已将任务派发给 ✍️ 主笔写手」→ 「2 个步骤全部执行完成。（2/2 步完成）」 |
| 6 | 看磁盘 | `大纲/细纲_第001章.md` 与 `正文/第001章_雨夜来客.md` 都落盘，左侧文件树自动刷新 |
| 7 | 写正文那一步故意删掉细纲再跑 | 派发卡里出现红线卡片，后续步骤不再派发（`master_run` 遇 `blocked` 直接 break） |

自动化：`node scripts/acc75.mjs panel | settingsRoles | "editPrompt:<文本>" | resetRoles | addRole | "send:<指令>" | "plan:<指令>" | regression | "engine:claude" | "remember:<工作区绝对路径>"`

截图：`node scripts/shot75.mjs master docs/phase7.5-master.png`、`node scripts/shot75.mjs roles docs/phase7.5-roles.png`

### 4.14 阶段 7.5 实测结论（2026-11-05）

| 验收项 | 实测 |
|---|---|
| 面板重构（验收 2） | `acc75.mjs panel` → `hasChat:true, hasInput:true, hasSend:true, tabs:[]`；标题 `🧭主 AI 总监待命…下属 6 个`；旧 `AgentTeamPanel/AiInput/ChatHeader` 已删 |
| 角色可定制（验收 1） | `acc75 "editPrompt:每章先写「本章卖点」，再列三个场景，钩子不超过两行。"` → `{before:"每章细纲先写「本章卖点」…", after:"每章先写「本章卖点」…", matched:true}`；随后 err.log 实录 `[storydesk] 角色提示词：[architect] 每章先写「本章卖点」，再列三个场景，钩子不超过两行。` |
| 主 AI 真的在规划（验收 3） | `master_plan` 返回 `source:"llm"`；err.log `[storydesk] 主 AI 规划：outcome=Some(Done { exit: 0 })，文本流 213 字`；计划卡 Badge「主 AI 规划 2 步」：`architect 生成第 1 章细纲：目标、冲突、转折、结尾钩子` → `writer 按第 1 章细纲写本章正文` |
| 拆解与派发可视化（验收 3） | `acc75 "send:帮我写第一章正文"` → `newEvents:8`、`kinds:[master-user, master-think, master-plan, master-think, master-dispatch, master-dispatch, master-done, user]`、`dispatchCount:2`、`done:true`；派发日志 `[storydesk] 主 AI 派发：[writer] 按第 1 章细纲写本章正文 → 写本章正文（第 2/2 步）` |
| 闭环落盘（验收 4） | `大纲/细纲_第001章.md` 2503 B、`正文/第001章_雨夜来客.md` 1734 B；每步都有「✅ 这一步完成（1.1s）」与「执行详情（读写了哪些文件）」 |
| 无回归 | 引擎下拉仍有 `mock`/`claude`；`npm run typecheck` 零错误；`cargo check` 零警告；`master_agents` / `master_default_agents` 正常返回 |

截图：`docs/phase7.5-master-plan.png`（思考 → 计划卡 → 第 1 步派发）、`docs/phase7.5-master.png`（第 2 步派发 → 全部完成）、`docs/phase7.5-roles.png`（角色管理台）。

踩坑（都是真踩出来的）：

1. **`#[serde(rename_all = "camelCase")]` 会把 `agent_id` 判死**：主 AI 返回的 JSON 是蛇形 `agent_id`，而 `rename_all` 只认 `agentId` → 反序列化失败 → 每次静默退化成 `rules_plan`，界面看起来「能用」。修法是给字段补 `#[serde(alias = "agentId", alias = "agent", alias = "agent_id")]`；诊断靠常驻日志「主 AI 规划：没解析出计划，原始输出：…」。
2. **假 CLI 在 Windows 管道上会丢最后几行**：`out(result)` 之后立刻 `process.exit(0)`，最后一条 stdout 常常没送到父进程 → 适配器看不到 `result`，`RunOutcome` 不是 `Done`。改成 `await finish(0)`（先 `sleep(150)` 再退出）。
3. **章号不能整轮共用一个**：前端 `chapter` 是残留值（实测是 2），两步派发都用它会让「写第 1 章」变成「写第 2 章」并被门禁拦住。现在每步从任务文本里解析 `第 N 章`（`chapter_in_task`），解析不到才用请求里的章号。
4. **主 AI 面板打开时设置可能还没加载** → 一直显示「下属 0 个」。加 `useEffect(() => { if (!settingsLoaded) void loadSettings(); })`。
5. **CDP 步骤不能混在一起跑**：`remember:` 会 `location.reload()`，紧随其后的步骤只会拿到 `null`；`engine:claude` 与 `send:` 写在同一条命令里，回显的是上一轮的事件。一次一条，中间留 `wait:`。
6. **截图前先「清空对话」**：否则截到的是历次运行叠加的事件流，无法当验收证据。


---

### 4.15 阶段 8 验收自测（人机协同 / 会话落盘 / 目录硬隔离 / 上下文传递）

前置同 4.13：`C:\storydesk-shim` 在 PATH 里、`mode.txt` 写 `ok`、引擎选 `Claude Code CLI`、工作区已打开且有书。

| # | 步骤 | 期望 |
|---|---|---|
| 1 | 输入「帮我写第一章正文」→ 「下发任务」 | 只出现用户气泡 + 思考行 + **计划确认卡**（`主 AI 规划 N 步` + 「✅ 确认执行」「✏️ 修改需求」+ 「不确认就什么都不动」）；此时**不该**有派发卡、不该有终端日志、不写任何文件 |
| 2 | 不点确认，等 10s | 仍然只有那张计划卡（`trace8` → `dispatchCount:0`、`hasDone:false`） |
| 3 | 点「✏️ 修改需求」 | 计划卡变成「已取消」，输入框里回填上一句原话，可以改完重发 |
| 4 | 重新下发 → 点「✅ 确认执行」 | 出现 `[系统] 已确认执行，开始派发 N 步。` → 逐张派发卡 + 流式日志 → 收尾卡 |
| 5 | 看 `%TEMP%\storydesk-dev.err.log` | 依次出现 `[storydesk] 第 1/2 步上下文文件（1 个）：大纲/大纲.md`、`[storydesk] 第 1 步产出：大纲/细纲_第001章.md`、`[storydesk] 第 2/2 步上下文文件（1 个）：大纲/细纲_第001章.md`；界面上有 `[系统] 正在将步骤 1 的产出『细纲_第001章.md』作为上下文传递给下一步…` |
| 6 | 关掉应用 → 重开 → 重新打开工作区 | 用户气泡、计划卡（「这份计划已执行（或已取消）」）、两张派发卡、`[系统]` 行、收尾卡全部还在；`.storydesk/chats/<sessionId>.json` 里是同一批消息 |
| 7 | 面板右上「历史对话」/「清空当前对话」 | 前者重新从磁盘加载；后者清空界面**并**把会话文件写成 `[]` |
| 8 | 把某个智能体的 `tools` 写上 `["Read","Write"]` 后再派活 | 该步 prompt 里出现 `【严格限制】你当前仅被允许修改 <dir>/ 目录下的文件。…` + `允许的工具：Read、Write。`，四行契约仍然原样在末尾 |
| 9 | `python scripts/check-dir-guard.py` | 4/4 通过：写入 `大纲/` 放行，写 `正文/`、`设定/`、`../../../evil.md` 一律 `exit 2` + `⛔ 目录越权：… 已拦截。` |
| 10 | 回归 | 引擎下拉仍有 `mock`/`claude`；`master_agents` 仍是 6 个；`master_execute` 传空 steps 会报「计划是空的：先在对话流里生成一份计划，再点「确认执行」」；门禁拦截卡片照旧出现 |

自动化：`node scripts/acc8.mjs panel8 | trace8 | session | "planOnly:<指令>" | confirm | edit | reload8 | clear8 | regression8 | "remember:<工作区绝对路径>"`

截图：`node scripts/shot8.mjs plan docs/phase8-plan.png`、`shot8.mjs flow docs/phase8-flow.png`、`shot8.mjs context docs/phase8-context.png`、`shot8.mjs historytop docs/phase8-history-top.png`、`shot8.mjs history docs/phase8-history.png`

### 4.16 阶段 8 实测结论（2026-11-05）

| 验收项 | 实测 |
|---|---|
| 计划确认生效（验收 1） | `acc8 "planOnly:帮我写第一章正文"` → `kinds:[master-user, master-think, master-plan]`、`pending:"1"`、`hasConfirm/hasEdit:true`、`dispatchCount:0`、`done:false`；等 10s 后 `trace8` 仍是 `dispatchCount:0, hasDone:false`。点确认后 → `[master-system] 已确认执行，开始派发 2 步。` + 2 张派发卡 + 收尾卡 |
| 会话持久化（验收 2） | `.storydesk/chats/s-mw32s40a-gncw.json` = 2493 B、7 条消息，`roles: [user, master, system, agent, system, agent, master]`；**杀掉进程重开 + 重新打开工作区**后 `shot8 history` → `user:1, plans:1, dispatches:2, doneCards:1`，`historytop` → 第一句原话「帮我写第一章正文」与计划卡「这份计划已执行（或已取消）」都在 |
| 上下文传递（验收 3） | err.log 实录四行（见 4.15 第 5 条）；落盘 `大纲/细纲_第001章.md` 2503 B、`正文/第001章_雨夜来客.md` 1734 B；第二步 prompt 的文件清单里确实是 `- 大纲/细纲_第001章.md` |
| 工具白名单（模块三） | 给 `writer` 写 `tools:["Read","Write"]` 后，`C:\storydesk-shim\args.log` 里那次调用的 prompt 含 `【严格限制】你当前仅被允许修改 正文/ 目录下的文件。严禁直接修改其他目录的内容，否则将被系统拦截。` 与 `允许的工具：Read、Write。除此之外不要调用其他工具。`；prompt 共 16 行，末尾仍是四行契约 + `补充要求：按第 1 章细纲写本章正文`（验完已把 `tools` 还原成空） |
| 目录硬隔离（模块三·脚本侧） | 书根 `.claude/hooks/guard-agent-dir.{sh,py}` 已写入，`.claude/settings.json` 的 `hooks.PreToolUse`（matcher `Write|Edit|MultiEdit`，`bash .claude/hooks/guard-agent-dir.sh`）幂等合并；`python scripts/check-dir-guard.py` → 允许 `大纲/细纲_第001章.md`（0），拦 `正文/`（2，`⛔ 目录越权：正文\第001章_雨夜来客.md 不在 大纲/ 下，已拦截。`）、拦 `设定/`（2）、拦 `../../../evil.md`（2） |
| 无破坏（验收 4） | `acc8 regression8` → `engineOptions:["mock","claude"]`、`agentCount:6`、`agentIds` 六项、`hasHistoryButton:true`、`emptyStepsGuard:"计划是空的：…"`；跑 mock 引擎那一轮仍出现「⛔ 缺少前置文件 / 第 2 章缺少细纲」的门禁卡；`npm run typecheck` 零错误、`cargo check` 零警告、命令总数 48 |

截图：`docs/phase8-plan.png`（计划确认卡 + 两个按钮 + 「不确认就什么都不动」）、`docs/phase8-context.png`（第 1 步派发卡 → `[系统] 正在将步骤 1 的产出『细纲_第001章.md』作为上下文传递给下一步…` → 第 2 步派发卡）、`docs/phase8-flow.png`（收尾「2 个步骤全部执行完成。（2/2 步完成）」）、`docs/phase8-history-top.png` / `docs/phase8-history.png`（重开应用后的历史还原）。

踩坑（本阶段新踩的）：

1. **计划卡和收尾卡会互相覆盖**：落盘时两者都用同一轮的 `runId` 当消息 id → `append` 的「同 id 覆盖」让历史里只剩其中一条（实测 5 条里少了收尾）。现在计划落盘用 `plan-<runId>`，收尾用 `<runId>`，`toEvent` 还原时去掉前缀。
2. **并发落盘会丢消息**：派发卡与收尾几乎同时到，`chat_store::append` 是「读全量 → 改 → 整份写回」，两个线程同时进来就会互相覆盖（实测 7 条只存在 5 条）。改用进程级 `static CHAT_LOCK: Mutex<()>` 串行化读-改-写，并把实际写盘抽成不加锁的 `write_raw`（`append` 自己持锁，避免二次加锁死锁）。
3. **重开后待确认的计划变成死卡**：只拆了计划、没点确认就关软件，重开时 `masterPlan` 是 `null`，卡片上没有按钮。现在 `loadChat` 里若「最后一张计划晚于最后一张收尾」就把它重新装回 `masterPlan`，卡片继续能点。
4. **用 PowerShell 管道喂 JSON 给 python 钩子会把中文变成乱码**：`'{"tool_input":{"file_path":"D:\…\设定\人物.md"}}' | python guard-agent-dir.py` 实测 `exit 0`（看着像放行），实际是 stdin 的 JSON 解码失败触发了 fail-open。验证脚本要像 `scripts/check-dir-guard.py` 那样在 Python 内部构造载荷、用 `subprocess` 传 stdin。
5. **计划卡会累加**：`document.querySelector('[data-testid="master-plan"]')` 拿到的永远是第一张旧卡，CDP 步骤必须取 `cards[cards.length-1]`。
6. **`remember:` 会 `location.reload()`**：必须单独一条命令跑，后面紧跟别的步骤只会回 `null`（等 6~9s 让它重连）。

---

### 4.17 阶段 8.1 移除内置 Mock 引擎（2026-11-05）

**改了什么**：应用只剩一种执行引擎——宿主 `claude` CLI。原来那个「不调外部程序、每 100ms 抛一段假 chunk」的 `MockAdapter` 连同它的 `Step`/`text()`/`status()`/`read_file()` 辅助函数、`MOCK_TICK_MS`、`MOCK_BLOCK_STDERR` 一起删掉了（`CANCEL_MARK` 保留，`ai_cancel` / `master_run` 靠它认「已取消」）。

| 位置 | 改动 |
|---|---|
| `src-tauri/src/models/dto.rs` | `EngineKind` 只剩 `Claude` 一个变体；`Deserialize` 改手写——读到 `"mock"` 或任何别的字符串都按 `claude` 处理并在 stderr 记一行 `[storydesk] 引擎配置写的是「mock」，已不再支持，按 claude 处理`，**不让一份过时配置把整个设置读崩** |
| `src-tauri/src/models/settings.rs` | `EnginePref` 的默认值改成 `"claude"`；`normalize()` 里把任何非 `"claude"` 的 `engine.kind` 改写回 `"claude"`（老 `settings.json` 打开一次就自动修正） |
| `src-tauri/src/services/engine.rs` | 删 Mock 适配器；`build()` 不再 match 分支，恒定返回 `ClaudeCodeAdapter`，缺 CLI 时的提示改成「应用只用 claude CLI 这一种引擎」；`RunCtx` 里 `workspace_root/task_packet/kind/chapter` 成了「只带不读」的信封字段，加了 `#[allow(dead_code)]` 并注明理由 |
| `src-tauri/src/commands/ai.rs` | `ai_engines` 只返回一条 `Claude Code CLI`（`available` 取决于 `which claude`）；`unwrap_or(EngineKind::Mock)` → `EngineKind::Claude`；「未检测到 storyctl.py，跳过前置门禁（Mock 联调可忽略）」去掉 Mock 字样 |
| `src-tauri/src/commands/master.rs` | 两处 `unwrap_or(EngineKind::Mock)` → `Claude` |
| `src/types.ts` / `src/store/aiStore.ts` / `src/store/settingsStore.ts` | `EngineKind = "claude"`；`engine` 初值 `"claude"`；`loadEngines` 不再「挑一个能用的」，直接 `set({ engines, engine: "claude" })`；`placeholder()` 的默认引擎也改成 `claude` |
| `src/components/ai/AiPanel.tsx` | 顶部那个引擎 `<select>` 整个删掉，换成只读文本 `⚙️ Claude Code CLI`（`data-testid="ai-engine"` 保留，`title` 带 CLI 路径；未探测到时补一个「未检测到」徽标） |

**守卫没动**：`dir_guard.rs` 的 PreToolUse hook 与 `check_writes` 一行没删，仍然只在跑 CLI 的子进程里生效（`master_run` 每步 `dir_guard::ensure` + 注入 `STORYDESK_AGENT_DIR`）。

**顺手修的真 bug（这次验证时撞出来的）**：`guard-agent-dir.py` 原来用 `sys.stdin.read()` 读 hook 载荷——Windows 控制台默认 cp936，载荷里带中文路径时这一步就抛 `UnicodeDecodeError` → JSON 解析失败 → 走 fail-open 放行（测试脚本里看到的是 `exit=0`，像「守卫没生效」）。现在改成 `sys.stdin.buffer.read()` 后显式按 UTF-8 解码（失败再退回文件系统编码兜底）；stderr 也改成 `sys.stderr.buffer.write(...utf-8...)`，免得中文写不出去把退出码从 2 变成 1。

**实测**：

| 验收项 | 结果 |
|---|---|
| 编译 | `npm run typecheck` 零错误；`cargo check --target x86_64-pc-windows-gnu` → `Finished dev profile`，零警告（`RunCtx` 那 4 个字段的 dead_code 用 `#[allow(dead_code)]` 注掉，不是靠 `-A warnings`） |
| 界面无 Mock | `node scripts/acc8.mjs ui8` → `engineText: "⚙️Claude Code CLI"`、设置页引擎区只有一行 `Claude Code CLI / 可用 / 当前 / C:\storydesk-shim\claude.cmd（将注入 OPENAI_API_KEY（provider: work））`、`hasMockWord: false`、`hasMockOption: false` |
| 功能正常 | `node scripts/acc8.mjs "planOnly:帮我写第一章正文"` → 计划卡「主 AI 规划 2 步」`pending:"1"`、`dispatchCount:0`；`confirm` 后 2 张派发卡 + `master-done`，第一步真写盘 `大纲/细纲_第001章.md`，日志仍是 `启动方式：E:\Program Files\nodejs\node.exe + C:\storydesk-shim\fake-claude.mjs（已解开 .cmd 包装）` |
| 守卫有效 | `python scripts/check-dir-guard.py` → 4/4：`大纲/` 放行（0）、`正文/`（2）、`设定/`（2）、`../../../evil.md`（2），stderr 逐字 `⛔ 目录越权：正文\第001章_雨夜来客.md 不在 大纲/ 下，已拦截。` |
| 回归 | `node scripts/acc8.mjs regression8` → `engineOptions:["claude"]`、`engineNames:["Claude Code CLI"]`、`hasSelect:false`、`agentCount:6`、`agentIds` 六项、`hasHistoryButton:true`、`emptyStepsGuard:"计划是空的：先在对话流里生成一份计划，再点「确认执行」"` |

> 4.x 前面那几张表里出现的 `Mock 引擎（内置）` / `engineOptions:["mock","claude"]` 都是**当时的实测记录**，不是现在的行为；现在引擎列表只有 `claude`。

---

### 4.18 阶段 9 工具级授权 + 计划可编辑（2026-11-05）

**三件事**：①「允许的工具」从写死的 `Read/Write` 变成设置里的勾选清单，勾什么就写进 prompt 的「允许的工具」一行；②计划卡在确认前可以就地改步骤文本、删步骤，也可以取消；③假 CLI 的章号不再写死 `第001章`（否则「写第 2 章正文」第二步永远被门禁拦下）。

| 位置 | 改动 |
|---|---|
| `src-tauri/src/models/ai.rs` | 新增 `TOOL_CHOICES`（Read/Write/Edit/MultiEdit/Bash/Glob/Grep/WebFetch）、`default_tools()`、`canonical_tool()`、`normalize_tools()`（丢未知项、去重、**按 TOOL_CHOICES 排**→ 同配置在任何机器上拼出的 prompt 逐字一致）、`effective_tools(agent, global)`（角色自己的 `tools` 优先 → 全局清单 → 都没有才用默认两项） |
| `src-tauri/src/models/settings.rs` | `AppSettings.allowed_tools: Option<Vec<String>>`（`#[serde(default)]`，缺省 `["Read","Write"]`），`normalize()` 里过一遍 `normalize_tools` |
| `src-tauri/src/services/engine.rs` | `compose_agent_prompt(agent, tools, context_files, contract)`：多一个 `tools` 参数；`【严格限制】` 那段不再受「角色填了 tools 才出现」的条件控制，恒定输出；`tools` 为空时写「允许的工具：（设置里一个都没勾选）只输出文字说明，不要读写任何文件，也不要执行命令。」 |
| `src-tauri/src/commands/{ai,master}.rs` | `apply_agent_prompt` 多收一个 `allowed_tools`；`master_run` 每步算一次 `effective_tools` 并打日志 `[storydesk] 第 i/N 步允许的工具：…` |
| `src/components/settings/AgentRolesTab.tsx` | 顶部新增 `ToolsPanel`（`data-testid="tool-whitelist"`）：八个复选框，勾选即落盘（没有「保存」按钮），标题带数量，一个都没勾时给一条警示徽标 |
| `src/components/ai/MasterChat.tsx` | 计划卡在 `pending` 时三个按钮：`✅ 确认执行` / `✏️ 编辑计划` / `🚫 取消计划`。编辑态每步一个 `<textarea>` + 删除按钮（只剩一步时禁用），底部「保存并执行」`plan-save`；确认过/取消过的卡只剩一行说明 |
| `src/store/aiStore.ts` | `confirmMaster(steps?)`：传了 steps 就换一个新 `runId`（`<原>-e<时间戳>`），历史里的旧计划卡不会被新一轮的派发/收尾覆盖；`editMasterRequest` 顺手把那张还没执行的计划卡从对话流里摘掉，`loadChat` 也按「取消过又没执行」过滤，重开不会留一张点不动的孤儿卡 |
| `src/components/settings/SettingsDialog.tsx` | 引擎区去掉「当前」胶囊（只剩一项），改一行说明「应用只用这一种引擎（阶段 8.1 已移除内置 Mock）」；`aiStore.setEngine` 一并删掉 |
| `scripts/fixtures/claude-shim/fake-claude.mjs` | 章号改成「先看 `补充要求：` 那行，没有才退回全文第一个『第 N 章』」——之前上下文文件清单里躺着 `细纲_第001章.md`，先匹配到它就永远写第 1 章，第二步必被门禁拦；同时把 prompt 里「允许的工具」一行记进 `args.log` 的 `allowedLine` |

**实测**：

| 验收项 | 结果 |
|---|---|
| 编译 | `npm run typecheck` 零错误；`cargo check --target x86_64-pc-windows-gnu` → `Finished dev profile`，零警告（新增 4 个单测：白名单归一 / 角色覆盖全局 / 勾掉 `Write` 后 prompt 里没有 `Write` / 空清单提示） |
| 工具面板 | `node scripts/acc8.mjs tools` → `hasPanel:true`、标题「允许调用的工具（2 个）」、`Read`/`Write` 勾选，其余六项未勾，`allowedInSettings:["Read","Write"]` |
| 勾选生效 | `acc8.mjs toolsOff:Write` → `allowedInSettings:["Read"]`；跑一轮后 `args.log` 里子智能体 prompt 的 `allowedLine` 是 `允许的工具：Read。除此之外不要调用其它工具。`，dev 日志 `[storydesk] 第 1/2 步允许的工具：Read`；勾回去变成 `允许的工具：Read、Write。` |
| 计划编辑 | `acc8.mjs planEdit` → 打开编辑态（2 个 textarea + 保存并执行），删掉第 2 步、改写第 1 步后保存 → 系统行「已确认执行（改过的 1 步）」、`dispatchCount:1`、收尾「1 个步骤全部执行完成」，旧卡标记「这份计划已执行（或已取消）」 |
| 计划取消 | `acc8.mjs planCancel` → `cardsBefore:1`、`cardsAfter:0`、输入框回填原话「检查伏笔与时间线」、系统行「已取消这份计划，改完需求再下发。」 |
| 章号动态 | `acc8.mjs "planOnly:写第2章正文"` + `confirm` → 步骤 1 写 `大纲/细纲_第002章.md`、步骤 2 写 `正文/第002章_雨夜来客.md`，两步都完成，没有再出现「第 2 章缺少细纲」；磁盘核对两份都是 2503 B / 1734 B |
| 回归 | `acc8.mjs regression8` → `engineOptions:["claude"]`、`hasSelect:false`、`agentCount:6`、`emptyStepsGuard` 文案不变 |

截图：`docs/phase9-plan-edit.png`（计划卡编辑态：两个文本框、两个 ✕、保存并执行/收起）、`docs/phase9-tools.png`（设置 → 智能体角色 → 允许调用的工具）。

**踩坑**：`acc8.mjs` 里 `toolsOn:`/`toolsOff:` 的前缀长度不一样，用 `step.slice("toolsOff:".length)` 会把 `Write` 切成 `rite`（报「设置里找不到工具勾选框：rite」）；打开设置到「允许的工具」面板可见之间要等 Radix 的 tab 切换，脚本里抽成了 `OPEN_TOOLS` 常量，两步都先确保面板可见再点。

---

### 4.19 阶段 10 意图路由 + 全局上下文注入 + 夹具意图识别（2026-11-05）

**三个毛病一起修**：①「检查伏笔与时间线」被派给了 `architect`，它反手一个 `Write` 把 `大纲/细纲_第001章.md` 覆盖了，检查动作一次没做；②写第 2 章正文时细纲里写着「2400-2600 字、对白四成、环境两成」，正文一句没照办——因为 `context_files` 只传上一步产出，`设定/`、`追踪/`、`参考资料/` 从来进不了 prompt；③假 CLI 不看任务类型，无脑写文件，所以检查类的走查永远测不出来。

| 位置 | 改动 |
|---|---|
| `src-tauri/src/services/context.rs`（新） | `global_context(book, chapter, skip)`：本章细纲 → `CORE_FILES`（`设定/世界概览.md`、`设定/角色状态.md`、`设定/人物.md`、`设定/文风.md`、`追踪/时间线.md`、`追踪/伏笔.md`）→ `大纲/大纲.md` → 扫 `设定/`、`追踪/`、`参考资料/` 各一层，去重后截到 `GLOBAL_LIMIT=12`；`hard_constraints(book, chapter)`：读本章细纲，正则 `(\d{3,4})\s*[-–—~～至到]\s*(\d{3,4})\s*字` 变「本章字数必须在 2400-2600 字之间」，再把「对白占四成」「环境描写不超过两成」拼成一句；`outline_path()` 兼容 `细纲_第3章.md` / `细纲_第003章.md` |
| `src-tauri/src/models/ai.rs` | `is_read_only_task(task)`：含 `【只读模式】` 恒真；含 写/生成/补/改写/润色/保存/新建/落盘/扩写 恒假；否则看 只读/检查/核对/分析/盘点/审查/梳理/列出/报告/评估/对账。`read_only_tools(tools)`：摘掉 `Write`/`Edit`/`MultiEdit` |
| `src-tauri/src/services/engine.rs` | `compose_agent_prompt(agent, tools, context_files, global_files, read_only, contract)`：只读时在角色名后插一段「【只读模式】不要调用 Write 工具，只输出报告」；全局清单以「动笔前，必须先阅读以下全局设定与大纲要求（它们是全书的硬约束，不要凭印象写）」开头，排在目标目录清单**之前**；`【严格限制】`＋工具行＋四行契约仍在最后。`compose_master_prompt` 重写：新增「职责边界（越界即算失败）」六行 + 「先判断需求类型」三条（生成类 / 检查类只读且优先派 tracker、task 必须以 `【只读模式】` 开头 / 拿不准只派一步）+ 输出要求 6 条（细纲已存在就别重复生成） |
| `src-tauri/src/commands/{ai,master}.rs` | `apply_agent_prompt` 多收 `chapter` 与 `task`：按 task 判只读并摘工具、注入全局清单；`ai_start` 的正文任务把 `hard_constraints` 接到 note 末尾。`master_run` 每步打三行日志：`第 i/N 步全局上下文（k 个）`、只读判定、`第 i/N 步硬约束`。`rules_plan` 的检查分支**直接返回一步**：派给最贴题的 `tracker`/`settings`/`benchmarker`/`researcher`，task 统一写成 `【只读模式】…：只输出报告，不要调用 Write 工具，不要修改任何文件` |
| `scripts/fixtures/claude-shim/fake-claude.mjs` | 主 AI 分支按 prompt 意图出计划（检查类 → 一步 tracker 只读；prose → architect + writer；其余 → architect）；子智能体分支只读时只吐「## 检查报告（只读）」并 `result: "已生成检查报告（只读，未修改任何文件）"`，**一次 Write 都不调**；`目标目录=追踪` 且非只读 → 写/追加 `追踪/时间线.md`；正文文件顶部带 `> [此处模拟生成了 2500 字]（字数区间 2400-2600 字；对白约四成，环境描写约两成）`；`args.log` 增记 `readOnly`、`targetDir`、`globalLines`、`constraintLine`、`extraLine` |

**实测**（工作区 `D:\桌面\大制作\小说\验收示例`，书根 `长篇\我的小说`）：

| 验收项 | 结果 |
|---|---|
| 编译 | `npm run typecheck` 零错误；`cargo check --target x86_64-pc-windows-gnu` → `Finished dev profile` 零警告（新增单测：只读判定 / 全局清单排序与 skip / 约束抽取 / 只读 prompt 里没有 `、Write` / 检查类派 tracker） |
| 检查类路由 | `acc8.mjs "planOnly:检查伏笔与时间线"` → 计划卡 `1. tracker 【只读模式】检查伏笔与时间线：只输出报告，不要调用 Write 工具，不要修改任何文件`，`pending:"1"`；`confirm` → `dispatchCount:1`，面板出现「## 检查报告（只读）」「结论：伏笔已核对，暂无冲突。以上为报告，未修改任何文件。」「已生成检查报告（只读，未修改任何文件）」；`大纲/细纲_第001章.md` 的 `Length/LastWriteTime` 仍是 `2503 / 15:50:03`，工作区一个文件都没多 |
| 全局上下文注入 | `planOnly:写第2章正文` + `confirm` 后 `args.log` 里 writer 那行：`global:["- 设定/文风.md","- 大纲/大纲.md","- 设定/全书梗概.md","- 设定/题材定位.md","- 大纲/细纲_第002章.md"]`、`constraint:"2400-2600 字"`、`extra:"补充要求：按第 2 章细纲写本章正文；本章字数必须在 2400-2600 字之间，对白占四成，环境描写不超过两成。"`；dev 日志对应 `第 2/2 步全局上下文（4 个）：…`、`第 2/2 步硬约束：本章字数必须在 2400-2600 字之间，对白占四成，环境描写不超过两成。`；落盘正文首行是 `> [此处模拟生成了 2500 字]（字数区间 2400-2600 字；对白约四成，环境描写约两成）` |
| 回归 | `acc8.mjs regression8` → `engineOptions:["claude"]`、`hasSelect:false`、`agentCount:6`、`emptyStepsGuard` 文案不变；`python scripts/check-dir-guard.py` → 4/4（`大纲/` exit 0；`正文/`、`设定/`、`../../../evil.md` 均 exit 2 并抛 `⛔ 目录越权：… 不在 大纲/ 下，已拦截。`） |

截图：`docs/phase10-check.png`（检查类任务：计划卡只读 + 「## 检查报告（只读）」+「✅ 这一步完成」，没有任何写入动作）、`docs/phase10-prose.png`（写正文：两张派发卡 + 系统行「正在将步骤 1 的产出『细纲_第002章.md』作为上下文传递给下一步…」+「已写入 正文/第002章_雨夜来客.md」）。

**踩坑**：①假 CLI 的 `readOnly` 一开始把主 AI 规划那一行也标成 `true`——主 AI 自己的 prompt 里就有「检查类任务必须以【只读模式】开头」这句话，改成「prompt 里出现『主 AI 总监』就不算只读」；②`硬约束` 只在 `RunKind::Prose` 的步骤上追加，检查类步骤不该出现字数要求；③PowerShell 5.1 没有 `Out-File -Encoding utf8NoBOM`（ValidateSet 直接报错），要不定 `utf8`、要不直接用编辑器写文件。

---

### 4.20 阶段 11 意图判定重写 + 任务清洗 + 角色边界（2026-11-05）

**出问题的现场**：作者输入「把设定里的几个文件内容给我写了，要写一本古代的农民起事的小说」，主 AI 拆出来的唯一一步是 `architect 给出可执行的第一步：<整句原话>`，架构师为了交差直接 `Write`，把 `大纲/细纲_第001章.md` 覆盖成 951 字节。

**根因**：`rules_plan` 只认 `["正文","写第","写一章","这一章","本章","下一章"]` 这六个词判「写正文」；「写一本…小说」一个都不中，最后掉进兜底分支，把作者原话当任务派了出去。关键词表缺失 + 兜底分支会派给一个「能写文件」的角色 = 原话被当细纲。

| 位置 | 改动 |
|---|---|
| `src-tauri/src/services/intent.rs`（新） | 意图判定收进一个模块：`Kind`（Prose/Check/Outline/Settings/Benchmark/Research/Unknown）、`Confidence`（High/Low）、`Intent { kind, confidence, chapter, genre, subject, whole_book, hits }`。权重表 `KEYWORDS`：写正文 3 分（正文/落盘/成稿/扩写/码字/写一章/写一版/写一本/写一部/写小说/写故事/开写/写第）、排大纲 3 分（细纲/大纲/卷纲/章纲）、只读检查 3 分（检查/核对/盘点/梳理/评估/审查/对账/伏笔/时间线/欠账/前后矛盾）…判定顺序：①检查分 ≥3 且压过其它意图、无强动笔词、正文分 0 → 只读检查；②否则按权重排序（平手 Prose > Check > Outline > Settings > Benchmark > Research）；③整本 + 设定 + 有正文分 → 正文；④零命中 → `Unknown/Low`。`step_task()` 把意图翻译成**可执行任务**（不是原话）：`按第 N 章细纲写本章正文（题材：X）`、`生成第 N 章细纲：目标、冲突、转折、结尾钩子（已有细纲就沿用，不要覆盖）`、`【只读模式】…：只输出报告，不要调用 Write 工具，不要修改任何文件`。`describe()` 产出卡片上那句「我理解为」。`clean_subject()` 剥掉「把设定里的几个文件内容给我写了」这类口语壳子 |
| `src-tauri/src/commands/master.rs` | `rules_plan` → `plan_locally()`：按 `Kind` 派活，写正文先看 `context::outline_path()` 有没有细纲，**没有才先派架构师**，然后再派写手；检查类派 `check_dir()` 挑出的角色并强制只读。`describe_request()`：优先按作者原话判定意图，原话判不出来才退到「第一步派给了谁」（`intent_hint`，按 agent id 推断——模型写的 task 里常有「冲突」「分析」这类词，用关键词判会误读）。`MasterPlan.intent` / `master://plan` 事件 / 落盘消息都带上这句意图 |
| `src-tauri/src/models/ai.rs` | `AgentConfig` 之外新增角色边界说明书由 `engine::role_boundary()` 拼：架构师只负责 `大纲/`、禁止写 `正文/` 与 `设定/`、**禁止把整句需求原样当细纲**（「只把需求复述成一句『细纲』等于没做」）；写手只负责 `正文/` 不改大纲；追踪者默认只读 |
| `src-tauri/src/services/engine.rs` | `compose_master_prompt` 的「先判断需求类型」改成 0~6 编号优先级表（0 只读类先判 / 1 写正文类 / 2 排大纲 / 3 补设定 / 4 对标 / 5 找资料 / 6 一个词都没命中只派一步），并加两条硬要求：「task 必须是可执行的指令，不许把作者原话直接塞进去」（附改写示例）、「一句话只能有一个意图」 |
| `src-tauri/src/models/{dto,chat}.rs` | `MasterPlan.intent`、`MasterPlanEvent.intent`、`ChatMessage.plan_intent`（跟着会话落盘，重开还能看到「我理解为」） |
| 前端 | `MasterChat.tsx` 的计划卡在标题下面加一行 `data-testid="plan-intent"`：`我理解为：<intent>`；`aiStore` 把 `intent` 一路带进 `PendingPlan`、落盘消息、`loadChat` 重建 |
| `scripts/fixtures/claude-shim/fake-claude.mjs` | 主 AI 分支按新规则出计划（检查 → 一步只读；细纲/大纲 → 一步架构师；写正文 → 架构师+写手；拿不准 → 一步只读）。子智能体分支新增「职责边界」：任务正文里没有「细纲/大纲/卷纲」就**明确拒绝写入**（`result: "已拒绝写入：这一步不是细纲任务"`）；细纲已存在则**沿用不覆盖**。`args.log` 增记 `taskLine` 与 `master-plan` 那一行（`want/subject/genre/chapter/prose/plan`） |

**实测**（工作区 `D:\桌面\大制作\小说\验收示例`）：

| 输入 | 计划卡 |
|---|---|
| `把设定里的几个文件内容给我写了，要写一本古代的农民起事的小说` | 我理解为：**写正文 · 正文/ · 章号待定 · 题材「古代的农民起事」 · 整本从头写**；`1. architect 生成第 1 章细纲：…（已有细纲就沿用，不要覆盖）`、`2. writer 按第 1 章细纲写本章正文（题材：古代的农民起事）` |
| `检查伏笔与时间线` | 我理解为：只读检查 · 追踪/ · 只读不改文件；`1. tracker 【只读模式】检查伏笔与时间线：只输出报告，不要调用 Write 工具，不要修改任何文件` |
| `生成第3章细纲` | 我理解为：排大纲 / 细纲 · 大纲/ · 第 3 章；`1. architect 生成第 3 章细纲：…` |
| `帮我写第一章正文` | 我理解为：写正文 · 正文/ · 第 1 章；architect（细纲）+ writer（正文） |

| 验收项 | 结果 |
|---|---|
| 编译 | `npm run typecheck` 零错误；`cargo check --all-targets --target x86_64-pc-windows-gnu` → `Finished dev profile` 零警告（新增单测：`novel_writing_request_is_prose_not_settings`、`write_first_chapter_is_prose_with_chapter`、`chapter_outline_request_routes_to_outline`、`check_request_is_read_only`、`strong_write_beats_check_words`、`vague_request_is_low_confidence`、`clean_subject_strips_the_ask_shell`、`rules_plan_reads_novel_request_as_prose`、`rules_plan_keeps_one_intent_per_round`） |
| 首页那句话不再覆盖大纲 | `probe:` 四例的计划如上表，`dispatchCount:0`（不确认不派发）；`confirm` 后架构师那步输出「`大纲/细纲_第001章.md` 已经在了，沿用现成的，不覆盖。」`result: 沿用了 大纲/细纲_第001章.md（未覆盖）`，写手那步才写 `正文/第001章_雨夜来客.md` |
| 任务清洗 + 全局上下文 + 硬约束 | `args.log` 里写手那行：`taskLine:"按第 1 章细纲写本章正文（题材：古代的农民起事）；本章字数必须在 2400-2600 字之间，对白占四成，环境描写不超过两成。"`、`global:["- 大纲/细纲_第001章.md","- 设定/文风.md","- 大纲/大纲.md","- 设定/全书梗概.md","- 设定/题材定位.md"]`、`constraint:"2400-2600 字"`；全程没有出现「把设定里的几个文件内容给我写了」这串原话 |
| 架构师拒绝越界 | `node scripts/_check-architect-reject.mjs`（直接喂假 CLI 一段伪 prompt）→ `{"exit":0,"result":"已拒绝写入：这一步不是细纲任务","wroteAnything":false,"outlineUntouched":true}` |
| 回归 | `acc8.mjs regression8` 全通过（`engineOptions:["claude"]`、`agentCount:6`）；`python scripts/check-dir-guard.py` → 4/4（`大纲/` exit 0，`正文/`、`设定/`、`../../../evil.md` exit 2） |

截图：`docs/phase11-intent.png`（计划卡：`拆解出的计划 / 主 AI 规划 / 2 步` + `我理解为：写正文·正文/·章号待定·题材「古代的农民起事」·整本从头写` + 两步任务 + `✅ 确认执行 / ✏️ 编辑计划 / 🚫 取消计划 / 不确认就什么都不动`）。

**踩坑**：①把「冲突」「分析」这类词也给「检查」加了权重后，`生成第 1 章细纲：目标、冲突、转折` 被判成只读检查（意图串错、计划卡只显示一支笔）→ 短路条件收紧成「检查分 ≥3 且 ≥ 其它意图最大值」；②模型给的计划没有 intent，一开始拿第一步 task 去 `classify` 反推，同样被「冲突」带偏 → 改成按「派给了谁」推断，原话可判时优先用原话；③假 CLI 的 `prompt.includes("细纲")` 判断永远为真（角色边界那段里就写着「章细纲」），必须改成只看 `补充要求：` 那一行；④`cargo` / `npm` 不在默认 PATH，每次都得先挂 `C:\Users\20113\.cargo\bin`、`E:\Program Files\nodejs`，并把 `CARGO_TARGET_DIR` 指到 `C:\storydesk-build`。

---

### 4.21 阶段 11.5 题材一致性 + 新书启动 + 设定不足拦截（2026-11-05）

**阶段 11 的残留**：计划卡判对了（「写正文 · 题材『古代的农民起事』」），可产物还是上一本书——架构师说「细纲已存在，沿用」，写手照着旧细纲写 `正文/第001章_雨夜来客.md` 672 字节。也就是说：**意图判对了，但没人检查「现有细纲属于哪本书」**。

| 位置 | 改动 |
|---|---|
| `src-tauri/src/services/context.rs` | 新增 `check_outline(book, chapter, genre) -> OutlineCheck { exists, reusable, reason }`：读 `大纲/细纲_第00X章.md` + 设定里的题材定位/全书梗概/世界概览，用 `topic_tokens()` 把题材切成两字组合（「古代的农民起事」→ 古代/代的/的农/农民/民起/起事），一个都不命中就判**不可复用**（`现有细纲与题材「X」不符，按新题材重写`）。新增 `setting_health()`：扫 `设定/`，缺核心文件或总量 < 400 字（去空白）判 `thin`。`hard_constraints()` 不变，仍从细纲抽字数区间与对白比例 |
| `src-tauri/src/services/intent.rs` | 三个任务模板：`writer_task()`（题材 + 「必须先完整阅读 设定/…」+ 文件命名规则「第00X章_<与本章相关、不超过八字的小标题>.md」+ 硬约束）、`outline_task(regenerate)`（`regen=true` 时写「旧细纲的人物与情节一律作废，不要沿用」）、`settings_bootstrap_task()`（按题材补 `世界概览.md` 与 `人物.md`） |
| `src-tauri/src/services/engine.rs` | `compose_agent_prompt(..., read_only, topic, contract)` 新增 `topic`：在【严格限制】之前插一段**最高优先级**的「【题材一致性 · 最高优先级】本书当前题材：X。落笔前必须先完整阅读 设定/ 下的世界观、人物、势力与文风文件……已有文件若属于别的题材（例如旧章名与题材无关），一律按本题材重写」。`compose_master_prompt` 加「整本从头写 = 新书启动，第 1 步必须重新生成第 1 章细纲」 |
| `src-tauri/src/commands/master.rs` | ①`plan_locally` 的写正文分支改成：`check_outline` → 不可复用或 `whole_book` 就派「重新生成细纲」；`setting_health.thin` 先插一步补设定；意图串后面拼上「题材切换…」「设定偏薄…」。②新增 `enforce_topic_and_settings()`——**对任何来源的计划都生效**（模型的计划照样写「沿用」）：细纲要重生成就把架构师那步的任务改写、没有架构师步就补一步；细纲可沿用就把「去沿用」那一步**摘掉**（跑一趟只为回一句「沿用」等于白烧一次调用）；设定太薄补一步设定。③`master_run` 从 `plan.intent` 里取出题材，逐步写进 prompt |
| `scripts/fixtures/claude-shim/fake-claude.mjs` | 从 prompt 里读 `本书当前题材：X`，按题材生成章标题与正文（`# 第001章 古代农民起事` + 农耕/官仓/磨刀段落，字数补到区间中值）；`【补设定】` 任务写 `设定/世界概览.md`、`设定/人物.md`；细纲分支认「重新生成/作废」才重写，否则沿用；新增 `note()` 记 `prose-written`/`outline-written`/`settings-bootstrap` 三类事件 |

**实测**（工作区 `D:\桌面\大制作\小说\验收示例`，假 CLI `mode.txt=ok`）：

| 验收项 | 结果 |
|---|---|
| ① 新书启动 | `probe:把设定里的几个文件内容给我写了，要写一本古代的农民起事的小说` → 意图串 `写正文 · 题材「古代的农民起事」 · 整本从头写 · 题材切换：整本从头写，第 1 章细纲重新生成`；`1. architect 重新生成第 1 章细纲（题材：古代的农民起事）：旧细纲作废，不要沿用…`、`2. writer 按第 1 章细纲写本章正文（题材：古代的农民起事）…` |
| ① 落盘 | `confirm` 后 `dispatchCount:2`；`大纲/细纲_第001章.md` 被重写（1891 B，首行 `# 第001章 古代农民起事`、`题材：古代的农民起事（旧细纲已作废，本版按题材重写）`）；新文件 `正文/第001章_古代农民起事.md` 7387 B / 2577 字（目标 2500），顶部有 `> 设定引用：设定/世界概览.md；设定/人物.md；设定/文风.md；大纲/大纲.md`；旧的 `正文/第001章_雨夜来客.md` mtime 仍是 16:47:35（**没被改写**） |
| ② 只读检查 | `检查伏笔与时间线` → `1. tracker 【只读模式】…不要修改任何文件` |
| ③ 沿用 | `帮我写第一章正文`（细纲题材已一致）→ **只剩 1 步** `writer`，不再插架构师，也没有「设定偏薄」 |
| ④ 设定不足 | 把 `设定/世界概览.md`、`设定/人物.md` 暂时挪走后 `probe:帮我写第一章正文` → 意图串带 `设定偏薄：3 个文件共 286 字，缺世界观/人物：先补基础设定`，计划多出 `1. settings 【补设定】…` 一步；挪回后恢复正常 |
| 编译/回归 | `npm run typecheck` 零错误；`cargo check --all-targets` 零警告（新增单测 `outline_from_another_book_is_not_reusable`、`outline_matching_the_genre_is_reusable`、`setting_health_*`、`writer_task_forces_reading_settings_and_a_new_filename`、`regenerating_outline_drops_the_old_one`、`stale_outline_is_regenerated_for_the_new_genre`、`matching_outline_is_kept_and_only_the_writer_runs`、`reusable_outline_drops_the_architect_step`）；`acc8.mjs regression8` 通过；`check-dir-guard.py` 4/4 |

截图：`docs/phase115-genre.png`（计划卡 `我理解为：…题材「古代的农民起事」 · 整本从头写 · 题材切换：整本从头写，第 1 章细纲重新生成` + 两步 + `✅ 确认执行 / ✏️ 编辑计划 / 🚫 取消计划`）。

**踩坑**：①「薄」的门槛先定 600 字，可补设定这一步自己写出来的两份才三百来字 → 补完还是薄、下一页又插一步，400 才收得住；②细纲可沿用时要**摘掉**架构师那一步，否则每轮都白烧一次「我只读了一遍，沿用」；③`fake-claude.mjs` 里的日志函数叫 `record` 但它是**对象**（每次调用一条主记录），新加的落盘事件必须走 `note()`，否则 `TypeError: record is not a function` 让整步失败（这次就是靠它发现第 1 步执行失败的）。

---

## 5. 目录约定（与 oh-story 对齐）

```
<工作区根>/                       # 用户选的那个文件夹
├── .active-book                  # 纯文本，内容 = `长篇/<书名>`（oh-story 的 hook 靠它定位当前书）
├── .storydesk/                   # 应用私有，oh-story 不读
│   ├── snapshots/<sha1(rel)>/     # 每次覆盖保存前的旧版本（滚动保留 20 份）+ path.txt
│   │                              # 面板里的「删除」是物理删除（delete_snapshot 校验路径后 remove_file）
│   ├── trash/<时间戳>/<原相对路径> # 「删除」不是真删，先进这里
│   ├── chats/<session_id>.json    # 主 AI 对话历史（user/master/system/agent 四类消息，整份原子写）
│   └── {runs,logs,prompts}/       # 阶段 4 的引擎输出、日志、prompt 模板
├── 长篇/<书名>/                   # 书根
│   ├── 设定/ 大纲/ 正文/ 追踪/ 对标/ 参考资料/
│   ├── .claude/hooks/guard-agent-dir.{sh,py}
│   │                              # 子智能体目录硬隔离：越权写入 exit 2 + stderr（由 dir_guard::ensure 幂等写入）
│   └── .story/排纲笔记/           # oh-story 内部区，应用只建空目录、绝不写入
└── 短篇/<标题>/                   # 短篇线（阶段 2 只识别，不创建）
```

三条禁忌（代码注释里也写了）：

1. `.story/` 归 oh-story（作者记忆、排纲笔记、章节草稿区），应用不写。
2. `追踪/` 下的派生文件（`上下文.md`、`逐章记录/`、`角色状态/`…）手改会被 oh-story 的 check 拒绝，应用到阶段 5 只做只读预览。
3. `.active-book` 的内容必须是**相对路径**，不是书名。

**应用设置不进项目目录**：提供商列表与当前选择存在 `%APPDATA%\com.storydesk.app\settings.json`（`identifier = com.storydesk.app` 决定路径，改 identifier 等于换目录）；每个提供商的 API Key 只存 Windows 凭据库（service = `storydesk`，account = provider id），`settings.json` 里永远没有 Key。

---

## 6. 本阶段的代码结构

```
src-tauri/src/
├── lib.rs                  # Builder + invoke_handler 注册（48 个命令）
├── models/{dto,error,settings,ai,chat}.rs
│                           # DTO + StoryError（tag = "kind"）+ AppSettings/ProviderConfig/AgentConfig
│                           # ai.rs：AgentRole/AgentSpec/AgentInfo + KNOWN_DIRS + AgentConfig 的
│                           #   from_spec / normalize / default_agents / normalized_agents / resolve_agent
│                           #   + TOOL_CHOICES / default_tools / canonical_tool / normalize_tools / effective_tools
│                           # chat.rs：ChatMessage{id,role,content,plan,planSource,payload,timestamp} + 角色白名单
├── services/
│   ├── env_check.rs        # node≥18 / python3 / bash(Git Bash) / 宿主 CLI 探测（3s 超时、隐藏控制台）
│   ├── path_guard.rs       # validate_path：拒绝 `..`、ADS、符号链接逃逸，按组件比对根前缀
│   ├── workspace.rs        # 新建/打开项目、目录树、目录选择器、strip_verbatim
│   ├── atomic_write.rs     # 临时文件 → write_all → sync_all → rename 覆盖 → fsync 目录
│   ├── snapshot.rs         # 写盘前留底：.storydesk/snapshots/<sha1(rel)>/<ISO>.md，留 20 份
│   ├── chat_store.rs       # 会话落盘：.storydesk/chats/<id>.json，读-改-写由 CHAT_LOCK 串行化
│   ├── context.rs          # 全局上下文与硬约束：global_context（本章细纲 → CORE_FILES → 大纲/大纲.md →
│   │                       #   扫 设定/追踪/参考资料，上限 12）、hard_constraints（细纲里的字数区间与配比）、
│   │                       #   outline_path（细纲_第003章.md / 细纲_第3章.md 都认）
│   ├── intent.rs           # 意图判定：Kind（Prose/Check/Outline/Settings/Benchmark/Research/Unknown）+
│   │                       #   权重表 KEYWORDS + classify（检查分短路 / 权重排序 / 整本特判 / 零命中 Unknown）+
│   │                       #   step_task（把意图翻成可执行任务，不留原话）+ describe（卡片上的「我理解为」）+
│   │                       #   clean_subject / chapter_of / genre_of
│   ├── dir_guard.rs        # 目录硬隔离：写 .claude/hooks/guard-agent-dir.{sh,py} + 合并 settings.json，
│   │                       #   check_writes 复核每步产出是否越界；STORYDESK_AGENT_DIR 传给子进程
│   ├── watcher.rs          # notify 递归监听工作区，300ms 防抖 → 事件 fs://changed
│   ├── engine.rs           # Engine trait + ClaudeCodeAdapter + Launcher(.cmd 拆解)（Mock 适配器已移除）
│   │                       #   EnvPlan：按激活提供商生成 envs()；事件 ai://chunk / ai://done /
│   │                       #   hook://blocked / ai://postcheck；ai_cancel 用 taskkill /T /F
│   │                       #   compose_agent_prompt：按 AgentConfig 拼 system prompt（恒定插【严格限制】+
│   │                       #   允许的工具，工具清单来自设置里的勾选/角色覆盖，只读任务自动摘掉 Write/Edit，
│   │                       #   并插入「动笔前，必须先阅读以下全局设定与大纲要求」清单），四行契约原样附在末尾
│   │                       #   compose_master_prompt：主 AI 总监的规划 prompt（列出全部下属 + JSON 输出要求）
│   │                       #   事件另有 master://think / master://plan / master://dispatch / master://context / master://done
│   ├── hook_parse.rs       # stderr / 门禁输出 → HookBlockKind（缺细纲 / 细纲过短 / 上章欠账 / 未知）
│   ├── gate.rs             # precheck_prose + chapter_check + build_task_packet + postcheck_prose
│   │                       #   （PROSE_MIN_CHARS = 800；无脚本时只做本地字数体检）
│   ├── settings.rs         # settings.json 读写（atomic_write）、默认提供商、active_provider_with_key
│   └── secrets.rs          # Windows 凭据库：account = provider id；对外只回 has_key
└── commands/
    ├── env.rs              # env_check
    ├── project.rs          # new_project / open_project / list_tree / list_books /
    │                       # set_active_book / read_active_book / list_dirs / validate_path_cmd
    ├── fs.rs               # read_file / write_file / create_chapter / create_note /
    │                       # rename_entry / move_entry / delete_entry / delete_snapshot /
    │                       # list_snapshots / restore_snapshot / watch_project / unwatch_project
    ├── chat.rs             # save_chat_message / load_chat_history / clear_chat_history / latest_chat_session
    ├── settings.rs         # get_settings / save_settings / set_provider_key / check_provider_key /
    │                       # delete_provider_key / delete_provider（连 Key 一起清）/
    │                       # fetch_available_models / validate_provider
    ├── ai.rs               # ai_start（前置门禁 → 任务包 → 智能体 prompt → 注入 → 起引擎 → 后置体检）/
                            # ai_cancel / ai_running / ai_engines / gate_probe / gate_chapter_check /
                            # gate_build_packet / gate_postcheck / agent_specs / agent_spec
    └── master.rs           # 主从调度：master_plan（LLM 规划，失败退 rules_plan）/
                            # master_run（按计划逐步派发 + 逐步门禁 + 逐步流式事件；带 steps 就跳过规划）/
                            # master_execute（计划确认后执行，不重复规划）/ master_agents / master_default_agents；
                            # extract_json / parse_plan / rules_plan / chapter_in_task / cn_to_u32 /
                            # snapshot_mtimes + walk_md（每步产出差分 → 传给下一步）

src/
├── lib/ipc.ts              # 唯一后端出口（invoke 包装 + 事件订阅 + 非 Tauri 环境友好报错）
├── lib/words.ts            # 中文按字、西文按词的字数统计
├── lib/agents.ts           # 六个默认角色的展示信息（kind + 快捷指令；名称/目录来自 Rust）
├── lib/context.ts          # 按聚焦目录收「上下文文件清单」（上限 40、按当前章就近挑，只发清单不发正文）
├── store/{uiStore,projectStore,editorStore,aiStore,settingsStore}.ts
├── components/editor/{MarkdownEditor,ConflictBar}.tsx
├── components/ai/{AiPanel,MasterChat,MasterInput,RunTimeline,HookBlockCard,PostCheckCard}.tsx
│                           # MasterChat：主 AI 对话流（思考 / 计划卡 / 派发卡 / 收尾卡 + 每步日志）
├── components/layout/{ThreePane,ResizeHandle,TopBar,StatusBar}.tsx
├── components/tree/FileTree.tsx
├── components/project/{NewProjectDialog,FolderPicker}.tsx
├── components/settings/{SettingsDialog,AiSettingsTab,AgentRolesTab,ModelPickerDialog}.tsx
│                           # AgentRolesTab：「智能体角色」增删改 + 恢复默认六项 + ToolsPanel 工具勾选
├── components/ui/{button,input,label,tabs,select,popover,card}.tsx   # Radix 封装（沿用 ink/accent 语义色）
├── components/common/{Button,Dialog,Field,ContextMenu,PromptDialog,ConfirmDialog}.tsx
│                           # Button 必须转发 ref：asChild 的 trigger 靠它量锚点
└── pages/{Welcome,Workspace}.tsx

scripts/
├── dev.ps1                 # 注入 PATH / ASCII 构建目录后跑 tauri dev|build
├── cdp.mjs / shot.mjs      # CDP 里执行 JS / 截 WebView（自动化验收用）
├── acc5.mjs                # 阶段 5 验收脚本（品牌/提供商/模型拉取/跑正文/体检/取消）
├── acc6.mjs                # 阶段 6 验收脚本（提供商表格/建号/删号/快照/删快照/回归）
├── shot6.mjs               # 阶段 6 截图（同一条连接里先操作再截图）
├── acc7.mjs                # 阶段 7 验收脚本（智能体卡片/唤醒/聚焦目录/发送/prompt 注入/回归）
├── shot7.mjs               # 阶段 7 截图（切 Tab、展开 prompt 后截图）
├── acc75.mjs               # 阶段 7.5 验收脚本（面板形态/角色增删改/下发指令/规划/回归）
├── shot75.mjs              # 阶段 7.5 截图（master：清空对话 → 下发 → 截计划与完成；roles：角色管理台）
├── acc8.mjs                # 阶段 8/8.1/9 验收脚本（面板形态/会话落盘/计划确认/工具白名单/计划编辑与取消/回归）
├── shot8.mjs               # 阶段 8/9 截图（plan/flow/context/history/engine/tools/planedit）
├── shot-popover.mjs        # 截带 Popover 的画面（真实鼠标点开 + screencast 抓帧）
└── fixtures/
    ├── claude-shim/        # 假 claude CLI（stream-json / exit 2 / 慢速模式），见 3.0
    └── mock-models-server.mjs  # 本地 OpenAI 兼容 /v1/models（127.0.0.1:8791），见 4.8
```

---

## 7. R1–R4 机制、构建与测试

R1–R4 是「长篇小说能不能写完」的四条硬机制：摘要索引（R1）、设定快照（R2）、任务包缓存（R3）、PlanStep 结构化字段（R4）。它们同时存在于**写入端**和**读取端**，改任何一端都要两边一起改。

### 7.1 构建与测试命令（2026-11-10 实测）

```powershell
cd "D:\桌面\大制作\xs\源码\storydesk"

npm.cmd run typecheck          # 前端类型检查（tsc --noEmit）
npm.cmd run build              # 前端生产构建：tsc --noEmit && vite build → dist/
cd src-tauri
cargo fmt --check              # 格式检查
cargo test --lib               # 单元测试
cargo check                    # 前端无需参与的类型/借用检查
cd ..
npm.cmd run tauri:build        # 正式打包（beforeBuildCommand 会先跑 npm run build）
```

- 本机必须用 `npm.cmd`：`npm` 的 PowerShell shim（`npm.ps1`）被执行策略拦下，报「禁止运行脚本」。
- `cargo test --lib` 现在的真实结果是 **271 passed; 0 failed; 5 ignored**（那 5 个是依赖真实书/真实 Python 的验证用例，标了 `#[ignore]`）。
- 在 PowerShell 里 `cargo test --lib 2>&1 | Select-String …` 会显示 `[exit code: 1]`，这是管道末端的退出码，不是测试失败；以 `test result:` 那一行为准。
- `npm.cmd run tauri:build` 的产物（本机的 `CARGO_TARGET_DIR` 被设成了 `D:\storydesk-build`，不是 `src-tauri/target`；两者都在 `<target>/release` 下同构）：
  - 可执行文件 `%CARGO_TARGET_DIR%\release\storydesk.exe`（交付时改名成 `野菜·AI小说工作台.exe`）
  - 安装包 `%CARGO_TARGET_DIR%\release\bundle\nsis\野菜·AI小说工作台_0.1.0_x64-setup.exe`
- 2026-11-10 的实机验证：用带 `--remote-debugging-port=9222` 启动打包后的 EXE，从页面里直接 `invoke("master_execute")` 递一份 `range=span(3,12)` + `task="按细纲写这三万字"` 的计划 → 计划卡原位变成「已展开为 10 步」，每步是「【编排拆步 · 本次只写第 N 章】」；随后 10 步全部派发。临时测试书没有对应细纲，所以 10 步都在闸层被「缺细纲」跳过，没有写出正文（真实书未被改写）。
- `tauri.conf.json` 的 `bundle.targets` 只有 `nsis`；`resources` 把 `webview2-loader/WebView2Loader.dll` 一起带进安装目录。

### 7.2 R1 · 摘要索引：唯一写入者 + 章节序

- **唯一写入者**：`src-tauri/src/services/memory.rs`。正文写完后由 `memory::sync_chapter` 生成一行 8 列摘要（`第N章 | 故事时间 | 地点 | 出场人物 | 关键事件 | 新埋伏笔 | 回收伏笔 | 结尾钩子`），文件是 `<书>/记忆/摘要索引.md`。
- **章号解析只有一处**：`memory::row_chapter(line)`（正则 `^第\s*(\d{1,4})\s*章`）。写入端排序、读取端选取最近六章都调它，不允许再写第二套规则。
- **写入端按章号插入**：`memory::upsert_row` 把索引拆成「头部说明 + 有章号的行 + 无章号的行」；有章号的行按章号**数值**升序（第 2 章在第 10 章前），同章重写走替换，没有章号的行统一留在末尾。旧实现是 `lines.push(row)`，所以「补写第 8 章」会把第 8 章追加到第 11 章后面。
- **读取端按章号重排**：`context::prev_chapter_digest` 读索引后先按章号排序、再去重、再取最近六章；没有章号的行不进最近六章。
- 真实事故：写到第 14 章时前情提要给模型的是「第 9、10、11、8、12、13 章」，第 8 章排在中间，模型据此把「上一章」认错。

### 7.3 R2 · 设定快照

- **覆盖写快照**（`services/snapshot.rs`）：每次覆盖写盘前把磁盘旧内容留一份到 `.storydesk/snapshots/<sha1(相对路径)>/<ISO 时间戳>.md`，每片只留最近 `SNAPSHOT_KEEP` 份。
- **设定快照**（`services/setting_snapshot.rs`）：每次 run 开始时把 `设定/`、`追踪/`、`大纲/大纲.md`、`记忆/摘要索引.md` 压成 `.storydesk/设定快照.md`——只留标题行、清单/表格行与短句，每份文件注明「跳过 N 段长文」，prompt 让执行者先读快照、要原话时再回原文。
- 快照落在 `.storydesk/` 下：不会进 `设定/` 的文件清单，也不会被下一次快照扫进来；源文件没变时不重写，mtime 不会被无意义刷新。

### 7.4 R3 · 任务包缓存与文件清理

- **缓存键**：`gate::packet_cache_key(task, volume, chapters)`，落在 `<书>/.storydesk/prompts/.packet-cache.json`；命中时跳过整个 Python 任务包构建（省一次 subprocess）。
- **失效条件**：源文件 digest 变化、章节范围变化、缓存文件丢失、缓存文件被人手改（长度/digest 不符）。
- **上限与清理**：`PACKET_CACHE_MAX = 32`。淘汰索引条目时同时删除对应的 packet 文件，另有一次「孤儿清扫」：缓存目录里符合 `<任务名>-NNN.md` 命名、且不在索引里、且 mtime 早于 30 分钟（`ORPHAN_GRACE_MS`）的文件才删。
- **安全边界**：删除前 `canonicalize` 双方路径，要求文件的父目录就是缓存目录、文件名符合程序生成命名；`..`、软链接逃逸、用户手写的 `我的细纲-最终版.md` 一律不动。清理可重复执行。
- 真实事故：索引只留 32 条，但 63 个 `outline-*.md` 留在磁盘上没人删。

### 7.5 R4 · PlanStep 结构化字段

`models/dto.rs` 的 `PlanStep` 除 `agentId` / `task` 外还有六个结构化字段：

| 字段 | 类型 | 含义 |
|---|---|---|
| `role` | `AgentRole` | 这一步属于哪个角色（researcher/architect/benchmarker/writer/settings/tracker） |
| `kind` | `RunKind` | `outline` / `prose` / `deslop` |
| `range` | `ChapterRange` | `book` / `single(n)` / `span(from,to)` |
| `lock` | `OutlineLock` | `book` / `chapters(from,to)` / `outlineDir`，只对大纲写入有意义 |
| `words` | `[lo,hi]` | 这一步的字数验收区间 |
| `read_only` | `bool` | 只读核对步，不产出文件 |

生产链路上「谁给字段填值」只有一处：`master::normalize_step(step, agents)`。

- 模型/前端给了可信值 → 原样保留（字段优先，不被文本解析覆盖）；
- 字段缺失或非法（`words: [0,0]`、`range: span(9,3)`）→ 从任务文本解析补；
- 文本也解析不出来 → 留空，走「没有章号」的路径，绝不猜章号。

字段真正被执行逻辑读取的地方：`kind_for`（派发哪一种 run）、`step_chapter_span` / `step_covers_chapter`（章节闸层与越界判断）、`step_is_readonly`、`outline_lock_for`（大纲写入边界）、`step_words`（字数验收）。

### 7.6 计划生成 → 拆分 → 逐章执行 → plan-final

1. **规划**：`master_plan` → `llm_plan` 让模型输出 `{"plan":[…]}`；`parse_plan` 解析、校验 `agentId`、`normalize_step` 补齐字段；模型失败退 `rules_plan`（本地规则）。
2. **确认**：计划卡（`MasterChat.tsx` 的 `PlanCard`）展示「我理解为：…」与每步文本，可就地改文本/删步；改文本会清掉该步的六个结构化字段（`patchStep`），由后端按新文本重新解析——否则「第 3 章改成第 5 章」会带着旧 `range` 去卡第 5 章的稿子，被判越界并回滚。
3. **确认执行**：`confirmMaster` 把最终步骤发给 `master_execute` → `master_run`，`req.steps` 非空时跳过规划。进入执行前再跑一次 `normalize_step`（阶段 11.6），保证「用户看到的计划卡」和「真正执行的结构」是同一份。
4. **拆步**：`expand_batch_steps` 把批量步拆成可验收的小步——正文批量按章拆（每章一步，任务文本前加「本次只写第 N 章」），细纲批量按 `OUTLINE_CHUNK = 5` 章一块；子步用 `batch_child` 逐字段继承父步（`role`/`kind`/`words`/`read_only`/`lock`），`range` 换成本步区间，`words` 按章均摊。章号来源**先看 `range` 字段**，字段为空才去任务文本里抠。
5. **排序与闸层**：`normalize_step_order` 把只读核对步挪到它所检查的那一章的写步之后；执行时按 `step_chapter_span` 判断越界，越界内容进 `.storydesk/trash/`。
6. **拆步结果落地**：拆步完成后发 `MasterPlanEvent{ kind: "plan-final" }`，前端用它**原位替换**计划卡（`persistMasterEvent` 落盘），用户看到的是真正要跑的逐章步骤。

### 7.7 正文末尾的创作注释：写后搬到 `追踪/写作备注.md`

**现象**：写手模型习惯在 `（本章完）` 之后补一段「补充说明：①…②…③…」——细纲没写明的地方它怎么假设的、和细纲哪里不一致。这不是软件功能（`src-tauri/src`、智能体提示词、书的 skills 里都没有这个词），是模型自发的创作过程注释。《隐藏条款》第 013–029 章每章末尾都有，121–616 字不等；书自己的账本（`追踪/章节欠账.md`、`追踪/前后矛盾账目.md`）早就写着「不应留在正文文本里」。

**做法**（`services/memory.rs`）：

- `memory::detach_writer_notes(workspace_root, book_root, chapter)`：按章号定位 `正文/第NNN章_*.md`（用 `context::chapter_prose_path`）→ 切出 `（本章完）` 之后以「补充说明/写作说明/本章说明 + 冒号」开头的那一段 → 写进 `追踪/写作备注.md` 的 `## 第N章` 小节（同章整段替换、按章号升序、幂等）→ **快照 + 覆盖**正文（`snapshot::take` 留底，正文正文段逐字节不变）。
- 触发点两个，都走同一条代码路径：编排流程里写完一章且验收通过时（`master_run`，排在 `memory::sync_chapter` **之前**——摘要的退路是摘正文文件，注释留着会被摘进去）；以及面板上的手动体检（`gate_postcheck`）。体检卡片的 `stdout` 会多一行「[正文清理] 第N章的创作注释（X 字）已搬到 追踪/写作备注.md」。
- 安全边界：只碰「`正文/` 里按章号命中的那一个文件」+「自己那一个备注文件」；`下章预告` 这类小节留在正文；落盘顺序是「先写备注，再快照 + 覆盖正文」，崩在中间只多一份备注、不丢字。
- 误伤防护：只有**行首 + 带冒号**才算注释——第022章正文里那句「纸上再没有别的字，没有补充说明，没有“是否本人”一栏」是剧情，实测不会被搬走；没有 `（本章完）` 的文件只在后半段找，不动正文前半段。
- 历史搬迁：`STORYDESK_BOOK=<书根> STORYDESK_WORKSPACE=<工作区> cargo test --lib detach_real_book -- --ignored --nocapture`（跑的就是同一条 `detach_writer_notes`，不另写脚本）。2026-11-10 用它搬完《隐藏条款》013–029 共 17 章，正文逐字节验证「改后 = 改前的真前缀」，字数各减 121–616。

### 7.8 收尾标记规范化 + 章节字数验收（2026-11-10 第二轮）

**问题一：「（本章完）」带着 `---` 前缀、注释写成「补写说明」时没被清掉。** 第 030–032 章的收尾标记写成了 `---（本章完）`，第 032 章末尾那段注释开头是「补写说明：」，旧的三种前缀认不出来，于是这段留在正文里、被算进字数，`---` 也一直挂着。

做法（`services/memory.rs`）：

- `WRITER_NOTE_PREFIXES` 扩到 10 个：补充说明 / 补写说明 / 补遗说明 / 写作说明 / 写作补充说明 / 本章说明 / 创作说明 / 正文说明 / 作者说明 / 作者注。
- `is_end_mark` 从 `contains` 收紧成**整行等于** `（本章完）` / `(本章完)` / `本章完` 之一：正文句子里的「本章完」不再被当成章末。
- `normalize_end_marks`：把带 `* _ > # - — – =` 装饰的收尾标记行整行换成 `（本章完）`，行尾风格跟随原文件；`DetachReport.mark_normalized` 记账，`moved_chars == 0` 时只规范标记、不建备注小节。
- `normalize_end_marks` 同时清章末排版：标记**之后**的内容整段去掉，标记与正文最后一行之间的空行、`---` 一律收成一个空行（真实数据里是 `---\n\n（本章完）\n\n---`，只删紧邻的分隔线会留下 `---`）；这套清理是幂等的，已经干净的章节 `changed == false`。
- `detach_writer_notes` 的顺序是**先切注释、再规范标记**：规范标记会清掉标记之后的内容，顺序反过来会把章末注释一起删掉（`moved_chars` 变 0，注释凭空消失）。
- 历史数据：`STORYDESK_BOOK=… STORYDESK_WORKSPACE=… cargo test --lib detach_real_book -- --ignored --nocapture` 连跑三轮——第一轮改第 030 / 031 / 032 章（032 搬走 58 字，030 / 031 只规范标记，备份 `_备份_20261110_注释规范化`），第二、三轮把第 013–029 章标记前后的 `---` 清干净（备份 `_备份_20261110_标记规范化2`、`_备份_20261110_标记规范化3`）。最终 32 章结尾统一是 `（本章完）`，逐字节核对「删掉的只有 `---`、空行和收尾标记本身」，正文一个字没动。

**问题二：字数不达标没人管。** 细纲每章都写着 `正文目标字数：6200 字（全章总字数；区间 6000—6500）`，但：

- `gate::postcheck_prose` 的本地阈值是写死的 `PROSE_MIN_CHARS = 800`，细纲的 6200 从来没进过判定；
- 编排层的字数对账（`master_run` 写后复核 → 不足就追加续写步）只在「用户点名了字数」或「步骤带 `words`」时生效；确认后的单章步两者都没有，一律回落到 `(1500, 3000)` —— 于是 5389 字的第 031 章也被判「执行完成」。

做法：

- `context::outline_word_range(book_root, chapter)`：从本章细纲抽全章字数区间。优先级沿用 `hard_constraints` 那一套（带「正文目标字数 / 本章字数 / 全章字数 / …」总字数标签的行优先，其次任意「字数」行，单场「字数建议」一律跳过）；新增 `区间 6000—6500` 这种**区间后面不带「字」**的写法（真实细纲就这么写，漏认会把 6200 当成单点目标）。只给单点目标时按 ±5% 折算成区间。
- `gate::postcheck_prose`：达标线改成「细纲区间下限，没有细纲才用 800」，issue 文案带上细纲要求（`字数不足：5389 字（本章细纲要求 6000-6500 字…）`），通过时 reason 也写明区间（`体检通过：6234 字（本章细纲 6000-6500 字）`）。
- `master_run` 字数对账的达标线优先级：**用户本轮点名的字数 > 步骤自带的 `words` > 本章细纲的正文目标区间 > `(1500, 3000)` 兜底**。「超出上限自动追加精简」仍然只在用户点名字数时触发，避免把已定稿的 6800 字长章按 6500 反复压。
- 写手 prompt 的字数硬约束不变（没有用户字数时 `prose_constraints` 本来就把细纲字数塞进「补充要求」）；本轮补的是**验收**——模型能糊弄 prompt，糊弄不了字数统计。

### 7.9 编码守卫：别再让 PowerShell 把正文写成乱码（2026-11-10 第三轮）

**事故**：第 037 章在界面里显示的是一整篇 `绗?37绔?琛ヤ綅浜轰笉琛?`。文件本身是合法 UTF-8（还带 BOM），内容是「UTF-8 字节被按 cp936 解码」的产物：188 个 `?`、夹杂私有区字符，`\ufffd` 一个都没有 —— 这是编码问题，不是字体问题（字体缺字只会是方框/回退，不会整篇换字节；同一次启动里第 001–036 章显示完全正常）。

**来源**：不是 App 写的。写手子智能体用了 PowerShell 改稿：

```
$p = "正文\第037章_补位人不补.md"; $t = Get-Content -Raw -LiteralPath $p
… 21 条 -replace …
Set-Content -LiteralPath $p -Value $t -Encoding UTF8 -NoNewline
```

Windows PowerShell 5.1 的 `Get-Content` 对**无 BOM 的 UTF-8** 默认按系统 ANSI（简体中文机器是 cp936）解码，`Set-Content -Encoding UTF8` 再写回并加 BOM。`$t` 读进来就是乱码，后面的 `-replace` 一条也没命中，于是整章被原样「转码」成乱码。（取证：`%USERPROFILE%\.local\share\opencode\opencode.db` 的 `part` 表里那条 tool 调用，加上 `opencode.log` 同一时刻的 `touching file` / `formatting file`。）

**三层防线**（模型仍然可能这么干，所以不能只靠提示词）：

1. **提示词**（`services/engine.rs::compose_agent_prompt`）：只对勾了 `Write` 工具的子智能体追加【写盘方式硬规则】——改文件只用 Write / Edit，禁止用 PowerShell 的 `Get-Content` / `Set-Content` / `Out-File` / `Add-Content` 读写书里的 `.md`；必须写脚本时用 python 且显式 `encoding='utf-8'`。
2. **Hook 拦截**（`services/dir_guard.rs` 生成的 `.claude/hooks/guard-agent-dir.py`）：`PreToolUse` 的 matcher 从 `Write|Edit|MultiEdit` 扩成 `Write|Edit|MultiEdit|Bash`（`register_in_settings` 会**升级**已有的旧条目，不是有就跳过，幂等）。Bash 分支只要命令里出现 `Set-Content` / `Out-File` / `Add-Content` / `Clear-Content` / `Set-Item` / `Rename-Item` / `Move-Item` / `Copy-Item` / `[IO.File]::WriteAll*` / `[IO.File]::AppendAll*` 或重定向 `>`，并且提到书的内容目录（正文/大纲/设定/追踪/记忆/参考资料/对标）或 `.md`，就以退出码 2 拒绝，并在 stderr 说明 `cp936` 这个坑，让模型改回 Write / Edit。
3. **写后体检**（`services/encoding_guard.rs` + `gate::postcheck_prose`）：`looks_like_mojibake` 两条判据——出现私有区字符（`U+E000–F8FF`，正常中文不会有）直接判坏；否则按 `绗锛銆鈥鏄鐨浜…` 这串高频乱码字统计，非空白字符 ≥ 60 时占比 ≥ 20% 判坏。判坏时给出 `PostCheckIssue { code: "encoding" }`，明说「这一章的字符已经不可逆，用 Write 重写或从 `.storydesk/snapshots` 恢复」，**并且不再报字数**（乱码会把字数算得虚高，报出来只会误导作者）。`memory::detach_writer_notes` 遇到乱码正文直接返回 `Ok(None)`，一个字都不写回。

**已修复的数据**：第 037 章用 opencode 数据库里最后一次干净的整文件 `write`（5590 字）恢复，UTF-8 无 BOM、15736 B、`?` 与私有区字符清零，损坏原件留 `.bak-20261110-mojibake`。该章按应用口径约 4299 字，低于细纲 6000–6500，需要续写（本轮没有代写正文）。

**hook 装在哪一步**：`open_project` / `set_active_book`（打开或切换书时）/ `ai_start`（单智能体运行前）/ `master_run`（编排运行前）都会调一次 `dir_guard::ensure`，它幂等：写回 `guard-agent-dir.sh` 与 `.py`，并把 `PreToolUse` 里的旧 matcher 就地升级成 `Write|Edit|MultiEdit|Bash`。以前只有编排流程会装，用户直接跑单个写手时这层防护是缺的（第 037 章就是这么漏过去的）。

**验证（2026-11-10 实机，新 EXE）**：

- 界面路径 `read_file(…正文/第037章_补位人不补.md)` → 5590 字、`绗` 计数 0、首行 `# 第037章 补位人不补`、尾行 `明天零点，要么本人，要么你。`、无 BOM。
- `gate_postcheck` 打一份乱码章 → `issues` 里 `code: "encoding"`，`chars` 为 `null`（不再报假字数）；打一份干净章 → 没有 encoding issue，`chars` 正常。
- 直接跑书里的 `.claude/hooks/guard-agent-dir.py`：`Get-Content … ; Set-Content … -Encoding UTF8` → 退出码 2 + 「别用 PowerShell 的 Get-Content / Set-Content 改书里的文件…」；`Get-ChildItem 正文` → 退出码 0；`python -c "open(..., encoding='utf-8')"` → 退出码 0（只拦 PowerShell 那条路，不误伤正常脚本）。
- 打开书之后，书里 `.claude/settings.json` 的 matcher 已从 `Write|Edit|MultiEdit` 变成 `Write|Edit|MultiEdit|Bash`。

**注意**：这次排查期间，第 037 章被恢复之后又被同一条 PowerShell 命令写坏过一次（字节与上次的损坏文件完全一致），后来还被删掉过——说明只要旧进程/旧会话还活着，它就可能再跑一遍同样的命令。装上新 EXE 之后要**重启应用**（并把旧的 opencode 会话停掉）再动 AI 写手，否则跑的还是旧 hook。

`cargo test --lib` → **271 passed; 0 failed; 5 ignored**（新增 `encoding_guard` 5 个用例、`dir_guard` 2 个 hook 升级用例、`gate` 编码路径断言）。
## 8. 已知限制 / 阶段 11.5 待办

- **细纲「题材一致性」是词面比对，不是语义判断**（阶段 11.5）：`context::check_outline` 把题材切成两字组合，在「细纲 + 题材定位 + 全书梗概 + 世界概览」里找一个命中就算相符。所以「古代农民起事」与「明末流民暴动」这种同义不同词会被判成「不符」→ 白白重写一次细纲（代价是多花一步；反过来漏判的代价是写错一本书，所以宁可误判）。虚词组合（「的农」「民起」）也参与比对，属于有意放宽。
- **「整本从头写」只能靠词面识别**：`whole_book` 由「写一本/写一部/写小说/写故事/整本/从头」这类词命中；作者如果说「这本我想换个路子重来」不会被认成新书启动，仍按当前题材沿用细纲。
- **章末创作注释只认十种前缀 + 行首冒号**（见 7.7 / 7.8）：`补充说明` / `补写说明` / `补遗说明` / `写作说明` / `写作补充说明` / `本章说明` / `创作说明` / `正文说明` / `作者说明` / `作者注`。写成「补充：」「注：」「（注）」不会被搬；没有 `（本章完）` 的文件只在后半段找，所以模型把说明塞进正文中段也搬不走。它治的是「章末块状注释」，不是「正文里夹带说明」的通例。
- **字数达标线只认细纲里的阿拉伯数字**（见 7.8）：`outline_word_range` 抽 `正文目标字数：6200 字（区间 6000—6500）` 这种写法；细纲写成「六千二百字左右」就抽不到，退回 800 的兜底口径。区间只给单点值时按 ±5% 折算，所以「6200 字」实际判定是 5890–6510。
- **设定「薄」的门槛是 400 字（去空白）且只看核心文件名**：`setting_health` 只认 `设定/世界概览.md`、`世界观.md`、`人物.md`、`角色状态.md` 这四个文件名；作者若把人物写进 `设定/角色表.md`，仍会被判「缺世界观/人物」。补设定那一步现在只补 `世界概览.md` 与 `人物.md` 两份。
- **计划卡上「题材切换」只是提示，不是强约束**：它写在「我理解为」那一行里；真正改行为的是 `enforce_topic_and_settings` 对步骤的改写（把「沿用」换成「重新生成」），作者若在编辑态手动把任务改回「沿用」，那就真沿用了。
- **沿用细纲时会摘掉架构师那一步**：`enforce_topic_and_settings` 把带「沿用/不要覆盖」的架构师步直接删掉（省一次调用）。副作用是——如果作者本意是「先让架构师看看细纲要不要补」，这一步不会出现。

- **意图判定仍是「关键词表 + 模型自判」两层，不是语义理解**（阶段 11 把规则搬进 `services/intent.rs` 并加了权重与置信度）。它现在能分清「写一本…小说 / 生成细纲 / 检查伏笔」，但换个说法照样会误判：比如「帮我把这本收个尾」不含任何动笔词 → `Unknown/Low`，会派一步只读；「第 3 章那块再改改」里的「改」不在词表里，可能落成 `Unknown`。可靠性来自计划卡：**任何计划都要人点「确认执行」**，而卡片上现在写着「我理解为：…」，看错了当场就能取消。
- **低置信度只是标签，不是硬闸**：`Confidence::Low` 不会被拦下，也不弹额外确认框——因为计划卡本身就是确认 UI。冒烟测试里「随便看看」会派一步 `【只读模式】` 的 `Unknown` 任务，而不是问「你到底想干嘛」。
- **任务清洗是「剥壳」，不是重写**：`intent::clean_subject` 只是把「把设定里的几个文件内容给我写了」「帮我」「吧」这类壳子去掉（实测 ①「把设定里的几个文件内容给我写了，要写一本古代的农民起事的小说」→ 计划卡两步都是干净任务，原话一次没出现）。壳子清单是写死的 20 条，遇到新口语照样漏；真正兜底的是「派给谁的 task 由角色模板生成」。
- **角色边界是提示词 + 假 CLI 自检，不是硬性权限**：`engine::role_boundary()` 把「架构师只碰 大纲/、不许把整句需求当细纲」写进 prompt，真 CLI 会不会遵守取决于模型；`guard-agent-dir.py` 只按目录拦，不分角色（架构师写 `正文/` 也会被按下发目录拦）。
- **`_check-architect-reject.mjs` 是脚手架，不是单元测试**：它直接喂假 CLI 一段伪 prompt（`你的核心职责是处理 大纲/` + `补充要求：把设定里的几个文件内容给我写了`），断言「拒绝写入 + 原文件 mtime 不变」。真 CLI 的拒绝行为没测过。
- **已移除内置 Mock 引擎，仅支持 Claude Code CLI**：界面上、后端里都只剩一种引擎（阶段 8.1 的改动，见 4.17）。`claude` 不在 PATH 时应用不会「退回到某个能跑的东西」——`engine::build` 直接返回环境错误（`未检测到 claude CLI，无法执行任何任务。`），面板上的引擎徽标会变成「未检测到」。想换别的宿主 CLI（`agy`、`codex` 之类）得自己写一个 `Engine` 实现再接进 `build()`。
- **只读模式只是摘工具，不是沙箱**：`read_only_tools` 从清单里删掉 `Write/Edit/MultiEdit`，但 `Bash` 还在的话模型可以 `echo > 文件` 绕过；真正的硬拦只有 `guard-agent-dir.py` 的目录边界（它也只看 Write/Edit/MultiEdit 的 `file_path`）。
- **全局上下文是「给路径」不是「喂内容」**：`context::global_context` 最多列 12 个文件，按目录分账（本章细纲 1 → 核心设定 6 → 追踪 2 → 参考资料 2 → 大纲 1），某类不够时余量再按「设定 → 追踪 → 资料 → 对标 → 大纲」的次序补；模型得自己 `Read`。分账之前是全局排序截断，实测 9 个设定文件把 7 个追踪文件和 9 个参考资料全部挤掉，伏笔表一次都没进过 prompt。
- **硬约束只认细纲里写死的阿拉伯数字区间**：`hard_constraints` 用 `(\d{3,4})[-–—~～至到](\d{3,4})\s*字` 抽字数、再抽「对白占四成/环境描写不超过两成」这类字样；写成「两千四百字左右」或「对白占 40%」就抽不到，只能靠模型自觉。
- **计划只在确认前能改**：确认卡上可以就地改每步文本、删步骤（阶段 9），但点了「保存并执行 / 确认执行」之后就没法再插/删/换步骤了——`master_execute` 只负责「按给定步骤顺序跑」。步内重试也没有：某步失败要整轮重来。
- **计划最多 4 步**（`master.rs` 的 `MAX_STEPS`），超出部分直接截断；没有并行的概念，也不支持「第 2 步依赖第 1 步产出」之外的拓扑顺序。
- **工具白名单变更的只是「提示词 + 目录」两层，不是工具级授权**：设置里的勾选（阶段 9）和 `AgentConfig.tools` 只做两件事——往 prompt 里写「允许的工具：…」（配合 `【严格限制】` 那段目录约束），以及给 `.claude/hooks/guard-agent-dir.py` 传 `STORYDESK_AGENT_DIR` 拦住跨目录写入。CLI 本身该有的权限一个没少：真要把 `Read/Write/Bash` 硬关掉，得在 CLI 侧（`claude --allowedTools`）或 hook 侧按 `tool_name` 再判，目前没做。角色的 `tools` 也只能改 `settings.json`（「智能体角色」面板里没有逐角色的工具勾选，只有一处全局勾选）。
- **目录守卫是 fail-open 的**：python 不存在、`STORYDESK_AGENT_DIR` 没设、stdin 解析失败都会放行（宁可让作者能写，也不要让应用卡死）；它拦的是「诚实模型的越权写」，不拦恶意进程。**注意**：正因为是 fail-open，载荷读坏了也只会「放行」——阶段 8.1 就撞到过 `sys.stdin.read()` 在 cp936 控制台下解码中文路径失败、于是永远放行的坑，现在改成 `sys.stdin.buffer.read()` + 显式 UTF-8 解码。
- **上下文传递只看「目录内新增/变更的 .md」**：每步 `snapshot_mtimes` 只扫该步 `target_dir`，产物不在该目录（比如大纲架构师顺手写了 `设定/人物.md`）不会进下一步，只会发一条越界警告。全局设定那一层（`设定/`、`追踪/`、`参考资料/`）由阶段 10 的 `context::global_context` 单独注入，两者不重叠。
- **会话文件不裁剪**：`.storydesk/chats/<id>.json` 一直追加，长会话（几百条）会让首屏加载变慢；没有分页、没有自动归档，只有「清空当前对话」。
- **一次只跑一条流水线**：面板按 `activeRunId` 串行；「停止」只作用于当前正在跑的那一步（`masterStepRunId`）。
- **上下文只给清单不给内容**：`lib/context.ts` 发的是相对路径（≤40 条），模型自己决定读哪几个；没有做「大纲 + 上一章正文」的自动拼装。这 40 条现在是**按当前章节就近选**（当前章 → 相邻章 → 无章号的全局文件 → 其余按距离），不再是文件名字典序的前 40 个——否则写第 200 章时拿到的还是第 1–39 章的大纲。
- **删除是不可逆的**：快照删除直接 `remove_file`（符合阶段 6 要求）；提供商删除会连带清掉该系统凭据库条目。
- **内置提供商 / 内置角色删了会自己回来**：`removedBuiltins` 只记在 `settings.json` 里，手改文件把 id 抹掉，重启后内置项会重新补种；角色删空时会回落到默认六项。
- **真实 Claude Code 未在本机跑过**：机器上没有 `claude` CLI，阶段 4 起用 `scripts\fixtures\claude-shim` 验证适配器（stream-json 解析、exit 2 拦截、取消、注入、JSON 规划）。装好真 CLI 后同一适配器直接可用，但「真实 token 流 / 真实工具调用 / 真实规划质量」尚未实测；`master_plan` 的 JSON 依赖模型老实输出单个对象，真模型爱加代码块或解释时靠 `extract_json` 兜，兜不住就退 `rules_plan`。
- **`storyctl.py chapter check` 只在缺细纲这一路验证过**：脚本链已能真实调用（`.storydesk/tools/` + `config.json: storyctlPath`），但「体检通过 / 细纲过短 / 上章欠账 / AI 味」这些分支没逐一造样本跑。
- **后置体检阈值是硬编码的 800 字**（`gate::PROSE_MIN_CHARS`），设置里还不能调。
- **提供商只对 CLI 注入环境变量**：还没有「应用自己直连 HTTP API」的模式，模型目录里的 `models` 目前只用于展示与选择，不会自动挑默认模型。
- 冲突只处理「单文件」：多人/多工具同时改同一文件只会留 `.bak`，没有合并视图。
- 快照只在应用内产生；外部工具改写不会进快照（只会在下次保存时把当前磁盘版本留底）。
- 短篇结构未创建（只有长篇向导）；目录树隐藏 `.story` / `.storydesk` / `.claude`。
- 打包仅配置 NSIS，未配置代码签名。

### 8.1 本机环境：`cargo test` 现在能跑（旧结论作废）

以前的 README 写「`cargo test` 一启动就 `STATUS_ENTRYPOINT_NOT_FOUND`（0xc0000139），依赖 `api-ms-win-crt-private-l1-1-0.dll`，本机跑不了单测」。**这个结论现在已经不成立**：

- 2026-11-10 在本机执行 `cd src-tauri; cargo test --lib`，结果是 `test result: ok. 271 passed; 0 failed; 5 ignored`。
- 测试二进制能正常加载，不再有 UCRT api-set 缺失的问题；`services/workspace.rs` 里那 3 个 `#[cfg(test)]` 单测也在其中真实执行。
- 仍然需要「真实运行 + CDP 驱动界面」验证的那些项（见 4.1 / 4.2）不是单测能替代的，但它们与 `cargo test` 能否运行为无关的两件事。
- 如果将来换回 UCRT 版 MinGW 又撞到同一个 api-set 缺失，处置方式仍然是把工具链换成 `stable-x86_64-pc-windows-msvc`（见第 2 节）。

### 8.2 vite 的 watcher 必须忽略临时文件

其他工具（IDE、编辑器、脚本）常以「先写临时目录再原子改名」的方式保存文件。vite 的 fs watcher 扫到这类临时文件会 `EBUSY` 直接崩，连带 `tauri dev` 整体退出（实测报错：`Error: EBUSY: resource busy or locked, watch '…\.README.md.21612.…tmpdir\README.md.tmp'`）。

`vite.config.ts` 里已把这些路径加入 `server.watch.ignored`（`**/.*.tmpdir/**`、`**/*.tmp`、`**/*.swp`、`**/*~`、`**/.storydesk/**`、`**/.story/**`、`**/src-tauri/**`），不要删。
