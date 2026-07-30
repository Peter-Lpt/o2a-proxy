# o2a-proxy

Anthropic → OpenAI 协议转换代理，支持 macOS 菜单栏客户端。

将 Anthropic API 格式的请求转换为 OpenAI 兼容格式，支持阿里云 DashScope、DeepSeek、Kimi 等国内模型服务。

## 特性

- **协议转换**：Anthropic Messages API → OpenAI Chat Completions API
- **流式响应**：完整支持 SSE 流式输出
- **缓存统计**：实时统计缓存命中率、Token 消耗、费用估算
- **macOS 客户端**：菜单栏应用，悬浮看板，实时查看请求状态
- **费用统计**：内置多平台定价数据，自动计算费用
- **配置管理**：JSON 配置文件，支持多服务配置

## 架构

```
Claude Code / Claude Desktop
        ↓ (Anthropic Messages API)
    o2a-proxy (proxy.py)
        ↓ (OpenAI Chat Completions API)
    DashScope / DeepSeek / Kimi
```

## 安装

### 1. 克隆仓库

```bash
git clone https://github.com/yourusername/o2a-proxy.git
cd o2a-proxy
```

### 2. 配置

复制配置模板：

```bash
cp config.example.json config.json
```

编辑 `config.json`：

```json
{
  "auth_token": "your-auth-token",
  "cache_stats_enabled": true,
  "cache_stats_dir": "cache_stats",
  "cache_stats_retention_days": 30,
  "services": [
    {
      "comment": "阿里云 DashScope",
      "model": "qwen-plus",
      "sub_model": "qwen-plus",
      "listen_address": "11011",
      "openai_base_url": "https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions",
      "openai_api_key": "sk-your-api-key"
    }
  ]
}
```

**配置说明：**

- `auth_token`：认证令牌（可选，用于保护代理接口）
- `cache_stats_enabled`：启用缓存统计
- `cache_stats_dir`：统计数据存储目录
- `cache_stats_retention_days`：统计数据保留天数
- `services`：服务配置列表
  - `model`：主模型（用于普通对话）
  - `sub_model`：子模型（用于工具调用等）
  - `listen_address`：监听端口
  - `openai_base_url`：上游 API 地址
  - `openai_api_key`：API Key

### 3. 启动代理

```bash
python3 proxy.py
```

或使用启动脚本：

```bash
./start-proxy.sh
```

### 4. 使用客户端（可选）

macOS 用户可以安装菜单栏客户端：

```bash
cd mac
npm install
npm start
```

## 使用方法

### 配置 Claude Code

设置环境变量：

```bash
export ANTHROPIC_BASE_URL=http://127.0.0.1:11011
export ANTHROPIC_AUTH_TOKEN=your-auth-token
```

或使用配置文件 `~/.claude/settings.json`：

```json
{
  "env": {
    "ANTHROPIC_BASE_URL": "http://127.0.0.1:11011",
    "ANTHROPIC_AUTH_TOKEN": "your-auth-token"
  }
}
```

### 查看统计

命令行查看：

```bash
python3 cache-stats.py day
```

或使用 macOS 客户端查看实时统计。

## 支持的平台

### 阿里云 DashScope

```json
{
  "services": [
    {
      "comment": "aliyun",
      "model": "qwen3.7-plus",
      "sub_model": "qwen3.7-plus",
      "listen_address": "11011",
      "openai_base_url": "https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions",
      "openai_api_key": "sk-your-api-key"
    }
  ]
}
```

其他平台（DeepSeek、Kimi 等）请参考各平台文档配置 `openai_base_url` 和 `openai_api_key`。

## 费用统计

代理内置费用统计功能，基于 `pricing.json` 中的定价数据自动计算费用。

**注意**：费用为估算值，实际费用以平台账单为准。

## 开发

### 项目结构

```
o2a-proxy/
├── proxy.py              # 核心代理逻辑
├── config.json           # 配置文件（不提交）
├── config.example.json   # 配置模板
├── pricing.json          # 模型定价数据
├── cache-stats.py        # 统计查看工具
├── mac/                  # macOS 客户端
│   ├── main.js          # Electron 主进程
│   ├── stats.js         # 统计模块
│   └── renderer/        # 渲染层
└── cache_stats/          # 统计数据（不提交）
```

### 运行测试

```bash
python3 test_cache_stats.py
```

## 贡献

欢迎提交 Issue 和 Pull Request！

### 添加模型定价

编辑 `pricing.json`，按以下格式添加：

```json
{
  "model-name": {
    "category": "text",
    "tiers": [
      { "range": "0-128K", "input": 2, "output": 8, "output_thinking": 8 }
    ],
    "cache": true,
    "batch": true,
    "free_quota": 1000000
  }
}
```

### 代码规范

- Python 代码遵循 PEP 8
- JavaScript 代码使用 ES6+ 语法
- 提交前运行测试

## 许可证

MIT License

## 致谢

- 基于 [openai-proxy](https://github.com/yourusername/openai-proxy) 项目
- 感谢阿里云 DashScope、DeepSeek、Kimi 提供的模型服务

## 常见问题

### Q: 为什么缓存命中率是 0？

A: 部分模型不支持缓存，或请求内容没有重复。

### Q: 费用统计准确吗？

A: 费用为估算值，基于官方定价计算。实际费用以平台账单为准。

### Q: 支持哪些模型？

A: 理论上支持所有 OpenAI 兼容的模型，具体取决于上游服务。

### Q: 如何切换模型？

A: 修改 `config.json` 中的 `model` 和 `sub_model` 字段，重启代理。

## 安全说明

- **不要提交 config.json**：包含 API Key，已添加到 .gitignore
- **使用 auth_token**：生产环境建议启用认证
- **限制访问**：默认只监听 127.0.0.1，不要暴露到公网
