"""端到端测试：显式 api 协议三种形态（proxy_async.py 引擎）。

1. api=openai-completions             → Chat 整包透传（pi 场景）
2. api=openai-responses + upstream=responses → Responses 整包透传（Codex → DeepSeek 官方场景）
3. api=openai-responses（默认 upstream=chat）→ Responses→Chat→Responses 转换（Codex → opencode 中转场景）

线程版引擎（proxy.py）已合并删除，仅测试 asyncio 引擎。
"""
import asyncio
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import aiohttp
from aiohttp import web

import proxy_async
from proxy import Account, Service, load_config

MOCK_PORT = 18901
P_ASYNC_PORT = 18902

upstream_bodies = []


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
    for content in ["你", "好"]:
        chunk = {"id": "s1", "object": "chat.completion.chunk", "model": req.get("model"),
                 "choices": [{"index": 0, "delta": {"content": content}, "finish_reason": None}]}
        await resp.write(f"data: {json.dumps(chunk, ensure_ascii=False)}\n\n".encode())
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
    svcs = load_config()
    ds = [s for s in svcs if s.account.id == "acc-3"][0]
    acc = Account(id=ds.account.id, name=ds.account.name, api_key=ds.account.api_key,
                  openai_url=openai_url, anthropic_url="")
    return Service(name=ds.name, account=acc, client=ds.client, host="127.0.0.1",
                   port=0, model=ds.model, sub_model=ds.sub_model,
                   max_tokens=ds.max_tokens, proxy="", api=api, upstream_api=upstream_api)


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


async def test_chat_passthrough(session, port, tag):
    """api=openai-completions：Chat 整包透传（字节一致）。"""
    del upstream_bodies[:]
    payload = {"model": "deepseek-v4-flash",
               "messages": [{"role": "user", "content": "你好中文"}],
               "stream": True, "max_tokens": 4096}
    sent = json.dumps(payload, ensure_ascii=False).encode("utf-8")
    async with session.post(f"http://127.0.0.1:{port}/chat/completions",
                            data=sent, headers={"Content-Type": "application/json"}) as resp:
        body = await resp.text()
        assert resp.status == 200 and '"delta"' in body and "response.output_text.delta" not in body
    assert upstream_bodies and upstream_bodies[0] == sent, f"{tag} chat 请求体被重建"
    print(f"  [{tag}] openai-completions Chat 整包透传（字节一致）OK")


async def test_responses_passthrough(session, port, tag):
    """api=openai-responses + upstream=responses：Responses 整包透传（Codex → DeepSeek 场景）。"""
    del upstream_bodies[:]
    payload = {"model": "deepseek-v4-flash",
               "input": [{"role": "user", "content": [{"type": "input_text", "text": "hi"}]}],
               "stream": True, "max_output_tokens": 4096}
    sent = json.dumps(payload, ensure_ascii=False).encode("utf-8")
    async with session.post(f"http://127.0.0.1:{port}/v1/responses",
                            data=sent, headers={"Content-Type": "application/json"}) as resp:
        body = await resp.text()
        assert resp.status == 200, f"{tag} HTTP {resp.status}"
        assert "response.output_text.delta" in body, f"{tag} 应透传 Responses 事件"
    # 上游收到的字节 = 客户端发送的字节（零转换）
    assert upstream_bodies and upstream_bodies[0] == sent, (
        f"{tag} responses 请求体被重建: 客户端={len(sent)}B 上游={len(upstream_bodies[0])}B")
    print(f"  [{tag}] openai-responses+upstream=responses 整包透传（字节一致）OK")


async def test_responses_convert(session, port, tag):
    """api=openai-responses（默认 upstream=chat）：Responses→Chat→Responses 转换（Codex → 中转场景）。"""
    payload = {"model": "deepseek-v4-flash",
               "input": [{"role": "user", "content": [{"type": "input_text", "text": "hi"}]}],
               "stream": True, "max_output_tokens": 4096}
    async with session.post(f"http://127.0.0.1:{port}/v1/responses", json=payload) as resp:
        body = await resp.text()
        assert resp.status == 200, f"{tag} HTTP {resp.status}"
        assert "response.output_text.delta" in body, f"{tag} 应转回 Responses 事件"
    print(f"  [{tag}] openai-responses（upstream=chat）转换 OK")


async def main():
    # 两个 mock 上游：chat（opencode 场景）+ responses（DeepSeek 场景）
    mock_app = web.Application()
    mock_app.router.add_post("/chat/completions", mock_chat_upstream)
    mock_app.router.add_post("/v1/responses", mock_responses_upstream)
    mock_runner = web.AppRunner(mock_app, access_log=None)
    await mock_runner.setup()
    await web.TCPSite(mock_runner, "127.0.0.1", MOCK_PORT).start()
    base = f"http://127.0.0.1:{MOCK_PORT}"

    connector = aiohttp.TCPConnector(force_close=True)
    session = aiohttp.ClientSession(connector=connector)

    print("== proxy_async.py 引擎 ==")
    # chat 透传（上游 chat 端点）
    r1 = await run_async_engine(base, P_ASYNC_PORT, "openai-completions", "openai-completions", session)
    # responses 透传（上游 responses 端点：Codex → DeepSeek）
    r2 = await run_async_engine(base, P_ASYNC_PORT + 10, "openai-responses", "openai-responses", session)
    # responses 转换（上游 chat 端点：Codex → opencode 中转）
    r3 = await run_async_engine(base, P_ASYNC_PORT + 20, "openai-responses", "openai-completions", session)
    await test_chat_passthrough(session, P_ASYNC_PORT, "async")
    await test_responses_passthrough(session, P_ASYNC_PORT + 10, "async")
    await test_responses_convert(session, P_ASYNC_PORT + 20, "async")

    for r in (r1, r2, r3):
        await r.cleanup()
    await mock_runner.cleanup()
    await session.close()
    print("\n全部通过：Chat 透传 / Responses 透传 / Responses→Chat 转换（单一引擎 proxy_async.py）")


if __name__ == "__main__":
    asyncio.run(main())
