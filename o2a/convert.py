"""o2a-proxy 协议转换：Anthropic Messages ↔ OpenAI Chat / Responses 互转、透传与思考深度映射。

从原 proxy.py 拆出，逻辑逐字保留。
"""

import json
import time
import uuid

from .base import logger
from .config import Service  # noqa: F401  （类型标注/复用 from typing in future）


def detect_client(request, payload):
    """自动识别入口协议：anthropic（Claude Code）还是 openai（Codex）。

    先看路径（/v1/messages、/v1/responses、/chat/completions），
    再看请求体特征（Anthropic 必有 max_tokens/system，OpenAI Responses 有 input）。
    """
    path = getattr(request, "path", "") or ""
    p = path.lower()
    if "/v1/messages" in p:
        return "anthropic"
    if "/responses" in p or "/chat/completions" in p or "/completions" in p:
        return "openai"
    if isinstance(payload, dict):
        if "input" in payload and "messages" not in payload:
            return "openai"  # OpenAI Responses
        if "max_tokens" in payload and "system" in payload:
            return "anthropic"  # Anthropic Messages
        if "messages" in payload:
            msgs = payload.get("messages") or []
            # Anthropic 的 content 是 block 列表（text/tool_use/tool_result）
            if msgs and isinstance(msgs[0], dict) and isinstance(msgs[0].get("content"), list):
                return "anthropic"
            return "openai"
        if "max_tokens" in payload:
            return "anthropic"
    return "openai"  # 默认


def resolve_mode(service, request=None, payload=None):
    """确定一次请求的分派模式（claude / codex / direct）。

    api 显式声明时直接采用推导结果，不再做请求体猜测（避免误判）；
    client 显式时按旧逻辑推导；auto 时先识别入口协议，再按账号端点选转换或透传。
    返回 None 表示该组合不支持（OpenAI 客户端 + 无 OpenAI 端点的账号）。
    """
    if service.api:
        # api 已显式声明（openai-completions / openai-responses / anthropic-messages）
        return service.mode
    if service.client == "auto":
        client = detect_client(request, payload)
        if client == "anthropic":
            return "direct" if service.kind in ("anthropic", "both") else "claude"
        return "codex" if service.kind != "anthropic" else None
    # 显式 client
    if service.client == "openai":
        return "codex" if service.kind != "anthropic" else None
    # anthropic 客户端
    return "direct" if service.kind in ("anthropic", "both") else "claude"


def sse_event(data, event_type=None):
    """格式化 SSE 事件。"""
    lines = []
    if event_type is None and isinstance(data, dict):
        event_type = data.get("type")
    if event_type:
        lines.append(f"event: {event_type}")
    lines.append(f"data: {json.dumps(data)}")
    lines.append("")
    return "\n".join(lines) + "\n"


def _to_int(value, default=0):
    """Best-effort conversion for provider usage fields."""
    if value is None:
        return default
    try:
        return int(value)
    except (TypeError, ValueError):
        return default


def _convert_usage(usage):
    """Convert OpenAI-compatible usage into Anthropic usage semantics."""
    usage = usage or {}
    prompt_details = usage.get("prompt_tokens_details") or {}
    input_details = usage.get("input_tokens_details") or {}

    prompt_total = _to_int(
        usage.get("prompt_tokens", usage.get("input_tokens", 0))
    )
    output_tokens = _to_int(
        usage.get("completion_tokens", usage.get("output_tokens", 0))
    )

    # DeepSeek 顶层字段：prompt_cache_hit_tokens（命中）/ prompt_cache_miss_tokens（未命中）
    # 命中部分计入缓存读，prompt_total 是全量（含命中），相减后才是真实输入。
    ds_cache_hit = _to_int(usage.get("prompt_cache_hit_tokens", 0))

    cached_tokens = _to_int(
        ds_cache_hit
        or prompt_details.get(
            "cached_tokens",
            prompt_details.get(
                "cache_read_input_tokens",
                input_details.get(
                    "cached_tokens",
                    input_details.get(
                        "cache_read_input_tokens",
                        usage.get("cache_read_input_tokens", usage.get("cached_tokens", 0)),
                    ),
                ),
            ),
        )
    )
    cache_write_tokens = _to_int(
        prompt_details.get(
            "cache_creation_input_tokens",
            prompt_details.get(
                "cache_write_tokens",
                input_details.get(
                    "cache_write_tokens",
                    input_details.get(
                        "cache_creation_input_tokens",
                        usage.get("cache_creation_input_tokens", usage.get("cache_write_tokens", 0)),
                    ),
                ),
            ),
        )
    )

    # Anthropic reports cache writes separately from ordinary input tokens.
    input_tokens = max(0, prompt_total - cached_tokens - cache_write_tokens)

    completion_details = usage.get("completion_tokens_details") or {}
    # Responses 格式的推理 token 在 output_tokens_details.reasoning_tokens
    output_details = usage.get("output_tokens_details") or {}
    reasoning_tokens = _to_int(
        completion_details.get("reasoning_tokens")
        or output_details.get("reasoning_tokens", 0)
    )

    return {
        "input_tokens": input_tokens,
        "output_tokens": output_tokens,
        "cache_creation_input_tokens": cache_write_tokens,
        "cache_read_input_tokens": cached_tokens,
        "reasoning_tokens": reasoning_tokens,
        "prompt_total": prompt_total,
    }


def _anthropic_stop_reason(finish_reason, has_tool_calls=False):
    """Map OpenAI finish_reason values to Anthropic stop_reason values."""
    if has_tool_calls or finish_reason == "tool_calls":
        return "tool_use"
    if finish_reason == "length":
        return "max_tokens"
    if finish_reason in ("stop", None, ""):
        return "end_turn"
    if finish_reason == "content_filter":
        return "stop_sequence"
    return finish_reason


def _extract_text(content):
    """将 Anthropic content blocks 转为纯文本字符串。"""
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        parts = []
        for block in content:
            if isinstance(block, str):
                parts.append(block)
            elif isinstance(block, dict):
                if block.get("type") == "text":
                    parts.append(block.get("text", ""))
                elif block.get("type") == "tool_result":
                    content_val = block.get("content", "")
                    if isinstance(content_val, str):
                        parts.append(content_val)
                    elif isinstance(content_val, list):
                        for cb in content_val:
                            if isinstance(cb, dict) and cb.get("type") == "text":
                                parts.append(cb.get("text", ""))
            else:
                parts.append(str(block))
        return "\n".join(parts)
    return str(content)


def convert_tool_input(input_schema):
    """将 Anthropic input_schema 转为 OpenAI function parameters 格式。"""
    if not isinstance(input_schema, dict):
        return input_schema
    params = dict(input_schema)
    if "type" not in params:
        params["type"] = "object"
    return params


def _strip_cache_control(obj):
    """递归移除 cache_control 字段（DashScope 不支持）。"""
    if isinstance(obj, dict):
        return {k: _strip_cache_control(v) for k, v in obj.items() if k != "cache_control"}
    elif isinstance(obj, list):
        return [_strip_cache_control(item) for item in obj]
    return obj


def normalize_roles(payload):
    """将 OpenAI 特有的 developer 角色规范化为 system（chat messages 与 responses input 均处理）。

    多数非 OpenAI 上游（DeepSeek / Kimi / Qwen 等）的角色枚举不含 developer（DeepSeek
    只认 system / user / assistant / tool 等），透传前统一降级为 system——与
    _responses_to_chat 已有的规范化一致；system 是所有上游都接受的通用角色，且不影响
    reasoning_effort / thinking 等其它字段。返回是否发生修改（未修改时透传保持字节一致）。
    """
    changed = False
    for key in ("messages", "input"):
        items = payload.get(key)
        if not isinstance(items, list):
            continue
        for item in items:
            if isinstance(item, dict) and item.get("role") == "developer":
                item["role"] = "system"
                changed = True
    return changed


def _responses_content_to_text(content):
    """将 Responses API 消息 content parts 提取为纯文本。"""
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        parts = []
        for p in content:
            if isinstance(p, str):
                parts.append(p)
            elif isinstance(p, dict):
                t = p.get("type")
                if "text" in p and isinstance(p.get("text"), str):
                    parts.append(p.get("text"))
                elif t in ("input_text", "output_text"):
                    parts.append(p.get("text", ""))
        return "\n".join(parts)
    return ""


def _responses_to_chat(req, service):
    """将 OpenAI Responses API 请求转成 Chat Completions 请求。

    兼容两种入参格式（Codex / pi 等客户端可能发任一种）：
    - Responses 格式：req 含 input（字符串或 item 数组）
    - Chat Completions 格式：req 含 messages —— 直通，仅做 role 规范化
    """
    messages = []
    pending_calls = []  # 连续 function_call 项合并为一条 assistant 消息

    def flush_calls():
        if pending_calls:
            messages.append({
                "role": "assistant",
                "content": None,
                "tool_calls": list(pending_calls),
            })
            del pending_calls[:]

    if not req.get("input"):
        # Chat Completions 直通：整包透传（保留 stream/tools/stop 等全部字段），仅替换 model、规范化 role
        chat = {k: v for k, v in req.items() if k != "model"}
        msgs = []
        for msg in chat.get("messages", []):
            if not isinstance(msg, dict):
                continue
            m = dict(msg)
            if m.get("role") == "developer":
                m["role"] = "system"
            msgs.append(m)
        chat["messages"] = msgs
        # 模型覆盖开关：默认用服务配置的 model；override_model=false 时透传客户端模型名（缺省回退服务配置）
        if service.override_model:
            chat["model"] = service.model
        else:
            chat["model"] = req.get("model") or service.model
        if not chat.get("max_tokens") and not chat.get("max_output_tokens"):
            # 没带 max_tokens 时用服务默认（不做封顶，透传）
            chat["max_tokens"] = service.max_tokens
        return chat
    else:
        raw_input = req.get("input", [])
        if isinstance(raw_input, str):
            # Responses 规范允许 input 为纯字符串
            raw_input = [{"role": "user", "content": raw_input}]
        for item in raw_input:
            if not isinstance(item, dict):
                continue
            itype = item.get("type")
            if itype == "function_call":
                pending_calls.append({
                    "id": item.get("call_id") or item.get("id") or "",
                    "type": "function",
                    "function": {
                        "name": item.get("name", ""),
                        "arguments": item.get("arguments", ""),
                    },
                })
            elif itype == "function_call_output":
                flush_calls()
                messages.append({
                    "role": "tool",
                    "tool_call_id": item.get("call_id") or item.get("id") or "",
                    "content": item.get("output", ""),
                })
            elif "role" in item:
                flush_calls()
                role = item.get("role")
                if role == "developer":
                    role = "system"
                messages.append({"role": role, "content": _responses_content_to_text(item.get("content", ""))})
    flush_calls()

    instructions = req.get("instructions", "")
    if instructions:
        if messages and messages[0].get("role") == "system":
            # input 已含 system 角色消息时合并，避免产生两条 system
            prev = messages[0].get("content", "") or ""
            messages[0]["content"] = (instructions + "\n\n" + prev) if prev else instructions
        else:
            messages.insert(0, {"role": "system", "content": instructions})

    if service.override_model:
        chat_model = service.model
    else:
        chat_model = req.get("model") or service.model
    chat = {
        "model": chat_model,
        "messages": messages,
        "stream": req.get("stream", False),
    }
    if "max_output_tokens" in req:
        chat["max_tokens"] = req["max_output_tokens"]
    elif "max_tokens" in req:
        chat["max_tokens"] = req["max_tokens"]
    else:
        chat["max_tokens"] = service.max_tokens
    for k in ("temperature", "top_p", "stream_options", "seed", "parallel_tool_calls"):
        if k in req:
            chat[k] = req[k]
    if req.get("stream") and "stream_options" not in chat:
        chat["stream_options"] = {"include_usage": True}

    tools = req.get("tools", [])
    if tools:
        chat_tools = []
        for t in tools:
            if isinstance(t, dict) and t.get("type") == "function":
                chat_tools.append({
                    "type": "function",
                    "function": {
                        "name": t.get("name", ""),
                        "description": t.get("description", ""),
                        "parameters": t.get("parameters", {"type": "object"}) or {"type": "object"},
                        "strict": t.get("strict", False),
                    },
                })
        if chat_tools:
            chat["tools"] = chat_tools

    tool_choice = req.get("tool_choice")
    if tool_choice:
        if isinstance(tool_choice, str):
            chat["tool_choice"] = tool_choice
        elif isinstance(tool_choice, dict):
            chat["tool_choice"] = {
                "type": "function",
                "function": {"name": tool_choice.get("name", "")},
            }
    # 思考深度：Responses reasoning（effort 档位）→ 上游 Chat 参数
    _apply_reasoning_to_chat(chat, req, service)
    return chat


def _chat_usage_to_responses(usage):
    """将 Chat Completions usage 转成 Responses API usage 格式。"""
    usage = usage or {}
    prompt = _to_int(usage.get("prompt_tokens", usage.get("input_tokens", 0)))
    completion = _to_int(usage.get("completion_tokens", usage.get("output_tokens", 0)))
    # 兼容 DeepSeek 顶层缓存字段与 Responses 格式的 details 嵌套
    cached = _to_int(
        usage.get("prompt_cache_hit_tokens", 0)
        or (usage.get("prompt_tokens_details") or {}).get("cached_tokens", 0)
        or (usage.get("input_tokens_details") or {}).get("cached_tokens", 0)
    )
    reasoning = _to_int(
        (usage.get("completion_tokens_details") or {}).get("reasoning_tokens", 0)
        or (usage.get("output_tokens_details") or {}).get("reasoning_tokens", 0)
    )
    return {
        "input_tokens": prompt,
        "input_tokens_details": {"cached_tokens": cached},
        "output_tokens": completion,
        "output_tokens_details": {"reasoning_tokens": reasoning},
        "total_tokens": prompt + completion,
    }


def _chat_to_responses_json(data, model):
    """将 Chat Completions 非流式响应转成 Responses API 响应。"""
    resp_id = "resp_" + uuid.uuid4().hex[:24]
    created = int(time.time())
    output = []
    choice = (data.get("choices") or [{}])[0]
    message = choice.get("message") or {}
    # 文本输出
    text = message.get("content") or ""
    if isinstance(text, list):
        # 上游 content 为 block 列表时转为纯文本，避免 Responses 结构非法
        text = _responses_content_to_text(text)
    if text:
        output.append({
            "id": f"msg_{len(output)}",
            "type": "message",
            "role": "assistant",
            "status": "completed",
            "content": [{"type": "output_text", "text": text, "annotations": []}],
        })
    # 推理内容（reasoning_content -> Responses reasoning item）
    reasoning = message.get("reasoning_content") or ""
    if reasoning:
        output.append({
            "id": f"reasoning_{len(output)}",
            "type": "reasoning",
            "status": "completed",
            "summary": [{"type": "summary_text", "text": reasoning}],
            "content": [{"type": "reasoning_text", "text": reasoning}],
        })
    # 函数调用
    for tc in message.get("tool_calls") or []:
        fn = tc.get("function") or {}
        output.append({
            "id": f"fc_{len(output)}",
            "type": "function_call",
            "status": "completed",
            "name": fn.get("name", ""),
            "call_id": tc.get("id", ""),
            "arguments": fn.get("arguments", ""),
        })
    return {
        "id": resp_id,
        "object": "response",
        "created_at": created,
        "status": "completed",
        "model": model or data.get("model", ""),
        "output": output,
        "parallel_tool_calls": True,
        "tools": [],
        "usage": _chat_usage_to_responses(data.get("usage")),
    }


class _ResponsesStreamTranslator:
    """将 Chat Completions 流式 SSE 翻译为 Responses API 流式 SSE。"""

    def __init__(self, model):
        self.model = model
        self.response_id = "resp_" + uuid.uuid4().hex[:24]
        self.created_at = int(time.time())
        self.output_index = 0
        self._emitted_created = False
        self._finished = False      # 内容 done 事件已发射（幂等）
        self._completed = False     # response.completed 已发射（幂等）
        self._msg_item_id = None
        self._msg_output_index = 0
        self._msg_delivered = False
        self._text = ""
        self._tool_states = {}  # index -> state
        self._tool_order = []
        self._delivered_tool = set()
        self._output_sequence = []  # 按交付(output_index)顺序记录 ('message'|'tool'|'reasoning', key)
        self._reasoning_item_id = None
        self._reasoning_output_index = 0
        self._reasoning_delivered = False
        self._reasoning_text = ""
        self.usage = None

    def _base_response(self):
        return {
            "id": self.response_id,
            "object": "response",
            "created_at": self.created_at,
            "status": "in_progress",
            "model": self.model,
            "output": [],
            "parallel_tool_calls": True,
            "tools": [],
            "usage": self.usage,
        }

    def _ensure_created(self, events):
        if not self._emitted_created:
            self._emitted_created = True
            events.append({"type": "response.created", "response": self._base_response()})

    def _deliver_message(self, events):
        if self._msg_delivered:
            return
        self._msg_delivered = True
        self._msg_item_id = f"msg_{self.output_index}"
        self._msg_output_index = self.output_index
        self.output_index += 1
        self._output_sequence.append(("message", None))
        item = {
            "id": self._msg_item_id,
            "type": "message",
            "role": "assistant",
            "status": "in_progress",
            "content": [],
        }
        events.append({"type": "response.output_item.added",
                       "output_index": self._msg_output_index, "item": item})
        events.append({"type": "response.content_part.added",
                       "item_id": self._msg_item_id,
                       "output_index": self._msg_output_index,
                       "content_index": 0,
                       "part": {"type": "output_text", "text": "", "annotations": []}})

    def _deliver_reasoning(self, events):
        """交付推理 item（reasoning_content -> reasoning）。"""
        if self._reasoning_delivered:
            return
        self._reasoning_delivered = True
        self._reasoning_item_id = f"rs_{self.output_index}"
        self._reasoning_output_index = self.output_index
        self.output_index += 1
        self._output_sequence.append(("reasoning", None))
        item = {
            "id": self._reasoning_item_id,
            "type": "reasoning",
            "status": "in_progress",
            "summary": [],
            "content": [],
        }
        events.append({"type": "response.output_item.added",
                       "output_index": self._reasoning_output_index, "item": item})

    def _deliver_tool(self, idx, events):
        if idx in self._delivered_tool:
            return
        self._delivered_tool.add(idx)
        state = self._tool_states[idx]
        state["output_index"] = self.output_index
        state["item_id"] = f"fc_{self.output_index}"
        self.output_index += 1
        self._output_sequence.append(("tool", idx))
        item = {
            "id": state["item_id"],
            "type": "function_call",
            "status": "in_progress",
            "name": state["name"],
            "call_id": state["id"],
            "arguments": "",
        }
        events.append({"type": "response.output_item.added",
                       "output_index": state["output_index"], "item": item})

    def translate(self, data):
        """处理一个 chat chunk，返回 Responses 事件 dict 列表。"""
        events = []
        choices = data.get("choices") or []
        if data.get("usage"):
            self.usage = _chat_usage_to_responses(data.get("usage"))
        if not choices:
            return events
        delta = choices[0].get("delta") or {}

        content = delta.get("content")
        reasoning = delta.get("reasoning_content")
        # 推理内容 -> reasoning item（保持与文本输出并行）
        if isinstance(reasoning, str) and reasoning:
            self._ensure_created(events)
            self._deliver_reasoning(events)
            self._reasoning_text += reasoning
            events.append({
                "type": "response.reasoning_summary_text.delta",
                "item_id": self._reasoning_item_id,
                "output_index": self._reasoning_output_index,
                "delta": reasoning,
            })

        if isinstance(content, str) and content:
            self._ensure_created(events)
            self._deliver_message(events)
            self._text += content
            events.append({
                "type": "response.output_text.delta",
                "item_id": self._msg_item_id,
                "output_index": self._msg_output_index,
                "content_index": 0,
                "delta": content,
            })

        for tc in delta.get("tool_calls") or []:
            idx = tc.get("index", 0)
            fn = tc.get("function") or {}
            state = self._tool_states.get(idx)
            if state is None:
                state = {"id": tc.get("id", ""), "name": fn.get("name", ""), "arguments": ""}
                self._tool_states[idx] = state
                self._tool_order.append(idx)
            else:
                if fn.get("name"):
                    state["name"] = fn["name"]
                if tc.get("id"):
                    state["id"] = tc["id"]
            if fn.get("arguments"):
                self._ensure_created(events)
                self._deliver_tool(idx, events)
                state["arguments"] += fn["arguments"]
                events.append({
                    "type": "response.function_call_arguments.delta",
                    "item_id": state["item_id"],
                    "output_index": state["output_index"],
                    "delta": fn["arguments"],
                })

        finish_reason = choices[0].get("finish_reason")
        if finish_reason:
            # 只收尾内容块（done 事件）；response.completed 延迟到流结束（[DONE]/EOF）
            # 由外层调用 complete() 发射，确保 usage 尾块（标准顺序在 finish_reason 之后）
            # 已到达——否则 completed 的 usage 会是 None（Codex 计费错乱）。
            self._close_items(events)
        return events

    def _close_items(self, events):
        """收尾内容块：done 事件（推理/消息/工具），幂等；不发射 response.completed。"""
        if self._finished:
            return
        self._finished = True
        if not self._emitted_created:
            self._ensure_created(events)
        # 关闭推理内容
        if self._reasoning_delivered:
            events.append({"type": "response.reasoning_summary_text.done",
                           "item_id": self._reasoning_item_id,
                           "output_index": self._reasoning_output_index,
                           "text": self._reasoning_text})
            events.append({"type": "response.output_item.done",
                           "output_index": self._reasoning_output_index, "item": {
                               "id": self._reasoning_item_id, "type": "reasoning",
                               "status": "completed",
                               "summary": [{"type": "summary_text", "text": self._reasoning_text}],
                               "content": [{"type": "reasoning_text", "text": self._reasoning_text}]}})
        # 关闭文本消息
        if self._msg_delivered:
            events.append({"type": "response.output_text.done",
                           "item_id": self._msg_item_id,
                           "output_index": self._msg_output_index,
                           "content_index": 0, "text": self._text})
            events.append({"type": "response.content_part.done",
                           "item_id": self._msg_item_id,
                           "output_index": self._msg_output_index,
                           "content_index": 0,
                           "part": {"type": "output_text", "text": self._text, "annotations": []}})
            events.append({"type": "response.output_item.done",
                           "output_index": self._msg_output_index, "item": {
                               "id": self._msg_item_id, "type": "message",
                               "role": "assistant", "status": "completed",
                               "content": [{"type": "output_text", "text": self._text, "annotations": []}]}})
        # 关闭工具调用
        for idx in self._tool_order:
            state = self._tool_states[idx]
            if idx not in self._delivered_tool:
                self._deliver_tool(idx, events)
            events.append({"type": "response.function_call_arguments.done",
                           "item_id": state["item_id"],
                           "output_index": state["output_index"],
                           "arguments": state["arguments"]})
            events.append({"type": "response.output_item.done",
                           "output_index": state["output_index"], "item": {
                               "id": state["item_id"], "type": "function_call",
                               "status": "completed", "name": state["name"],
                               "call_id": state["id"], "arguments": state["arguments"]}})

    def complete(self, events):
        """发射 response.completed（含最终 usage），幂等。流结束（[DONE]/EOF）时调用。"""
        if self._completed:
            return
        self._close_items(events)
        self._completed = True
        events.append({"type": "response.completed", "response": self.assemble()})

    def _finish(self, events):
        """完整收尾：done 事件 + response.completed（幂等，供流结束统一调用）。"""
        self._close_items(events)
        self.complete(events)

    def assemble(self):
        output = []
        for kind, key in self._output_sequence:
            if kind == "message":
                output.append({"id": self._msg_item_id, "type": "message", "role": "assistant",
                               "status": "completed",
                               "content": [{"type": "output_text", "text": self._text, "annotations": []}]})
            elif kind == "reasoning":
                output.append({"id": self._reasoning_item_id, "type": "reasoning",
                               "status": "completed",
                               "summary": [{"type": "summary_text", "text": self._reasoning_text}],
                               "content": [{"type": "reasoning_text", "text": self._reasoning_text}]})
            else:
                state = self._tool_states[key]
                output.append({"id": state["item_id"], "type": "function_call",
                               "status": "completed", "name": state["name"],
                               "call_id": state["id"], "arguments": state["arguments"]})
        resp = self._base_response()
        resp["status"] = "completed"
        resp["output"] = output
        resp["usage"] = self.usage
        return resp


def _tool_choice_any(openai_tools):
    """Anthropic tool_choice='any'（必须调用）→ OpenAI：单工具时绑定该工具，多工具时 required。"""
    names = [
        t.get("function", {}).get("name", "")
        for t in (openai_tools or [])
        if isinstance(t, dict) and isinstance(t.get("function"), dict)
    ]
    names = [n for n in names if n]
    if len(names) == 1:
        return {"type": "function", "function": {"name": names[0]}}
    return "required"


# ---------------------------------------------------------------------------
# 思考深度透传：Anthropic thinking / OpenAI Responses reasoning → 上游参数
#
# 入口 × 上游矩阵：
#   anthropic-messages 入口 → OpenAI Chat 上游    : _apply_thinking_to_chat
#   openai-responses 入口 → OpenAI Chat 上游      : _apply_reasoning_to_chat
#   openai-completions 入口 → Chat 上游           : 整包透传（reasoning_effort 等原样保留）
#   Responses 入口 → Responses 上游 / anthropic 入口 → Anthropic 上游（direct）：整包透传
# 响应方向（上游思考内容 → 客户端）已有完整转换（thinking 块 / reasoning item），此处只处理请求方向。
# ---------------------------------------------------------------------------


def _budget_to_effort(budget):
    """Anthropic budget_tokens（token 预算）→ OpenAI reasoning_effort 档位（近似映射）。

    Anthropic 的深度是 token 预算，OpenAI 系是 low/medium/high 档位，两者不同构，
    只能做阈值近似：≥8192 → high，≥2048 → medium，其余 → low。
    """
    try:
        b = int(budget or 0)
    except (TypeError, ValueError):
        return None
    if b <= 0:
        return None
    if b >= 8192:
        return "high"
    if b >= 2048:
        return "medium"
    return "low"


def _infer_thinking_style(service):
    """auto 模式下按上游 URL / 模型名推断思考参数风格。

    - dashscope / qwen          → enable_thinking（布尔开关）
    - deepseek / kimi / moonshot → thinking（Anthropic 风格对象，可带 budget_tokens）
    - 其他 OpenAI 兼容网关      → reasoning_effort（OpenAI 标准档位）
    """
    url = (service.account.openai_url or "").lower()
    model = (service.model or "").lower()
    if "dashscope" in url or "qwen" in url or "qwen" in model:
        return "enable_thinking"
    if "deepseek" in url or "moonshot" in url or "kimi" in url or "kimi" in model:
        return "passthrough"
    return "effort"


def _apply_thinking_to_chat(chat, thinking, service):
    """Anthropic Messages thinking 配置 → OpenAI Chat 请求参数（按服务 thinking_mode）。"""
    if service.thinking_mode == "none" or not thinking or not isinstance(thinking, dict):
        return
    mode = service.thinking_mode
    if mode == "auto":
        mode = _infer_thinking_style(service)
    enabled = thinking.get("type") != "disabled"
    if mode == "passthrough":
        # 上游原生支持 Anthropic 风格 thinking（DeepSeek V3.2 / Kimi K2 / 兼容网关）：
        # 原样保留 type 与 budget_tokens（Kimi 支持 budget 控制深度）
        out = {"type": thinking.get("type", "enabled")}
        if enabled and thinking.get("budget_tokens"):
            out["budget_tokens"] = thinking["budget_tokens"]
        chat["thinking"] = out
    elif mode == "enable_thinking":
        # DashScope / Qwen 兼容模式：布尔开关
        chat["enable_thinking"] = enabled
    elif mode == "effort":
        # OpenAI 标准档位：budget → low/medium/high；enabled 无预算时用 medium 兜底
        effort = _budget_to_effort(thinking.get("budget_tokens")) if enabled else None
        if enabled and not effort:
            effort = "medium"
        if effort:
            chat["reasoning_effort"] = effort
        # disabled 时 OpenAI 系无关闭语义，忽略（由模型默认决定）


def _apply_reasoning_to_chat(chat, req, service):
    """OpenAI Responses reasoning（effort 档位）→ OpenAI Chat 请求参数（按服务 thinking_mode）。

    兼容两种入参：Responses 的 reasoning: {effort} 对象，或顶层 reasoning_effort 标量。
    """
    if service.thinking_mode == "none":
        return
    reasoning = req.get("reasoning") or {}
    effort = reasoning.get("effort") if isinstance(reasoning, dict) else None
    if not effort:
        effort = req.get("reasoning_effort")
    if not effort:
        return
    mode = service.thinking_mode
    if mode == "auto":
        mode = _infer_thinking_style(service)
    if mode == "effort":
        chat["reasoning_effort"] = effort
    elif mode == "passthrough":
        # Responses 无 token 预算概念，effort 存在即开启思考（深度由上游默认）
        chat["thinking"] = {"type": "enabled"}
    elif mode == "enable_thinking":
        chat["enable_thinking"] = True


def convert_request(req, service):
    """将 Anthropic Messages 格式转为 OpenAI chat completions 格式。"""
    raw_messages = list(req.get("messages", []))

    messages = []
    for msg in raw_messages:
        role = msg.get("role", "user")
        content = msg.get("content", "")

        if isinstance(content, list):
            # 检查是否包含 tool_result blocks
            tool_results = [b for b in content if isinstance(b, dict) and b.get("type") == "tool_result"]
            if tool_results:
                # 转换为 OpenAI tool 消息格式
                # 与 tool_result 交错的文本块按出现顺序冲刷为 user 消息，保持交错顺序
                orphan_text_parts = []
                for block in content:
                    if isinstance(block, dict) and block.get("type") == "tool_result":
                        if orphan_text_parts:
                            messages.append({
                                "role": "user",
                                "content": "\n".join(orphan_text_parts),
                            })
                            orphan_text_parts = []
                        tool_id = block.get("tool_use_id", block.get("id", ""))
                        if not tool_id:
                            # 缺 tool_use_id 无法形成合法 tool 消息（上游会 400）
                            logger.warning("tool_result 块缺少 tool_use_id，已跳过")
                            continue
                        content_val = block.get("content", "")
                        text = _extract_text(content_val)
                        messages.append({
                            "role": "tool",
                            "tool_call_id": tool_id,
                            "content": text,
                        })
                    elif isinstance(block, dict) and block.get("type") == "text":
                        # 与 tool_result 同行的文本块没有 tool_use_id，
                        # 收集后作为 user 消息追加，避免生成非法的空 tool_call_id
                        orphan_text_parts.append(block.get("text", ""))
                if orphan_text_parts:
                    messages.append({
                        "role": "user",
                        "content": "\n".join(orphan_text_parts),
                    })
                continue
            # 检查 assistant 消息是否包含 tool_use
            if role == "assistant":
                tool_uses = [b for b in content if isinstance(b, dict) and b.get("type") == "tool_use"]
                if tool_uses:
                    text_parts = []
                    tool_calls = []
                    for block in content:
                        if isinstance(block, dict) and block.get("type") == "text":
                            text_parts.append(block.get("text", ""))
                        elif isinstance(block, dict) and block.get("type") == "tool_use":
                            tc = {
                                "id": block.get("id", ""),
                                "type": "function",
                                "function": {
                                    "name": block.get("name", ""),
                                    "arguments": json.dumps(block.get("input", {})),
                                },
                            }
                            tool_calls.append(tc)
                    oai_msg = {"role": "assistant", "content": None}
                    if tool_calls:
                        oai_msg["tool_calls"] = tool_calls
                    if text_parts:
                        oai_msg["content"] = "\n".join(text_parts)
                    messages.append(oai_msg)
                    continue

        # 普通文本消息 - 转为纯文本（DashScope 不支持 content blocks 格式）
        text = _extract_text(content)
        if not text:
            # 纯 thinking 块等空 content 消息，跳过（部分上游拒绝空 content）
            continue
        messages.append({
            "role": role,
            "content": text,
        })

    system = req.get("system", "")
    if system:
        # 转为纯文本（DashScope 不支持 content blocks 格式）
        system_content = _extract_text(system)
        messages.insert(0, {"role": "system", "content": system_content})

    is_stream = req.get("stream", False)
    # 模型覆盖开关：默认用服务配置的 model 覆盖客户端请求的模型；
    # override_model=false 时忠实透传客户端模型名（缺失时回退服务配置）
    client_model = req.get("model", "")
    if service.override_model:
        model = service.model
    else:
        model = client_model or service.model
    if client_model and client_model != model:
        logger.debug(f"[MODEL] client requested {client_model} -> use {model} (override={service.override_model})")
    openai_req = {
        "model": model,
        "messages": messages,
        "max_tokens": req.get("max_tokens", service.max_tokens),
        "stream": is_stream,
    }
    if is_stream:
        openai_req["stream_options"] = {"include_usage": True}

    # 转发采样参数（子 agent 可能设置特定 temperature）
    if "temperature" in req:
        openai_req["temperature"] = req["temperature"]
    if "top_p" in req:
        openai_req["top_p"] = req["top_p"]

    # 处理 thinking 参数（Claude Code 的扩展思考功能）：
    # 按服务 thinking_mode 映射到上游（auto 推断 / passthrough 原样 / effort 档位 / enable_thinking 布尔）
    if "thinking" in req:
        _apply_thinking_to_chat(openai_req, req["thinking"], service)

    # 转换 tools: Anthropic -> OpenAI
    tools = req.get("tools", [])
    openai_tools = []
    if tools:
        for tool in tools:
            if isinstance(tool, dict):
                name = tool.get("name", "")
                description = tool.get("description", "")
                input_schema = tool.get("input_schema", {})
                openai_tools.append({
                    "type": "function",
                    "function": {
                        "name": name,
                        "description": description,
                        "parameters": convert_tool_input(input_schema),
                        "strict": False,
                    },
                })
        openai_req["tools"] = openai_tools

    # 转换 tool_choice: Anthropic -> OpenAI
    tool_choice = req.get("tool_choice")
    if tool_choice:
        if isinstance(tool_choice, str):
            if tool_choice == "any":
                openai_req["tool_choice"] = _tool_choice_any(openai_tools)
            elif tool_choice in ("auto", "none"):
                openai_req["tool_choice"] = tool_choice
        elif isinstance(tool_choice, dict):
            tool_type = tool_choice.get("type", "")
            if tool_type == "tool":
                openai_req["tool_choice"] = {
                    "type": "function",
                    "function": {"name": tool_choice.get("name", "")},
                }
            elif tool_type == "any":
                openai_req["tool_choice"] = _tool_choice_any(openai_tools)
            elif tool_type in ("auto", "none"):
                openai_req["tool_choice"] = tool_type

    return openai_req