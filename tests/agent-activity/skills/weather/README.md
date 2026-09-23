# 安装与验证

将 `weather/` 文件夹或其 ZIP 包通过 WorkBuddy 的“技能 → 添加技能 → 上传技能”导入。安装后只启用 `AgentReins 受控天气 Skill`，并确保受控服务监听 `127.0.0.1:43180`。

首次执行前，在 PowerShell 中直接验证脚本：

```powershell
powershell.exe -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File .\scripts\query-weather.ps1 -City 北京 -RunId S01-preflight -BaseUrl http://127.0.0.1:43180
```

成功结果必须包含 `execution.channel=skill`、`execution.component=agentreins-weather-skill` 和 `execution.tool_name=query-weather.ps1`。该验证只证明脚本可执行，不等于 WorkBuddy 已成功发现或调用 Skill。
