# 图标来源

2026-10-24 ZCode 额度图标：`zcode.png` 来自 Magpie 原始品牌资源 https://github.com/yetone/magpie/blob/main/internal/gui/assets/icons/zcode.png ，保留 PNG 原样，仅用于标识 ZCode 供应商。

品牌资源来自 `@lobehub/icons-static-svg@1.95.1`，新增的 WorkBuddy 与 GitHub Copilot 供应商图标来自 `1.97.0`，遵循本目录的 [MIT License](LICENSE)。

2026-10-22 额度供应商图标：`workbuddy-color.svg` 来自 https://unpkg.com/@lobehub/icons-static-svg@1.97.0/icons/workbuddy-color.svg ，保留原始彩色；`githubcopilot.svg` 来自 https://unpkg.com/@lobehub/icons-static-svg@1.97.0/icons/githubcopilot.svg ，`currentColor` 固定为 `#f2f0f2`。额度按 Magpie 订阅页映射：codex/chatgpt-api → openai，workbuddy/workbuddy-ai → workbuddy-color，grok → xai，gemini → geminicli-color，copilot → githubcopilot；其余已知供应商沿用本地图标，未知显示中性符号。

彩色资源保留上游 SVG 原样：

| 文件 | 上游地址 |
| --- | --- |
| `claude-color.svg` | https://unpkg.com/@lobehub/icons-static-svg@1.95.1/icons/claude-color.svg |
| `claudecode-color.svg` | https://unpkg.com/@lobehub/icons-static-svg@1.95.1/icons/claudecode-color.svg |
| `codex-color.svg` | https://unpkg.com/@lobehub/icons-static-svg@1.95.1/icons/codex-color.svg |
| `gemini-color.svg` | https://unpkg.com/@lobehub/icons-static-svg@1.95.1/icons/gemini-color.svg |
| `deepseek-color.svg` | https://unpkg.com/@lobehub/icons-static-svg@1.95.1/icons/deepseek-color.svg |

单色标志：

| 文件 | 上游地址 |
| --- | --- |
| `zai.svg` | https://unpkg.com/@lobehub/icons-static-svg@1.95.1/icons/zai.svg |

2026-10-22 GLM、智谱与 Z.ai 统一使用 `zai.svg`：智谱 2025 年品牌升级后官方站点（z.ai、zhipuai.cn、docs.z.ai）只保留斜体 Z 标志，2025 年之前的蓝色点环 `zhipu-color.svg`、`zhipu.svg` 已停用并删除。彩色资源保留上游 SVG 原样，单色标志的 `currentColor` 固定为适合深色背景的 `#f2f0f2`。运行时所有图标均从本地加载。

2026-10-21 按 Magpie 供应商清单补齐：小米 MiMo（`xiaomimimo.svg`，上游无彩色版）、Qwen、Kimi、MiniMax、Mistral、StepFun、Groq、Ollama、OpenRouter、Copilot、Vertex AI、Bedrock、Azure、百度、腾讯云、华为云、火山引擎、豆包、Z.ai、Together、Fireworks、SiliconFlow、NVIDIA、ModelScope、Cline、OpenCode、Cloudflare、Workers AI、Nous、Antigravity、Gemini CLI、xAI、302.AI、AiHubMix、Command Code、Kilo、Cherry Studio、Fish Audio、百炼、混元。
