"""思考深度透传单元测试：Anthropic thinking / OpenAI Responses reasoning → 上游参数。

覆盖 thinking_mode 全部模式（auto / passthrough / effort / enable_thinking / none）、
auto 推断（dashscope / deepseek / kimi / 其他网关）与边界（disabled / 无预算）。

运行方式：
    python -m pytest test_thinking.py -v
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from proxy import (
    Account,
    Service,
    _apply_reasoning_to_chat,
    _apply_thinking_to_chat,
    _budget_to_effort,
    _infer_thinking_style,
    _responses_to_chat,
    convert_request,
)


def make_service(url="https://api.example.com/v1", model="gpt-x", mode="auto"):
    acc = Account(id="a1", name="a1", api_key="k", openai_url=url)
    return Service(
        name="s1", account=acc, client="auto", host="127.0.0.1",
        port=18000, model=model, thinking_mode=mode,
    )


# ---------- budget_tokens → reasoning_effort ----------

def test_budget_to_effort_thresholds():
    assert _budget_to_effort(512) == "low"
    assert _budget_to_effort(2047) == "low"
    assert _budget_to_effort(2048) == "medium"
    assert _budget_to_effort(8191) == "medium"
    assert _budget_to_effort(8192) == "high"
    assert _budget_to_effort(32000) == "high"
    assert _budget_to_effort(None) is None
    assert _budget_to_effort(0) is None
    assert _budget_to_effort("abc") is None


# ---------- auto 推断 ----------

def test_infer_style_by_url():
    assert _infer_thinking_style(
        make_service("https://dashscope.aliyuncs.com/compatible-mode/v1")
    ) == "enable_thinking"
    assert _infer_thinking_style(make_service("https://api.deepseek.com")) == "passthrough"
    assert _infer_thinking_style(make_service("https://api.moonshot.cn/v1")) == "passthrough"
    assert _infer_thinking_style(make_service("https://api.kimi.com/v1")) == "passthrough"
    assert _infer_thinking_style(
        make_service("https://opencode.ai/zen/v1/chat/completions")
    ) == "effort"
    assert _infer_thinking_style(make_service("https://api.example.com/v1", model="qwen3-max")) == "enable_thinking"
    assert _infer_thinking_style(make_service("https://api.example.com/v1", model="kimi-k2-thinking")) == "passthrough"


# ---------- Anthropic thinking → Chat ----------

def test_thinking_passthrough_keeps_budget():
    svc = make_service(mode="passthrough")
    chat = {}
    _apply_thinking_to_chat(chat, {"type": "enabled", "budget_tokens": 32000}, svc)
    assert chat == {"thinking": {"type": "enabled", "budget_tokens": 32000}}


def test_thinking_passthrough_disabled():
    svc = make_service(mode="passthrough")
    chat = {}
    _apply_thinking_to_chat(chat, {"type": "disabled"}, svc)
    assert chat == {"thinking": {"type": "disabled"}}


def test_thinking_effort_mapping():
    svc = make_service(mode="effort")
    chat = {}
    _apply_thinking_to_chat(chat, {"type": "enabled", "budget_tokens": 32000}, svc)
    assert chat == {"reasoning_effort": "high"}
    chat = {}
    _apply_thinking_to_chat(chat, {"type": "enabled", "budget_tokens": 1024}, svc)
    assert chat == {"reasoning_effort": "low"}
    # enabled 无预算 → medium 兜底（显式开启思考）
    chat = {}
    _apply_thinking_to_chat(chat, {"type": "enabled"}, svc)
    assert chat == {"reasoning_effort": "medium"}
    # disabled → 忽略（OpenAI 系无关闭语义，由模型默认决定）
    chat = {}
    _apply_thinking_to_chat(chat, {"type": "disabled", "budget_tokens": 32000}, svc)
    assert chat == {}


def test_thinking_enable_thinking_bool():
    svc = make_service(mode="enable_thinking")
    chat = {}
    _apply_thinking_to_chat(chat, {"type": "enabled", "budget_tokens": 32000}, svc)
    assert chat == {"enable_thinking": True}
    chat = {}
    _apply_thinking_to_chat(chat, {"type": "disabled"}, svc)
    assert chat == {"enable_thinking": False}


def test_thinking_auto_inference():
    # dashscope → 布尔开关（深度由模型默认）
    svc = make_service("https://dashscope.aliyuncs.com/compatible-mode/v1", mode="auto")
    chat = {}
    _apply_thinking_to_chat(chat, {"type": "enabled", "budget_tokens": 32000}, svc)
    assert chat == {"enable_thinking": True}
    # deepseek → 原样对象（保留 budget，由上游消费）
    svc = make_service("https://api.deepseek.com", mode="auto")
    chat = {}
    _apply_thinking_to_chat(chat, {"type": "enabled", "budget_tokens": 32000}, svc)
    assert chat == {"thinking": {"type": "enabled", "budget_tokens": 32000}}
    # 其他网关 → effort 档位
    svc = make_service("https://opencode.ai/zen/v1/chat/completions", mode="auto")
    chat = {}
    _apply_thinking_to_chat(chat, {"type": "enabled", "budget_tokens": 32000}, svc)
    assert chat == {"reasoning_effort": "high"}


def test_thinking_none_mode():
    svc = make_service(mode="none")
    chat = {"model": "x"}
    _apply_thinking_to_chat(chat, {"type": "enabled", "budget_tokens": 32000}, svc)
    assert chat == {"model": "x"}


# ---------- Responses reasoning → Chat ----------

def test_reasoning_to_chat_effort():
    svc = make_service(mode="effort")
    chat = {}
    _apply_reasoning_to_chat(chat, {"reasoning": {"effort": "high"}}, svc)
    assert chat == {"reasoning_effort": "high"}
    # 顶层标量（部分客户端直接发 reasoning_effort）
    chat = {}
    _apply_reasoning_to_chat(chat, {"reasoning_effort": "low"}, svc)
    assert chat == {"reasoning_effort": "low"}


def test_reasoning_to_chat_passthrough_and_bool():
    svc = make_service(mode="passthrough")
    chat = {}
    _apply_reasoning_to_chat(chat, {"reasoning": {"effort": "high"}}, svc)
    assert chat == {"thinking": {"type": "enabled"}}
    svc = make_service(mode="enable_thinking")
    chat = {}
    _apply_reasoning_to_chat(chat, {"reasoning": {"effort": "high"}}, svc)
    assert chat == {"enable_thinking": True}
    # 无 effort → 不动
    chat = {"model": "x"}
    _apply_reasoning_to_chat(chat, {"reasoning": {}}, svc)
    assert chat == {"model": "x"}


def test_reasoning_to_chat_none():
    svc = make_service(mode="none")
    chat = {}
    _apply_reasoning_to_chat(chat, {"reasoning": {"effort": "high"}}, svc)
    assert chat == {}


# ---------- 集成 ----------

def test_convert_request_with_thinking():
    svc = make_service("https://api.example.com/v1", mode="effort")
    req = {
        "model": "claude-sonnet",
        "max_tokens": 4096,
        "messages": [{"role": "user", "content": "hi"}],
        "thinking": {"type": "enabled", "budget_tokens": 32000},
    }
    out = convert_request(req, svc)
    assert out["reasoning_effort"] == "high"
    assert out["messages"] == [{"role": "user", "content": "hi"}]


def test_convert_request_without_thinking():
    svc = make_service("https://api.example.com/v1", mode="effort")
    req = {
        "model": "claude-sonnet",
        "max_tokens": 4096,
        "messages": [{"role": "user", "content": "hi"}],
    }
    out = convert_request(req, svc)
    assert "reasoning_effort" not in out


def test_responses_to_chat_with_reasoning():
    svc = make_service("https://api.example.com/v1", mode="effort")
    req = {
        "model": "gpt-5",
        "input": "hello",
        "reasoning": {"effort": "high"},
    }
    out = _responses_to_chat(req, svc)
    assert out["reasoning_effort"] == "high"
    assert out["messages"] == [{"role": "user", "content": "hello"}]


def test_chat_direct_passthrough_keeps_reasoning_effort():
    """api=openai-completions 直通：无 input 的 Chat 格式入参整包透传，reasoning_effort 原样保留。"""
    svc = make_service("https://api.example.com/v1", mode="none")
    req = {
        "model": "gpt-5",
        "messages": [{"role": "user", "content": "hi"}],
        "reasoning_effort": "high",
    }
    out = _responses_to_chat(req, svc)
    assert out["reasoning_effort"] == "high"
    assert out["model"] == "gpt-x"  # override_model=True 默认用服务模型
