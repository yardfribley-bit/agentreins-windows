---
name: agentreins-weather-mcp-guide
description: 指导 WorkBuddy 只调用 AgentReins 受控天气 MCP 查询北京固定天气，用于 MCP 活动观测。
description_zh: 指导 WorkBuddy 调用 AgentReins 受控天气 MCP 测试工具。
description_en: Guides WorkBuddy to call the AgentReins controlled weather MCP test tool.
version: "0.1.0"
author: AgentReins
disable-model-invocation: false
user-invocable: true
---

# AgentReins 受控天气 MCP

仅在用户明确要求使用已配置的天气查询 MCP 时使用。

1. 从用户指令中提取 `city`；缺少城市时向用户询问，不要猜测。
2. 生成一个仅用于内部证据关联的唯一标识，不得要求用户提供该标识。
3. 调用 `get_controlled_weather`，传入 `city` 和内部关联标识。
4. 不得改用天气 Skill、网页搜索、命令行或其他 MCP。
5. 保留工具返回的执行元数据用于内部观测；除非用户询问，否则只用自然语言回答天气结果。
6. 若工具不可用或报错，清楚说明实际错误，不得编造天气或静默切换路径。

当前夹具只支持北京，返回的是固定测试数据，不代表真实天气。
