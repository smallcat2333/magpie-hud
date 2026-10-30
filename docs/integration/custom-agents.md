# 自定义 Agent 接入

## 来源图标规则

HUD 从应用内的 `ui/source-icons.json` 读取规则。以下是通用示例，可在本地加入自己的来源名称与图标：

```json
{
  "version": 1,
  "fallback": "icons/robot.svg",
  "rules": [
    {
      "agents": ["Example Agent", "example-agent", "example_agent"],
      "icon": "icons/robot.svg"
    }
  ]
}
```

`agents` 是来源名称的精确别名列表。比较时忽略大小写、空格、下划线和连字符，先匹配的规则生效。`icon` 和 `fallback` 只允许 `icons/文件名.svg`、`.png` 或 `.webp`，图标文件放在 `ui/icons/`；不支持网络地址或任意磁盘路径。规则优先于已有模型品牌映射，未知来源使用机器人图标，全部来源选项仍使用星号。规则和图标随 exe 打包，修改后需要重新构建；不是安装目录中的运行时外置配置。

公共仓库只包含通用图标与公开产品的匹配规则。自建 Agent 的专属图片、名称规则和部署信息应保留在自己的本地配置或私有构建中；发布前同时检查源码和打包资源。

## 对话标题文件（协议版本 1）

自定义 Agent 写入运行 HUD 的本机用户目录，每个生产者使用独立文件：

- Windows：`%APPDATA%\magpie-hud\session-titles\example-agent.json`
- macOS：`~/Library/Application Support/magpie-hud/session-titles/example-agent.json`

UTF-8 JSON 示例：

```json
{
  "version": 1,
  "agent": "Example Agent",
  "sessions": [
    {
      "session": "example-session-1",
      "title": "示例任务 / 问题排查",
      "updatedAt": "2026-01-01T12:00:00+08:00"
    }
  ]
}
```

- `version`：必填，固定为整数 `1`。
- `agent`：必填，来源名称，例如 `Example Agent`；它说明文件由谁提供，不能代替请求携带的来源标记。
- `sessions`：必填，为该生产者维护的完整标题快照。同一文件中每个 `session` 仅一条。
- `session`：必填，必须逐字等于该对话在 Magpie `/v1/magpie/trace` 和 `usage.jsonl` 中的 `session`。推荐 UUID；长度不超过 256，仅 ASCII 字母、数字、`-`、`_`。同一对话的多次请求继续使用同一个 ID，新建或重置对话时用新 ID。
- `title`：必填，对话短标题，不是消息正文。空白会合并为单个空格，最长展示 160 个字符；空标题忽略。
- `updatedAt`：可选，ISO 8601 时间，供生产者维护；HUD 当前不据此排序。

HUD 大约每 2 秒读取一次，仅用于补全标题，不新增会话、不改统计、不读取聊天正文。每个文件最多 1 MiB、最多 4096 条标题；目录最多读取 128 个 `.json` 文件。缺文件时正常跳过，文件短暂写坏时保留上次有效数据，删除文件后移除相应标题。重复 ID 的冲突标题不随意选取；已有 Claude/Codex 官方标题优先。

每个生产者只写自己的文件，不覆盖其他文件。更新时先将完整快照写到同目录临时文件（扩展名用 `.tmp`），完成后原子替换正式 JSON，避免读到半份文件。不能写 API Key、账号密码、聊天消息或用户输入全文。不要让 Windows 服务写到 SYSTEM 的 `%APPDATA%`：若服务运行用户不同，需显式配置 HUD 所在用户的目标目录及写入权限。

验收：发送两次用户输入，确认 trace 的 `agent` 为生产者自己的来源名称、两次 `session` 相同；文件中的 session 完全一致，标题与实际对话对应；重命名后约 2 秒可刷新；重置对话生成新 ID，不能串到旧对话。

## 跨电脑部署限制

本协议是本机标题文件协议，不会自动同步远端文件。HUD 展示标题、路由、状态和用量，不读取或展示聊天正文。

| 部署方式 | HUD 可见性 |
| --- | --- |
| Agent 远端运行，仍请求 HUD 电脑上的 Magpie | 本机 Magpie 仍有请求和账本时可显示路由、模型及用量；会话标题依赖 HUD 用户目录中的本地标题文件。 |
| Agent 和 Magpie 都迁到远端，HUD 留在原电脑 | HUD 当前不会自动连接远端网关或读取远端账本，无法看到新的远端请求。 |
| HUD 与 Magpie 一起迁移 | 可读取迁移后本机的网关、账本；标题文件需写到新电脑上运行 HUD 的用户目录。旧记录是否保留取决于账本和标题快照是否迁移。 |

当前版本没有实现远程标题拉取、远端元数据接入或自动迁移；仅在服务器自己的 `%APPDATA%` 写文件不会让另一台电脑上的 HUD 获得标题。
