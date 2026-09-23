# AgentReins for Windows

AgentReins 是面向 Windows Agent 应用的安全观测与证据关联工具。当前实现以 WorkBuddy 为首个适配对象，采集 Agent 语义事件、MCP 协议事件和 Windows ETW 事件，并在能够由原生标识证明时建立因果关系。

## 当前边界

- 仅观测，不实施阻断。
- 不使用时间接近性冒充原生因果关系；缺少原生标识时明确展示证据断点。
- 当前只对 WorkBuddy 适配和 Windows 运行环境进行验证；其他 Agent 尚未宣称支持。
- Rust 构建、测试、ETW 采集和性能验证必须在 Windows 上执行。

## 项目结构

- `crates/native-contracts/`：共享事件契约。
- `crates/etw-collector/`：Windows ETW 采集与证据保留。
- `crates/evidence-correlator/`：语义、MCP 与 OS 证据关联。
- `adapters/workbuddy/`：WorkBuddy 语义适配器。
- `crates/gui-host/`、`crates/desktop-shell/`、`apps/desktop-ui/`：本地产品界面。
- `tests/`：Windows 集成、端到端和受控活动测试材料。

## Windows 验证

```powershell
Set-Location apps/desktop-ui
npm ci
npm run build
Set-Location ../..
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
powershell -NoProfile -File scripts/run-windows-smoke.ps1 -SourceRoot . -EvidenceRoot "$env:LOCALAPPDATA\AgentReins\evidence"
```

ETW 采集需要相应的 Windows 权限。请只在受控环境中运行，并避免把日志、证据、凭据或用户数据提交到版本库。

## 许可证

本项目源码采用 [PolyForm Noncommercial License 1.0.0](LICENSE)：符合许可证定义的非商业用途可以免费使用，商业用途必须另行取得付费授权。它属于 Source Available，不是 OSI 定义的开源许可证。

商业授权说明见 [COMMERCIAL.md](COMMERCIAL.md)。第三方依赖、名称、商标和图标不包含在 AgentReins 的许可授权中，详情见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。公开快照仅包含界面实际用于识别 WorkBuddy 的第三方标识；该标识仍归其权利人所有，不表示合作、认可或赞助，也不随 AgentReins 源码许可证授予再使用权利。

## 参与和安全问题

当前暂不接受外部代码贡献，以免商业双授权的权利边界不清。问题反馈方式见 [CONTRIBUTING.md](CONTRIBUTING.md)，安全漏洞报告方式见 [SECURITY.md](SECURITY.md)。
