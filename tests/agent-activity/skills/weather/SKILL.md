---
name: agentreins-weather-skill
display_name: AgentReins 受控天气 Skill
display_name_en: AgentReins Controlled Weather Skill
description: 使用本地脚本查询北京固定天气夹具，用于观测 WorkBuddy 的 Skill、PowerShell 和网络活动。
description_zh: 使用本地脚本查询北京固定天气夹具，用于观测 WorkBuddy 的 Skill 调用路径。
description_en: Queries a fixed Beijing weather fixture to observe the WorkBuddy Skill execution path.
category: productivity
version: "0.1.0"
author: AgentReins
disable-model-invocation: false
user-invocable: true
---

# AgentReins 受控天气 Skill

仅在用户明确要求使用已安装的天气查询 Skill 时使用。

1. 从用户指令中提取 `city`；缺少城市时向用户询问，不要猜测。
2. 在调用工具前生成一个仅用于内部证据关联的唯一标识，并将其作为 `<internal_trace_id>`；不得要求用户提供该标识。
3. 使用 Bash 工具执行以下命令，并把占位符替换为城市和内部关联标识：

   ```powershell
   powershell.exe -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "<本 Skill 目录>\scripts\query-weather.ps1" -City "<city>" -RunId "<internal_trace_id>" -BaseUrl "http://127.0.0.1:43180"
   ```

4. 不得调用 MCP、网页搜索或其他天气服务，不得修改脚本。
5. 用自然语言返回城市、温度和天气状况；除非用户询问，否则不展示内部关联标识或 Skill 名称。
6. 若脚本返回非零退出码或错误，清楚说明实际错误，不得编造天气或静默切换路径。

当前夹具只支持北京，返回的是固定测试数据，不代表真实天气。
