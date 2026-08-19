"""pytest 集中夹具：把项目根目录加入 sys.path，使 tests/ 下以普通脚本方式也能 import proxy/ proxy_async。"""

import os
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
if ROOT not in sys.path:
    sys.path.insert(0, ROOT)