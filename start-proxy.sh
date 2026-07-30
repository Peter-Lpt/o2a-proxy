#!/bin/bash
# Anthropic -> OpenAI 协议转换代理启动脚本

set -euo pipefail

PROXY_DIR="$(cd "$(dirname "$0")" && pwd)"
CONFIG_FILE="${PROXY_DIR}/config.json"

echo "============================================"
echo "  Anthropic -> OpenAI 协议转换代理"
echo "============================================"
echo ""

# 检查配置文件
if [ ! -f "$CONFIG_FILE" ]; then
    echo "错误: 配置文件不存在: $CONFIG_FILE"
    exit 1
fi

# 从 config.json 读取配置
eval "$(python3 "$PROXY_DIR/load_config.py" "$CONFIG_FILE")"

PROXY_URL="http://${PROXY_HOST}:${PROXY_PORT}"
CLAUDE_SETTINGS="${CLAUDE_SETTINGS:-.claude/settings.json}"

# 检查 API Key
if [ -z "${DASHSCOPE_API_KEY:-}" ]; then
    echo "错误: DASHSCOPE_API_KEY 未配置"
    exit 1
fi

# 检查端口占用
if lsof -Pi :${PROXY_PORT} -sTCP:LISTEN >/dev/null 2>&1; then
    echo "警告: 端口 ${PROXY_PORT} 已被占用"
    echo "正在停止旧进程..."
    pids=$(lsof -ti :${PROXY_PORT} 2>/dev/null) && [ -n "$pids" ] && echo "$pids" | xargs kill -9 2>/dev/null
    sleep 1
fi

# ============================================
# 自动更新 .claude/settings.json 代理配置
# ============================================
update_claude_settings() {
    local settings_file="${1}"
    local proxy_url="${2}"

    python3 - "$settings_file" "$proxy_url" <<'PYEOF'
import json
import sys
import os
import shutil

settings_file = sys.argv[1]
proxy_url = sys.argv[2]

# 要写入的代理配置 (key -> value)
proxy_config = {
    "ANTHROPIC_BASE_URL": proxy_url,
}

# 读取或初始化 JSON
if os.path.exists(settings_file):
    with open(settings_file, 'r') as f:
        content = f.read().strip()
        if content:
            try:
                config = json.loads(content)
            except json.JSONDecodeError as e:
                print(f"  ⚠️  配置文件 JSON 格式错误，已备份到 .bak: {e}")
                shutil.copy2(settings_file, settings_file + '.bak')
                config = {}
        else:
            config = {}
else:
    config = {}

# 确保 env 节点存在
if "env" not in config or not isinstance(config.get("env"), dict):
    config["env"] = {}

env = config["env"]
changed = False

for key, value in proxy_config.items():
    backup_key = f"_prev_{key}"

    if key in env:
        if str(env[key]) == str(value):
            # 值相同，无需修改
            continue
        # 值不同 —— 将旧值保存到 _prev_ 键（等效于"注释掉"）
        old_val = env[key]
        env[backup_key] = old_val
        print(f"  ✏️  {key}: \"{old_val}\" → \"{value}\"  (旧值已保存为 {backup_key})")
        env[key] = value
        changed = True
    else:
        # 新配置，直接写入
        env[key] = value
        print(f"  ✅ {key}: \"{value}\"")
        changed = True

if changed:
    # 写入前备份
    if os.path.exists(settings_file):
        shutil.copy2(settings_file, settings_file + '.bak')

    settings_dir = os.path.dirname(settings_file)
    if settings_dir:
        os.makedirs(settings_dir, exist_ok=True)
    with open(settings_file, 'w') as f:
        json.dump(config, f, indent=2, ensure_ascii=False)
        f.write('\n')
    print(f"  💾 配置已写入: {settings_file}")
else:
    print(f"  ✔️  配置已是最新，无需修改")

# 校验写入结果
try:
    with open(settings_file, 'r') as f:
        json.loads(f.read())
    print(f"  ✅ 配置校验通过")
except Exception as e:
    print(f"  ❌ 配置校验失败: {e}")
    sys.exit(1)
PYEOF
}

echo "📝 更新 Claude 代理配置..."
update_claude_settings "$CLAUDE_SETTINGS" "$PROXY_URL"
echo ""

# 启动代理
echo "启动代理: ${PROXY_URL}"
echo "目标: ${DASHSCOPE_URL}"
echo "主 agent 模型: ${PROXY_MODEL}"
echo "子 agent 模型: ${SUB_PROXY_MODEL:-${PROXY_MODEL}}"
echo "按 Ctrl+C 停止代理"
echo ""

export PROXY_HOST PROXY_PORT DASHSCOPE_URL DASHSCOPE_API_KEY PROXY_MODEL SUB_PROXY_MODEL
exec python3 "$PROXY_DIR/proxy.py"
