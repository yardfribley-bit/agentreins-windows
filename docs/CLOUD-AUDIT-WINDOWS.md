# Windows 云审计上传

Windows 客户端使用与 Mac 版相同的云审计接收服务和任务快照顶层字段。
ETW 采集器不参与网络传输；上传器只读取已发布的 WorkBuddy 语义事件及
有原生 `operation_id` 可关联的 ETW 事件。没有原生关联的 OS 事件不按时间
接近性归给某个任务，快照的 `capture.state` 标记为 `partial`。

上传包含用户请求、模型上下文、工具参数及返回、可关联的文件和网络证据原文。
它可能包含密码、令牌、个人资料或源代码。和 Mac 版一样，只有显式注册并
开启之后的新任务才上传；不会回传历史任务。请在告知被监控者和确认保留
政策后启用，不要把设备令牌、任务证据或服务端凭据提交到 Git。

配置路径：`%LOCALAPPDATA%\AgentReins\CloudAudit\device.json`。
该文件由企业审计服务的设备注册流程提供，示意结构如下（值均为占位符）：

```json
{
  "endpoint": "https://audit.example.com/api/audit/ingest",
  "deviceID": "registered-windows-device-id",
  "token": "registered-device-token",
  "enabledAt": "2026-09-27T00:00:00Z",
  "uploadEnabled": false
}
```

将 `enabledAt` 设为服务端为这台设备登记的生效时间；默认
`uploadEnabled: false`。客户端只接受 HTTPS，不跟随跳转，不使用 SSH
凭据作为上传令牌。启用后，桌面进程每 15 秒检查一次已发布证据，
在本地保留待发包；收到服务端持久化回执后才清除待发包。失败采用退避
重试，同一包的序号和正文保持不变，以便服务端幂等处理。

当前实现使用服务端兼容的单包协议；单个任务快照不能超过 64 MiB。
超过时会明确报错、保留本机原始证据，不会静默截断。Mac 端的分块协议
尚未移植，不能据此宣称与 Mac 端采集能力完全等同。

服务器管理员可用 `scripts/enroll-windows-audit-device.py` 为 Windows 单独
注册设备。它拒绝覆盖已有客户端配置、保留其他设备、备份服务端配置，
只输出设备 ID 和配置文件路径，不在终端打印令牌。注册后重启审计服务，
再通过安全通道将生成的 `device.json` 交付到上述 Windows 路径，
检查文件仅当前用户可读，最后由用户将 `uploadEnabled` 改为 `true`。
不要把 Mac 的设备令牌复制给 Windows。

EXE 安装包必须在 Windows 上完成安装、启动、真实 WorkBuddy 新任务、
服务端回执、断网重试、卸载验证后再发布。
