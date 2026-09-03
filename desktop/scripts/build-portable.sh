#!/usr/bin/env bash
# 绿色免安装版打包脚本（Windows）：
# 1. 构建 Rust 引擎二进制（o2a-engine）+ tauri build --no-bundle 产出裸 exe
# 2. 整理为绿色目录：o2a-proxy.exe + o2a-engine + config.example.json + 使用说明
#    （文件夹整体免安装，双击 o2a-proxy.exe 即用；不写注册表，卸载=删文件夹；
#      无需 Python —— 引擎为 Rust 二进制）
# 产物：desktop/dist-portable/o2a-proxy/
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"              # desktop/scripts
DESKTOP="$(cd "$HERE/.." && pwd)"                  # desktop
ROOT="$(cd "$DESKTOP/.." && pwd)"                  # 项目根
TAURI_BIN="$DESKTOP/src-tauri/target/release/o2a-desktop.exe"
ENGINE_BIN="$ROOT/target/release/o2a-engine.exe"
OUT="$DESKTOP/dist-portable/o2a-proxy"

echo "==> 1/3 构建 Rust 引擎二进制（release）..."
cd "$ROOT"
cargo build -p o2a-engine --release
[ -f "$ENGINE_BIN" ] || { echo "构建失败：未找到 $ENGINE_BIN" >&2; exit 1; }

echo "==> 2/3 tauri release 构建（--no-bundle，产物：o2a-desktop.exe）..."
cd "$DESKTOP"
pnpm tauri build --no-bundle

[ -f "$TAURI_BIN" ] || { echo "构建失败：未找到 $TAURI_BIN" >&2; exit 1; }

echo "==> 3/3 组装绿色目录..."
rm -rf "$OUT"
mkdir -p "$OUT"
cp "$TAURI_BIN" "$OUT/o2a-proxy.exe"
cp "$ENGINE_BIN" "$OUT/o2a-engine.exe"
cp "$ROOT/config.example.json" "$OUT/"

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
  - Windows 10/11（自带 WebView2 运行时，无需安装；代理引擎为内置 Rust 二进制，无需 Python）
    · 引擎二进制 o2a-engine.exe 缺失时，桌面端回退系统 Python 3.9+（需 proxy.py + o2a/）

首次使用
  - 无需 config.json：双击运行后在「配置」页添加服务/账号，保存时自动生成配置。
  - 配置文件默认保存在用户目录（%APPDATA%\com.o2aproxy.desktop\），
    不会随程序文件夹丢失；也可在「配置」页的「配置文件位置」卡片自定义。

关于引擎
  - o2a-engine.exe 为内置代理引擎（与 o2a-proxy.exe 同目录，请勿删除）。
  - 如需在桌面端之外单独使用引擎：o2a-engine.exe --service <名称> --config <路径> --auth <路径>
EOF
fi

ls -la "$OUT"
echo "完成：$OUT"
