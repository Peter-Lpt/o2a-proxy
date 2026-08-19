#!/usr/bin/env bash
# 绿色免安装版打包脚本（Windows）：
# 1. tauri build --no-bundle 产出裸 exe + 引擎资源（exe 旁）
# 2. 整理为绿色目录：o2a-proxy.exe + proxy.py + proxy_async.py + config.example.json + 使用说明
#    （文件夹整体免安装，双击 o2a-proxy.exe 即用；不写注册表，卸载=删文件夹）
# 产物：desktop/dist-portable/o2a-proxy/
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"              # desktop/scripts
DESKTOP="$(cd "$HERE/.." && pwd)"                  # desktop
ROOT="$(cd "$DESKTOP/.." && pwd)"                  # 项目根
TAURI_BIN="$DESKTOP/src-tauri/target/release/o2a-desktop.exe"
OUT="$DESKTOP/dist-portable/o2a-proxy"

echo "==> 1/2 tauri release 构建（--no-bundle，产物：o2a-desktop.exe + 引擎资源）..."
cd "$DESKTOP"
pnpm tauri build --no-bundle

[ -f "$TAURI_BIN" ] || { echo "构建失败：未找到 $TAURI_BIN" >&2; exit 1; }

echo "==> 2/2 组装绿色目录..."
rm -rf "$OUT"
mkdir -p "$OUT"
cp "$TAURI_BIN" "$OUT/o2a-proxy.exe"
cp "$ROOT/proxy.py" "$ROOT/proxy_async.py" "$ROOT/config.example.json" "$OUT/"
cp -r "$ROOT/o2a" "$OUT/o2a"

# 使用说明（若不存在则生成）
if [ ! -f "$OUT/使用说明.txt" ]; then
cat > "$OUT/使用说明.txt" <<'EOF'
o2a-proxy 绿色版（免安装）
============================

使用方法
  1. 双击 o2a-proxy.exe 即可运行（托盘图标启动，快捷键 Ctrl+Alt+O 打开面板）。
  2. 整个文件夹可拷到任意位置/U盘使用，不写注册表、不安装任何东西。
  3. 卸载 = 删除整个文件夹。

环境要求
  - Windows 10/11（自带 WebView2 运行时，无需安装）
  - 系统已安装 Python 3.9+（代理引擎为 Python 实现）
    · 若 Python 不在 PATH，可设置环境变量 O2A_PYTHON 指向 python.exe 的完整路径

首次使用
  - 无需 config.json：双击运行后在「配置」页添加服务/账号，保存时自动生成配置。
  - 配置文件默认保存在用户目录（%APPDATA%\com.o2aproxy.desktop\），
    不会随程序文件夹丢失；也可在「配置」页的「配置文件位置」卡片自定义。

关于引擎
  - proxy.py / proxy_async.py 为内置代理引擎入口（与程序同目录，请勿删除）。
  - o2a/ 为代理引擎实现包（与 proxy.py 同目录，请勿删除）。
  - 如需在桌面端之外单独使用引擎：python proxy_async.py --service <名称>
EOF
fi

ls -la "$OUT"
echo "完成：$OUT"
