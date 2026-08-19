"""o2a-proxy 包的公共基础：日志、环境变量常量、项目根定位与路径解析。

从原 proxy.py 顶部抽取：任何模块（config / convert / stats / engine）都会依赖本模块，
故独立成层避免循环导入。
"""

import json
import logging
import os

# fcntl is Unix-only, provide fallback for Windows
try:
    import fcntl
    HAS_FCNTL = True
except ImportError:
    HAS_FCNTL = False

# 配置
LISTEN_HOST = os.environ.get("PROXY_HOST", "127.0.0.1")
LISTEN_PORT = int(os.environ.get("PROXY_PORT", "8317"))
TARGET_URL = os.environ.get(
    "DASHSCOPE_URL",
    "https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions"
)
API_KEY = os.environ.get("DASHSCOPE_API_KEY", "")
PROXY = os.environ.get("HTTP_PROXY", "")
PROXY_MODEL = os.environ.get("PROXY_MODEL", "qwen-plus")
# 最大输出 token 数（mac 客户端「1M 上下文」开关会将其设为 1,000,000）
PROXY_MAX_TOKENS = int(os.environ.get("PROXY_MAX_TOKENS", "4096"))
# 流式响应总超时（秒），防止模型长时间卡在推理阶段
STREAM_TIMEOUT = int(os.environ.get("STREAM_TIMEOUT", "600"))

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(levelname)s] %(message)s",
)
logger = logging.getLogger("proxy")


def _project_root():
    """定位项目根目录（含 proxy.py 兼容层的目录）。

    从本文件所在目录向上查找：源码布局 root/proxy.py + root/o2a/ 时立即命中；
    绿色/便携版布局 <exeDir>/proxy.py + <exeDir>/o2a/ 同样命中；
    桌面端/引擎子进程均依赖该约定（与 desktop/src-tauri 的 find_root 一致）。
    """
    d = os.path.dirname(os.path.abspath(__file__))
    for _ in range(20):
        if os.path.exists(os.path.join(d, "proxy.py")):
            return d
        parent = os.path.dirname(d)
        if parent == d:
            break
        d = parent
    return os.path.dirname(os.path.abspath(__file__))


PROJECT_ROOT = _project_root()


def _default_script_path(filename):
    """默认配置目录 = 项目根（proxy.py 所在目录），Windows/macOS 一致。"""
    return os.path.join(PROJECT_ROOT, filename)


def _resolve_config_path(filename, env_name):
    """解析配置文件路径：优先环境变量（O2A_CONFIG / O2A_AUTH），否则项目根。

    - 环境变量指向已存在目录或以分隔符结尾 → 视为目录，取目录下 filename
    - 环境变量指向具体文件 → 直接用
    - 未设置 → 项目根（proxy.py 所在目录）
    """
    env = os.environ.get(env_name, "").strip()
    if env:
        if os.path.isdir(env) or env.endswith(("/", "\\")):
            return os.path.join(env, filename)
        return env
    return _default_script_path(filename)


def _config_file_path():
    """config.json 路径：O2A_CONFIG 可指定文件或目录（目录时取目录下 config.json）。"""
    return _resolve_config_path("config.json", "O2A_CONFIG")


def _auth_file_path():
    """auth.json 路径：默认跟随 config.json 所在目录（整套配置一起迁移）；
    也可用 O2A_AUTH 单独指定文件或目录。"""
    if os.environ.get("O2A_AUTH", "").strip():
        return _resolve_config_path("auth.json", "O2A_AUTH")
    return os.path.join(os.path.dirname(_config_file_path()), "auth.json")


def _normalize_openai_url(url):
    """OpenAI 端点归一化为完整 chat/completions 地址（o2a 出口统一走 Chat）。

    - "https://api.deepseek.com"        -> "https://api.deepseek.com/chat/completions"
    - "https://.../compatible-mode/v1"  -> "https://.../compatible-mode/v1/chat/completions"
    - "https://.../v1/chat/completions" -> 原样（已是完整地址）
    """
    url = (url or "").strip().rstrip("/")
    if not url or url.endswith("/chat/completions"):
        return url
    return url + "/chat/completions"


def _responses_url(chat_url):
    """从归一化的 chat/completions 地址推导同基座的 responses 端点。

    - https://api.deepseek.com/chat/completions      -> https://api.deepseek.com/v1/responses
    - https://x.com/v1/chat/completions              -> https://x.com/v1/responses
    """
    base = chat_url[: -len("/chat/completions")] if chat_url.endswith("/chat/completions") else chat_url.rstrip("/")
    if base.endswith("/v1"):
        return base + "/responses"
    return base + "/v1/responses"