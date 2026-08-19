#!/usr/bin/env python3
"""o2a-proxy 核心库兼容入口（shim）。

真实实现已在 o2a/ 包中（base.py / config.py / convert.py / stats.py）。
保留本文件：桌面端路径探测（find_root）、绿色版组装、旧导入方式（from proxy import ...）均依赖它。

直接运行会提示使用引擎入口：
    python proxy_async.py [--service <名称|端口>]
"""

import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from o2a import *  # noqa: F401,F403  re-export 全部公开符号
from o2a import __version__  # noqa: F401

# 私有符号（下划线开头不进 __all__，显式补导出，兼容旧代码 from proxy import _xxx 的用法）
from o2a.base import (  # noqa: F401
    _auth_file_path,
    _config_file_path,
    _default_script_path,
    _normalize_openai_url,
    _project_root,
    _resolve_config_path,
    _responses_url,
)
from o2a.config import (  # noqa: F401
    _OPENAI_API_VALUES,
    _THINKING_MODES,
    _UPSTREAM_API_VALUES,
    _resolve_api_key,
)
from o2a.convert import (  # noqa: F401
    _ResponsesStreamTranslator,
    _anthropic_stop_reason,
    _apply_reasoning_to_chat,
    _apply_thinking_to_chat,
    _budget_to_effort,
    _chat_to_responses_json,
    _chat_usage_to_responses,
    _convert_usage,
    _extract_text,
    _infer_thinking_style,
    _responses_content_to_text,
    _responses_to_chat,
    _strip_cache_control,
    _tool_choice_any,
    _to_int,
    convert_tool_input,
)
from o2a.stats import _stats, _stats_lock  # noqa: F401


if __name__ == "__main__":
    print("proxy.py 现为核心库（协议转换 / 配置 / 统计），不包含代理引擎。")
    print("请运行：python proxy_async.py [--service <名称|端口>]")
    sys.exit(1)