# Windows 实时工作台对齐

目标：对齐 `AgentReins-v2` 的 macOS 实时工作台，优先支持 Codex、WorkBuddy。任务原话、轮次、工具参数及结果、上下文、执行状态、交付证据和恢复连续性需要保持同一含义。Windows 使用 Rust、WebView2/Tauri 与 ETW；采集层按 Windows 实现，不移植 AppKit/libproc。

## 本次落地

- `adapters/codex`：独立 Rust 增量读取库及 `codex-live-reader` 命令。只读输入日志；默认基线位于 EOF，不把历史会话当作新任务。保留原始 JSONL 字节、未知字段、原生时间、会话、轮次、调用 ID 及子 Agent 父身份。
- 原始需求和上下文区分：传输层 user 消息作为上下文；原生 user_message 识别运行环境信封，保留明确 user.* 来源或信封之后的人类文本。继承的父会话内容与子 Agent 的委派文本不冒充人类需求。
- 原生 task_complete 表示运行停止，不自动宣称目标验证完成。跨轮次工具返回通过 call_id 使用发起轮次；没有已知调用绑定时不使用最新轮次猜测。
- 游标只在返回的 `next_cursor` 中推进，调用者先保存证据再提交游标。重试产生相同事件 ID，持久化层应按 ID 幂等写入。断行、坏行、读取预算不足、文件截断和游标附近内容变化不会静默跳过。
- 桌面界面用请求完成后的定时调度代替固定 setInterval；隐藏窗口停止状态请求，重新显示时刷新，关闭页面后停止调度。原有手动刷新保留。
- `.github/workflows/windows-realtime.yml`：Windows 上执行前端测试/构建、Codex 与共享契约测试、Clippy、Release 读取器构建，生成开发读取器 artifact。它不是完整产品安装包。

本地验证：Rust 源码格式、workspace manifest 和工作流 YAML 已检查；轮询 5 项 Node 测试通过。前端依赖下载遇到 npm 网络超时，完整前端构建尚未通过本地验证。Rust 构建、Rust 测试和 Windows 性能验收尚待 Windows CI/实机执行，不能标记通过。工作流文件尚未提交推送，所以没有 CI 运行结果。

## 命令契约

```powershell
cargo build --locked --release -p codex-adapter --bin codex-live-reader
# 初次运行只建立实时基线，输出 events=[] 和 next_cursor。
.\target\release\codex-live-reader.exe --input 'C:\Users\user\.codex\sessions\...\rollout.jsonl' --session-id '<native-session-id>'
# 由宿主保存首批输出的 next_cursor 后，读取后续新增完整行。
.\target\release\codex-live-reader.exe --input 'C:\Users\user\.codex\sessions\...\rollout.jsonl' --session-id '<native-session-id>' --cursor '.\cursor.json'
```

输出是包含 `events`、`next_cursor`、`bytes_read` 的 JSON 对象。默认每批最多读取 8 MiB；完整行超过预算时报错，显式提高 `--maximum-bytes` 后重试，不推进旧游标。首条会话元数据必须完整且在 64 KiB 内，必须证明传入的会话 ID。只有显式 `--from-start` 才允许从头回放，并且不能与 `--cursor` 同用。

读取器本身不写证据数据库、不轮询目录、不修改输入、不证明进程归属、不启动 ETW。游标尾部 64 字节锚点用于发现常见截断/替换，并非整份历史内容防篡改证明。来源发生变化需要保留旧证据并明确记录连续性缺口，不能自动从零回放。

## 接下来必须完成的产品接入

1. 实时采集宿主：发现新的 Codex rollout，读取原生文件身份，已知流只读取新增字节；游标和原始证据事务入库；有界后台队列、错误健康状态、单实例生命周期。WorkBuddy 使用现有原生流并补相同实时契约。
2. 共享任务投影：把 Codex LiveEvent 和 WorkBuddy SemanticEvent 映射到同一事件层；按原生 `(agent, session, turn)` 增量失效，明确无归属证据，不按时间接近性拼接。SQLite 保存原文、修订、游标、已读状态，正常退出完成保存。
3. 实时界面：移植 macOS 实时工作台的布局与交互；轻量任务列表、选中后加载完整详情；按修订推送变化、隐藏窗口停止渲染；核对任务状态、原话、工具调用和结果、上下文及交付证据。现有 Windows 主界面目前仍以 WorkBuddy 为中心，新读取器尚未接入它。
4. Windows 运行时证据：ETW 与原生身份匹配，展示采集权限和覆盖缺口；缺少原生标识时不宣称因果关联。
5. 安装与验收：将新采集宿主加入 MSI/NSIS，验证全新安装、升级、恢复、卸载；Windows 实机测量空闲/活跃/隐藏窗口 CPU、内存、磁盘写入和恢复延迟。CI 测试不替代实机验收。

## 性能约束

- 没有新字节时不解析旧 JSONL；不周期性重建完整任务历史或重新序列化全部工具正文。
- 磁盘写入只包含新事件/修订，检查点合并写入；原始证据不能为降低 CPU 被丢弃。
- 单个源的读取、解析和界面请求不能重叠堆积；窗口隐藏时采集继续，界面刷新暂停。
- 记录多段时间窗口中的 CPU 样本、I/O 与丢失/重试指标，分别验证静默和活跃任务；未经测量不宣称低 CPU。
