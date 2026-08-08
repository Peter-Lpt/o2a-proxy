#!/bin/bash
# o2a-proxy 多服务代理启动脚本
# 每个 service 一个端口，mode 为 claude(Anthropic 转换) 或 codex(OpenAI 透传)

set -euo pipefail

PROXY_DIR="$(cd "$(dirname "$0")" && pwd)"
CONFIG_FILE="${PROXY_DIR}/config.json"

echo "============================================"
echo "  o2a-proxy 多服务代理"
echo "============================================"
echo ""

# 检查配置文件
if [ ! -f "$CONFIG_FILE" ]; then
    echo "错误: 配置文件不存在: $CONFIG_FILE"
    exit 1
fi

# 列出启用服务并校验 API key
echo "启用服务:"
python3 - "$CONFIG_FILE" <<'PYEOF'
import json, sys
config = json.load(open(sys.argv[1]))
for svc in config.get("services", []):
    mode = svc.get("mode", "claude")
    if mode not in ("claude", "codex"):
        continue
    port = svc.get("listen_address", "8317")
    if not svc.get("openai_api_key"):
        raise SystemExit(f"错误: 服务 {svc.get('comment','?')} 缺少 openai_api_key")
    print(f"  [{mode}] {svc.get('comment','?')} -> http://127.0.0.1:{port}")
    print(f"         target={svc.get('openai_base_url')}  model={svc.get('model')}")
PYEOF
echo ""

# 检查端口占用并清理旧进程
for port in $(python3 - "$CONFIG_FILE" <<'PYEOF'
import json, sys
config = json.load(open(sys.argv[1]))
for svc in config.get("services", []):
    if svc.get("mode", "claude") in ("claude", "codex"):
        print(svc.get("listen_address", "8317"))
PYEOF
); do
    if lsof -Pi :${port} -sTCP:LISTEN >/dev/null 2>&1; then
        echo "警告: 端口 ${port} 已被占用，正在停止旧进程..."
        pids=$(lsof -ti :${port} 2>/dev/null) && [ -n "$pids" ] && echo "$pids" | xargs kill -9 2>/dev/null
        sleep 1
    fi
done

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

# 取第一个 claude 模式服务，更新 Claude 客户端配置
CLAUDE_SETTINGS="${CLAUDE_SETTINGS:-.claude/settings.json}"
CLAUDE_URL=$(python3 - "$CONFIG_FILE" <<'PYEOF'
import json, sys
config = json.load(open(sys.argv[1]))
for svc in config.get("services", []):
    if svc.get("mode", "claude") == "claude":
        print(f"http://127.0.0.1:{svc.get('listen_address','8317')}")
        break
PYEOF
)

if [ -n "$CLAUDE_URL" ]; then
    echo "📝 更新 Claude 代理配置..."
    update_claude_settings "$CLAUDE_SETTINGS" "$CLAUDE_URL"
    echo ""
fi

# 启动代理
echo "启动代理..."
echo "按 Ctrl+C 停止代理"
echo ""
exec python3 "$PROXY_DIR/proxy_async.py"