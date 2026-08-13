"""端到端测试：显式 api 协议三种形态 + 流式终止形态回归（proxy_async.py 引擎）。

1. api=openai-completions             → Chat 整包透传（pi 场景）
2. api=openai-responses + upstream=responses → Responses 整包透传（Codex → DeepSeek 官方场景）
3. api=openai-responses（默认 upstream=chat）→ Responses→Chat→Responses 转换（Codex → opencode 中转场景）
4. 流式终止形态回归（对应协议审计 I1/I2/I3 缺陷修复）：
   - claude 流：上游 EOF 无 [DONE]（含 finish_reason 与无 finish_reason 两种）→ message_delta/message_stop 必须存在
   - responses 转换流：无 finish_reason + [DONE] → response.completed 必须存在（I2）
   - responses 转换流：finish_reason + usage 尾块 + EOF → completed.usage 非空（I3）
   - responses 转换流：仅内容 + EOF → response.completed 仍必须存在

运行方式：
    python test_codex_direct.py        # 直接运行
    python -m pytest test_codex_direct.py -v   # pytest（anyio 插件）

线程版引擎（proxy.py）已合并删除，仅测试 asyncio 引擎。
"""
import asyncio
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import aiohttp
import pytest
from aiohttp import web

import proxy_async
from proxy import Account, Service

MOCK_PORT = 18901
P_ASYNC_PORT = 18902

upstream_bodies = []

# mock_chat_upstream 的行为开关（测试按需切换）：
#   default          内容 delta + usage 尾块 + [DONE]
#   finish_usage_eof 内容 → finish_reason 块 → usage 尾块 → EOF（无 [DONE]）
#   content_only_eof 仅内容 delta → EOF（无 finish_reason 无 [DONE]）
mock_behavior = {"chat": "default"}


async def mock_chat_upstream(request: web.Request):
    """模拟只支持 Chat 的上游（opencode.ai）：chat completions 流式/非流式。"""
    body = await request.read()
    upstream_bodies.append(body)
    req = json.loads(body)
    stream = req.get("stream", False)
    if not stream:
        return web.json_response({
            "id": "mock-chat-1", "object": "chat.completion", "model": req.get("model"),
            "choices": [{"index": 0, "finish_reason": "stop",
                         "message": {"role": "assistant", "content": "你好"}}],
            "usage": {"prompt_tokens": 100, "completion_tokens": 20, "total_tokens": 120},
        })

    resp = web.StreamResponse(headers={"Content-Type": "text/event-stream"})
    await resp.prepare(request)
    behavior = mock_behavior.get("chat", "default")

    def chunk(choices, usage=None):
        data = {"id": "s1", "object": "chat.completion.chunk", "model": req.get("model"),
                "choices": choices}
        if usage is not None:
            data["usage"] = usage
        return f"data: {json.dumps(data, ensure_ascii=False)}\n\n".encode()

    if behavior == "finish_usage_eof":
        # 形态：内容 → finish_reason 块 → usage 尾块 → EOF（不发 [DONE]）
        for content in ["你", "好"]:
            await resp.write(chunk([{"index": 0, "delta": {"content": content}, "finish_reason": None}]))
        await resp.write(chunk([{"index": 0, "delta": {}, "finish_reason": "stop"}]))
        await resp.write(chunk([], usage={"prompt_tokens": 100, "completion_tokens": 20,
                                          "total_tokens": 120}))
        await resp.write_eof()
        return resp

    if behavior == "content_only_eof":
        # 形态：仅内容 delta，无 finish_reason 无 usage 无 [DONE]，直接 EOF
        for content in ["你", "好"]:
            await resp.write(chunk([{"index": 0, "delta": {"content": content}, "finish_reason": None}]))
        await resp.write_eof()
        return resp

    # default：内容 delta（finish_reason=None）+ usage 尾块 + [DONE]
    for content in ["你", "好"]:
        await resp.write(chunk([{"index": 0, "delta": {"content": content}, "finish_reason": None}]))
    await resp.write(chunk([], usage={"prompt_tokens": 100, "completion_tokens": 20,
                                      "total_tokens": 120}))
    await resp.write(b"data: [DONE]\n\n")
    await resp.write_eof()
    return resp


async def mock_responses_upstream(request: web.Request):
    """模拟原生支持 Responses 的上游（DeepSeek 官方）：/v1/responses SSE。"""
    body = await request.read()
    upstream_bodies.append(body)
    req = json.loads(body)
    assert "input" in req or "messages" in req, "responses 上游应收到 responses/chat 请求"
    resp = web.StreamResponse(headers={"Content-Type": "text/event-stream"})
    await resp.prepare(request)
    events = [
        {"type": "response.created",
         "response": {"id": "resp_1", "object": "response", "status": "in_progress",
                      "model": req.get("model"), "output": []}},
        {"type": "response.output_text.delta",
         "item_id": "msg_1", "output_index": 0, "content_index": 0, "delta": "你好"},
        {"type": "response.completed",
         "response": {"id": "resp_1", "object": "response", "status": "completed",
                      "model": req.get("model"), "output": [],
                      "usage": {"input_tokens": 100, "output_tokens": 20,
                                "total_tokens": 120}}},
    ]
    for ev in events:
        await resp.write(f"event: {ev['type']}\ndata: {json.dumps(ev, ensure_ascii=False)}\n\n".encode())
    await resp.write(b"data: [DONE]\n\n")
    await resp.write_eof()
    return resp


def build_service(openai_url: str, api: str = "", upstream_api: str = "openai-completions"):
    # 测试自包含：不依赖本地 config.json（CI 环境没有该文件，.gitignore 忽略）。
    # mock 上游不校验 API Key，用假 key 即可；模型名与各测试 payload 保持一致。
    acc = Account(id="acc-3", name="Test-DeepSeek", api_key="sk-test",
                  openai_url=openai_url, anthropic_url="")
    return Service(name="test-codex", account=acc, client="openai", host="127.0.0.1",
                   port=0, model="deepseek-v4-flash", override_model=True,
                   max_tokens=4096, proxy="", api=api, upstream_api=upstream_api)


async def run_async_engine(openai_url: str, port: int, api: str, upstream_api: str,
                           session: aiohttp.ClientSession) -> web.AppRunner:
    app = web.Application(client_max_size=proxy_async.MAX_BODY_SIZE)
    app["service"] = build_service(openai_url, api, upstream_api)
    app["session"] = session
    app.router.add_route("*", "/{tail:.*}", proxy_async.handle_request)
    runner = web.AppRunner(app, access_log=None)
    await runner.setup()
    await web.TCPSite(runner, "127.0.0.1", port).start()
    return runner


def parse_sse(body: str):
    """解析 SSE 文本，返回事件 dict 列表（跳过 [DONE]）。"""
    events = []
    for line in body.splitlines():
        line = line.strip()
        if not line.startswith("data:"):
            continue
        payload = line[5:].strip()
        if payload == "[DONE]":
            continue
        try:
            events.append(json.loads(payload))
        except json.JSONDecodeError:
            pass
    return events


# ---------------------------------------------------------------------------
# pytest harness（anyio 插件：async fixture + async test 共用同一事件循环）
# ---------------------------------------------------------------------------

pytestmark = pytest.mark.anyio


class Harness:
    def __init__(self, session, ports, runners, mock_runner):
        self.session = session
        self.ports = ports
        self.runners = runners
        self.mock_runner = mock_runner


@pytest.fixture(scope="module")
async def harness():
    mock_app = web.Application()
    mock_app.router.add_post("/chat/completions", mock_chat_upstream)
    mock_app.router.add_post("/v1/responses", mock_responses_upstream)
    mock_runner = web.AppRunner(mock_app, access_log=None)
    await mock_runner.setup()
    await web.TCPSite(mock_runner, "127.0.0.1", MOCK_PORT).start()
    base = f"http://127.0.0.1:{MOCK_PORT}"

    connector = aiohttp.TCPConnector(force_close=True)
    session = aiohttp.ClientSession(connector=connector)

    runners = [
        await run_async_engine(base, P_ASYNC_PORT, "openai-completions", "openai-completions", session),
        await run_async_engine(base, P_ASYNC_PORT + 10, "openai-responses", "openai-responses", session),
        await run_async_engine(base, P_ASYNC_PORT + 20, "openai-responses", "openai-completions", session),
        await run_async_engine(base, P_ASYNC_PORT + 30, "anthropic-messages", "openai-completions", session),
    ]
    ports = {
        "chat": P_ASYNC_PORT,
        "responses_passthrough": P_ASYNC_PORT + 10,
        "responses_convert": P_ASYNC_PORT + 20,
        "claude": P_ASYNC_PORT + 30,
    }
    yield Harness(session, ports, runners, mock_runner)

    for r in runners:
        await r.cleanup()
    await mock_runner.cleanup()
    await session.close()


# ---------------------------------------------------------------------------
# 三种协议形态基线（原有用例）
# ---------------------------------------------------------------------------


async def test_chat_passthrough(harness):
    """api=openai-completions：Chat 整包透传（字节一致）。"""
    mock_behavior["chat"] = "default"
    del upstream_bodies[:]
    payload = {"model": "deepseek-v4-flash",
               "messages": [{"role": "user", "content": "你好中文"}],
               "stream": True, "max_tokens": 4096}
    sent = json.dumps(payload, ensure_ascii=False).encode("utf-8")
    async with harness.session.post(f"http://127.0.0.1:{harness.ports['chat']}/chat/completions",
                                    data=sent, headers={"Content-Type": "application/json"}) as resp:
        body = await resp.text()
        assert resp.status == 200 and '"delta"' in body and "response.output_text.delta" not in body
    assert upstream_bodies and upstream_bodies[0] == sent, "chat 请求体被重建"


async def test_chat_passthrough_developer_role(harness):
    """api=openai-completions：developer 角色在透传前规范化为 system（DeepSeek 等上游不接受 developer）。"""
    mock_behavior["chat"] = "default"
    del upstream_bodies[:]
    payload = {"model": "deepseek-v4-flash",
               "messages": [
                   {"role": "developer", "content": "你是系统提示词"},
                   {"role": "user", "content": "你好"},
               ],
               "stream": True, "max_tokens": 4096}
    async with harness.session.post(f"http://127.0.0.1:{harness.ports['chat']}/chat/completions",
                                    json=payload) as resp:
        body = await resp.text()
        assert resp.status == 200, f"HTTP {resp.status}"
        assert '"delta"' in body, "应收到 chat 流式响应"
    assert upstream_bodies
    up = json.loads(upstream_bodies[-1])
    assert up["messages"][0]["role"] == "system", "developer 应被规范化为 system"
    assert up["messages"][0]["content"] == "你是系统提示词"
    assert up["messages"][1]["role"] == "user"
    # 其余字段保持透传
    assert up["stream"] is True and up["max_tokens"] == 4096 and up["model"] == "deepseek-v4-flash"


async def test_responses_passthrough(harness):
    """api=openai-responses + upstream=responses：Responses 整包透传（字节一致）。"""
    del upstream_bodies[:]
    payload = {"model": "deepseek-v4-flash",
               "input": [{"role": "user", "content": [{"type": "input_text", "text": "hi"}]}],
               "stream": True, "max_output_tokens": 4096}
    sent = json.dumps(payload, ensure_ascii=False).encode("utf-8")
    async with harness.session.post(
            f"http://127.0.0.1:{harness.ports['responses_passthrough']}/v1/responses",
            data=sent, headers={"Content-Type": "application/json"}) as resp:
        body = await resp.text()
        assert resp.status == 200, f"HTTP {resp.status}"
        assert "response.output_text.delta" in body, "应透传 Responses 事件"
    assert upstream_bodies and upstream_bodies[0] == sent, "responses 请求体被重建"


async def test_responses_passthrough_developer_role(harness):
    """api=openai-responses + upstream=responses：input 中 developer 消息项规范化为 system。"""
    del upstream_bodies[:]
    payload = {"model": "deepseek-v4-flash",
               "input": [
                   {"type": "message", "role": "developer",
                    "content": [{"type": "input_text", "text": "你是系统提示词"}]},
                   {"type": "message", "role": "user",
                    "content": [{"type": "input_text", "text": "hi"}]},
               ],
               "stream": True, "max_output_tokens": 4096}
    async with harness.session.post(
            f"http://127.0.0.1:{harness.ports['responses_passthrough']}/v1/responses",
            json=payload) as resp:
        body = await resp.text()
        assert resp.status == 200, f"HTTP {resp.status}"
        assert "response.output_text.delta" in body, "应透传 Responses 事件"
    assert upstream_bodies
    up = json.loads(upstream_bodies[-1])
    assert up["input"][0]["role"] == "system", "responses input 的 developer 应被规范化为 system"
    assert up["input"][1]["role"] == "user"
    assert up["input"][0]["type"] == "message" and up["input"][0]["content"][0]["type"] == "input_text"


def test_normalize_roles_unit():
    """normalize_roles 纯函数边界：input 为字符串 / 非列表 / 无 developer 时零修改。"""
    from proxy import normalize_roles

    # chat messages：developer 降级，其余不动
    payload = {"model": "m", "messages": [
        {"role": "developer", "content": "a"},
        {"role": "user", "content": "b"},
        {"role": "assistant", "content": "c"},
    ]}
    assert normalize_roles(payload) is True
    assert payload["messages"][0]["role"] == "system"

    # responses input 字符串形态：不改
    p2 = {"model": "m", "input": "hello"}
    assert normalize_roles(p2) is False
    assert p2["input"] == "hello"

    # 无 developer：零修改（透传保持字节一致）
    p3 = {"model": "m", "messages": [{"role": "user", "content": "x"}]}
    assert normalize_roles(p3) is False

    # input 非列表（None / dict）
    p4 = {"model": "m", "input": None}
    assert normalize_roles(p4) is False



async def test_responses_convert(harness):
    """api=openai-responses（默认 upstream=chat）：Responses→Chat→Responses 转换基线。"""
    mock_behavior["chat"] = "default"
    payload = {"model": "deepseek-v4-flash",
               "input": [{"role": "user", "content": [{"type": "input_text", "text": "hi"}]}],
               "stream": True, "max_output_tokens": 4096}
    async with harness.session.post(
            f"http://127.0.0.1:{harness.ports['responses_convert']}/v1/responses",
            json=payload) as resp:
        body = await resp.text()
        assert resp.status == 200, f"HTTP {resp.status}"
        assert "response.output_text.delta" in body, "应转回 Responses 事件"


# ---------------------------------------------------------------------------
# 流式终止形态回归（协议审计 I1 / I2 / I3 修复验证）
# ---------------------------------------------------------------------------


async def test_claude_stream_done_termination(harness):
    """claude 流：无 finish_reason + [DONE] → message_delta/message_stop 存在。"""
    mock_behavior["chat"] = "default"
    payload = {"model": "deepseek-v4-flash", "max_tokens": 4096, "stream": True,
               "messages": [{"role": "user", "content": "hi"}]}
    async with harness.session.post(f"http://127.0.0.1:{harness.ports['claude']}/v1/messages",
                                    json=payload) as resp:
        body = await resp.text()
        assert resp.status == 200, f"HTTP {resp.status}"
        types = [e.get("type") for e in parse_sse(body)]
        assert "message_stop" in types, f"message_stop 缺失: {types}"
        assert "message_delta" in types


async def test_claude_stream_eof_termination(harness):
    """claude 流：finish_reason + usage 尾块 + EOF 无 [DONE]（I1 修复）→ message_stop 存在。"""
    mock_behavior["chat"] = "finish_usage_eof"
    payload = {"model": "deepseek-v4-flash", "max_tokens": 4096, "stream": True,
               "messages": [{"role": "user", "content": "hi"}]}
    async with harness.session.post(f"http://127.0.0.1:{harness.ports['claude']}/v1/messages",
                                    json=payload) as resp:
        body = await resp.text()
        assert resp.status == 200, f"HTTP {resp.status}"
        types = [e.get("type") for e in parse_sse(body)]
        assert "message_stop" in types, f"EOF 无 [DONE] 时 message_stop 缺失: {types}"
        assert "message_delta" in types


async def test_claude_stream_eof_no_finish(harness):
    """claude 流：仅内容 + EOF，无 finish_reason 无 [DONE]（I1 极端）→ message_stop 存在。"""
    mock_behavior["chat"] = "content_only_eof"
    payload = {"model": "deepseek-v4-flash", "max_tokens": 4096, "stream": True,
               "messages": [{"role": "user", "content": "hi"}]}
    async with harness.session.post(f"http://127.0.0.1:{harness.ports['claude']}/v1/messages",
                                    json=payload) as resp:
        body = await resp.text()
        assert resp.status == 200, f"HTTP {resp.status}"
        types = [e.get("type") for e in parse_sse(body)]
        assert "message_stop" in types, f"EOF 无 [DONE] 无 finish_reason 时 message_stop 缺失: {types}"


async def _responses_completed(harness, body):
    """断言 responses 转换流以 response.completed 收尾，并返回 completed 事件。"""
    evs = parse_sse(body)
    types = [e.get("type") for e in evs]
    assert "response.completed" in types, f"response.completed 缺失: {types}"
    completed = next(e for e in evs if e.get("type") == "response.completed")
    # 事件顺序：response.created 必须在 completed 之前
    assert types.index("response.created") < types.index("response.completed")
    return completed


async def _post_responses(harness, port):
    payload = {"model": "deepseek-v4-flash",
               "input": [{"role": "user", "content": [{"type": "input_text", "text": "hi"}]}],
               "stream": True, "max_output_tokens": 4096}
    async with harness.session.post(f"http://127.0.0.1:{port}/v1/responses",
                                    json=payload) as resp:
        body = await resp.text()
        assert resp.status == 200, f"HTTP {resp.status}"
        return body


async def test_responses_convert_completed_no_finish(harness):
    """responses 转换流：无 finish_reason + [DONE]（I2 修复）→ response.completed 存在。"""
    mock_behavior["chat"] = "default"
    body = await _post_responses(harness, harness.ports["responses_convert"])
    completed = await _responses_completed(harness, body)
    usage = (completed.get("response") or {}).get("usage")
    assert usage and usage.get("output_tokens") is not None, f"completed.usage 应含 output_tokens: {usage}"


async def test_responses_convert_usage_tail(harness):
    """responses 转换流：finish_reason + usage 尾块 + EOF（I2/I3 修复）→ completed.usage 非空。"""
    mock_behavior["chat"] = "finish_usage_eof"
    body = await _post_responses(harness, harness.ports["responses_convert"])
    completed = await _responses_completed(harness, body)
    usage = (completed.get("response") or {}).get("usage")
    assert usage and usage.get("output_tokens") == 20, f"completed.usage 错误: {usage}"
    assert usage.get("input_tokens") == 100


async def test_responses_convert_eof_no_done(harness):
    """responses 转换流：仅内容 + EOF（无 finish_reason 无 [DONE]）→ completed 仍存在。"""
    mock_behavior["chat"] = "content_only_eof"
    body = await _post_responses(harness, harness.ports["responses_convert"])
    await _responses_completed(harness, body)


async def main():
    """直接运行（python test_codex_direct.py）：同 pytest 相同的引擎与断言。"""
    mock_app = web.Application()
    mock_app.router.add_post("/chat/completions", mock_chat_upstream)
    mock_app.router.add_post("/v1/responses", mock_responses_upstream)
    mock_runner = web.AppRunner(mock_app, access_log=None)
    await mock_runner.setup()
    await web.TCPSite(mock_runner, "127.0.0.1", MOCK_PORT).start()
    base = f"http://127.0.0.1:{MOCK_PORT}"

    connector = aiohttp.TCPConnector(force_close=True)
    session = aiohttp.ClientSession(connector=connector)
    runners = [
        await run_async_engine(base, P_ASYNC_PORT, "openai-completions", "openai-completions", session),
        await run_async_engine(base, P_ASYNC_PORT + 10, "openai-responses", "openai-responses", session),
        await run_async_engine(base, P_ASYNC_PORT + 20, "openai-responses", "openai-completions", session),
        await run_async_engine(base, P_ASYNC_PORT + 30, "anthropic-messages", "openai-completions", session),
    ]
    h = Harness(session, {
        "chat": P_ASYNC_PORT,
        "responses_passthrough": P_ASYNC_PORT + 10,
        "responses_convert": P_ASYNC_PORT + 20,
        "claude": P_ASYNC_PORT + 30,
    }, runners, mock_runner)

    print("== proxy_async.py 引擎 ==")
    await test_chat_passthrough(h)
    await test_responses_passthrough(h)
    await test_responses_convert(h)
    await test_claude_stream_done_termination(h)
    await test_claude_stream_eof_termination(h)
    await test_claude_stream_eof_no_finish(h)
    await test_responses_convert_completed_no_finish(h)
    await test_responses_convert_usage_tail(h)
    await test_responses_convert_eof_no_done(h)

    for r in runners:
        await r.cleanup()
    await mock_runner.cleanup()
    await session.close()
    print("\n全部通过：Chat 透传 / Responses 透传 / Responses→Chat 转换 / 流式终止形态回归")


if __name__ == "__main__":
    asyncio.run(main())
