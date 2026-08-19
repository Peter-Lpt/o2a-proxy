#!/usr/bin/env python3
"""o2a-proxy 异步引擎兼容入口（shim）。

真实实现已移入 o2a/engine.py（原 proxy_async.py 整体迁移）。
保留本文件：桌面端启动子进程、start-proxy.sh 与旧测试（import proxy_async）均依赖它。

用法：
    python proxy_async.py [--service <comment|port>] [--config <路径|目录>] [--auth <路径|目录>]
    python -m o2a [--service <comment|port>] [--config <路径|目录>] [--auth <路径|目录>]
"""

import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from o2a.engine import *  # noqa: F401,F403  re-export 引擎全部公开符号
from o2a.engine import main  # noqa: F401
from o2a.config import Service  # noqa: F401  （测试等旧代码 from proxy_async 引用）


if __name__ == "__main__":
    main()