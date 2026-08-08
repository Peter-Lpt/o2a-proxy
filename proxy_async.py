"""
Anthropic -> OpenAI 异步代理（asyncio + aiohttp 单进程版）

相对 proxy.py（线程 + urllib）的架构改进：
- 客户端：aiohttp 服务器，HTTP/1.1 keep-alive，不按连接占用 OS 线程
- 上游：aiohttp.ClientSession 连接池，复用 TCP/TLS 连接
- 客户端断开连接时立即停止读取上游并释放连接
- 多服务多端口共享同一个事件循环和连接池

用法与 proxy.py 完全一致：
    python proxy_async.py [--service <comment|port>]

协议转换逻辑复用 proxy.py 中的纯函数，保证行为一致。
"""

import asyncio
import json
import os
import sys
import time

import aiohttp
from aiohttp import web

from proxy import (
    STREAM_TIMEOUT,
    Service,
    _ResponsesStreamTranslator,
    _anthropic_stop_reason,
    _chat_to_responses_json,
    _convert_usage,
    _responses_to_chat,
    _responses_url,
    _strip_cache_control,
    convert_request,
    detect_client,
    get_account_summary,
    get_stats,
    is_cache_stats_enabled,
    load_config,
    logger,
    resolve_mode,
    sse_event,
)

# 上游建连/首包超时（秒），与旧版 urllib timeout=120 对齐
CONNECT_TIMEOUT = 120
# 上游连接池上限
UPSTREAM_POOL_LIMIT = 200
# 请求体上限：1M 上下文场景请求可能很大
MAX_BODY_SIZE = 128 * 1024 * 1024


class ClientGone(Exception):
    """客户端已断开连接。"""


def build_target(service: Service, request: web.Request) -> str:
    """拼接上游地址与客户端传来的查询参数（如 ?beta=true）。"""
    target = service.target_url
    qs = request.rel_url.query_string
    if qs:
        target += ("&" if "?" in target else "?") + qs
    return target


def upstream_headers(service: Service) -> dict:
    headers = {
        "Content-Type": "application/json",
        "Authorization": f"Bearer {service.api_key}",
    }
    return {"headers": headers}


def upstream_kwargs(service: Service) -> dict:
    kwargs = upstream_headers(service)
    if service.proxy:
        kwargs["proxy"] = service.proxy
    return kwargs


def json_response(data, status: int = 200) -> web.Response:
    return web.Response(
        body=json.dumps(data).encode("utf-8"),
        status=status,
        content_type="application/json",
    )


def error_response(status: int, message: str) -> web.Response:
    """Anthropic 风格的错误响应。"""
    return json_response(
        {"type": "error", "error": {"type": "api_error", "message": message}},
        status=status,
    )


def openai_error_response(status: int, message: str) -> web.Response:
    """OpenAI 风格的错误响应。"""
    return json_response(
        {"error": {"message": message, "type": "api_error"}},
        status=status,
    )


async def stream_write(resp: web.StreamResponse, data: bytes) -> None:
    """向客户端写 SSE 数据；客户端断开时抛 ClientGone。"""
    if not data:
        return
    try:
        await resp.write(data)
    except (ConnectionResetError, BrokenPipeError, aiohttp.ServerDisconnectedError):
        raise ClientGone() from None


async def record_stats(service: Service, model: str, usage: dict) -> None:
    """记录缓存统计（写盘放到线程池，避免阻塞事件循环）。"""
    if is_cache_stats_enabled() and usage and usage.get("input_tokens"):
        await asyncio.to_thread(
            get_stats(service.name, service.account.id).record, model, usage
        )


def upstream_timeout(*, total=None):
    return aiohttp.ClientTimeout(
        total=total,
        connect=CONNECT_TIMEOUT,
        sock_read=STREAM_TIMEOUT,
    )


# ---------------------------------------------------------------------------
# Claude 模式（Anthropic Messages -> OpenAI Chat Completions）
# ---------------------------------------------------------------------------

def _payload_summary(openai_request: dict, body: bytes) -> str:
    """仅记录请求元信息，不输出 messages 内容，避免泄露对话。"""
    msgs = openai_request.get("messages") or []
    tools = openai_request.get("tools") or []
    return (f"model={openai_request.get('model')} "
            f"messages={len(msgs)} tools={len(tools)} "
            f"has_thinking={'thinking' in openai_request} "
            f"bytes={len(body)}")


async def handle_claude_stream(request: web.Request, service: Service,
                               openai_request: dict, req_start: float):
    """流式请求：上游 SSE 逐行翻译为 Anthropic SSE 转发给客户端。"""
    session = request.app["session"]
    target = build_target(service, request)
    body = json.dumps(openai_request).encode("utf-8")
    logger.info(f"[FWD] forwarding stream model={openai_request.get('model')} "
                f"url={target} timeout=120s payload_bytes={len(body)} "
                f"elapsed_since_req={time.time()-req_start:.3f}s")

    resp = None
    try:
        async with session.post(
            target, data=body,
            timeout=upstream_timeout(),
            **upstream_kwargs(service),
        ) as up:
            if up.status != 200:
                err_body = await up.text()
                logger.error(f"Upstream error {up.status}: {err_body}")
                logger.error(f"Sent request: {_payload_summary(openai_request, body)}")
                return error_response(up.status, f"upstream error: {err_body}")

            logger.info(f"[FWD] upstream connected status={up.status} "
                        f"connect_time={time.time()-req_start:.2f}s")

            resp = web.StreamResponse(status=200, headers={
                "Content-Type": "text/event-stream; charset=utf-8",
                "Cache-Control": "no-cache",
                "X-Accel-Buffering": "no",
            })
            await resp.prepare(request)

            message_id = "proxy-msg-stream"
            model = service.model
            started = False
            finished = False
            input_tokens = 0
            output_tokens = 0
            cached_tokens = 0
            cache_write_tokens = 0
            reasoning_tokens = 0
            latest_usage = {
                "input_tokens": 0,
                "output_tokens": 0,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 0,
            }
            content_block_idx = 0
            content_block_open = False
            thinking_block_open = False
            pending_finish_reason = None
            tool_input_buf = {}
            tool_index_to_block_index = {}
            n_chunks = 0
            reasoning_bytes = 0
            text_bytes = 0
            first_chunk_ts = None
            last_prog_ts = time.time()

            try:
                while True:
                    line = await up.content.readline()
                    if not line:
                        break
                    line = line.decode("utf-8").strip()
                    if not line.startswith("data:"):
                        continue

                    n_chunks += 1
                    if first_chunk_ts is None:
                        first_chunk_ts = time.time()
                        logger.info(f"[STREAM] first upstream chunk received "
                                    f"time_to_first_token={first_chunk_ts - req_start:.2f}s "
                                    f"elapsed_since_req={first_chunk_ts - req_start:.2f}s")
                    now = time.time()
                    req_elapsed = now - req_start
                    if req_elapsed > STREAM_TIMEOUT:
                        logger.warning(f"[STREAM] timeout after {req_elapsed:.1f}s "
                                       f"(limit={STREAM_TIMEOUT}s) chunks={n_chunks} "
                                       f"reasoning_bytes={reasoning_bytes} text_bytes={text_bytes}")
                        if thinking_block_open:
                            await stream_write(resp, sse_event({
                                "type": "content_block_stop",
                                "index": content_block_idx,
                            }).encode())
                            thinking_block_open = False
                        if content_block_open:
                            await stream_write(resp, sse_event({
                                "type": "content_block_stop",
                                "index": content_block_idx,
                            }).encode())
                            content_block_open = False
                        if started:
                            await stream_write(resp, sse_event({
                                "type": "message_delta",
                                "delta": {"stop_reason": "max_tokens"},
                                "usage": latest_usage,
                            }).encode())
                        await stream_write(resp, sse_event({
                            "type": "message_stop",
                        }).encode())
                        finished = True
                        break
                    if now - last_prog_ts >= 5.0:
                        last_prog_ts = now
                        logger.info(f"[STREAM] progress chunks={n_chunks} "
                                    f"elapsed={req_elapsed:.1f}s "
                                    f"reasoning_bytes={reasoning_bytes} text_bytes={text_bytes}")

                    data_str = line[5:].strip()
                    if data_str == "[DONE]":
                        if not finished and started:
                            if thinking_block_open:
                                await stream_write(resp, sse_event({
                                    "type": "content_block_stop",
                                    "index": content_block_idx,
                                }).encode())
                                thinking_block_open = False
                            if content_block_open:
                                await stream_write(resp, sse_event({
                                    "type": "content_block_stop",
                                    "index": content_block_idx,
                                }).encode())
                                content_block_open = False
                            stop_reason = _anthropic_stop_reason(pending_finish_reason)
                            await stream_write(resp, sse_event({
                                "type": "message_delta",
                                "delta": {"stop_reason": stop_reason, "stop_sequence": None},
                                "usage": latest_usage,
                            }).encode())
                            await stream_write(resp, sse_event({
                                "type": "message_stop",
                            }).encode())
                            finished = True
                            await record_stats(service, model, latest_usage)
                        logger.info(f"[STREAM] completed finished={finished} chunks={n_chunks} "
                                    f"total_elapsed={time.time()-req_start:.2f}s "
                                    f"reasoning_bytes={reasoning_bytes} text_bytes={text_bytes} "
                                    f"input={latest_usage.get('input_tokens')} "
                                    f"output={latest_usage.get('output_tokens')}")
                        break

                    try:
                        chunk = json.loads(data_str)
                    except json.JSONDecodeError:
                        continue

                    usage = chunk.get("usage", {})
                    if usage:
                        converted_usage = _convert_usage(usage)
                        input_tokens = converted_usage["input_tokens"]
                        output_tokens = converted_usage["output_tokens"] or output_tokens
                        cached_tokens = converted_usage["cache_read_input_tokens"]
                        cache_write_tokens = converted_usage["cache_creation_input_tokens"]
                        reasoning_tokens = converted_usage["reasoning_tokens"]
                        latest_usage = {
                            "input_tokens": input_tokens,
                            "output_tokens": output_tokens,
                            "cache_creation_input_tokens": cache_write_tokens,
                            "cache_read_input_tokens": cached_tokens,
                        }
                        logger.debug(f"[DEBUG] cached_tokens={cached_tokens}, "
                                     f"cache_write_tokens={cache_write_tokens}, "
                                     f"input_tokens={input_tokens}, "
                                     f"prompt_total={converted_usage['prompt_total']}, "
                                     f"reasoning_tokens={reasoning_tokens}")

                    choices = chunk.get("choices", [])
                    if not choices:
                        # 最后一个 chunk（choices 为空）带 usage，发送结束事件
                        if pending_finish_reason and not finished:
                            stop_reason = _anthropic_stop_reason(pending_finish_reason)
                            await stream_write(resp, sse_event({
                                "type": "message_delta",
                                "delta": {"stop_reason": stop_reason, "stop_sequence": None},
                                "usage": latest_usage,
                            }).encode())
                            await stream_write(resp, sse_event({
                                "type": "message_stop",
                            }).encode())
                            finished = True
                            await record_stats(service, model, latest_usage)
                        continue

                    choice = choices[0]
                    delta = choice.get("delta", {})
                    finish_reason = choice.get("finish_reason")

                    if not started:
                        started = True
                        message_id = chunk.get("id", message_id)
                        model = chunk.get("model", model)
                        await stream_write(resp, sse_event({
                            "type": "message_start",
                            "message": {
                                "id": message_id,
                                "type": "message",
                                "role": "assistant",
                                "content": [],
                                "model": model,
                                "stop_reason": None,
                                "stop_sequence": None,
                                "usage": {
                                    "input_tokens": input_tokens,
                                    "output_tokens": 0,
                                    "cache_creation_input_tokens": cache_write_tokens,
                                    "cache_read_input_tokens": cached_tokens,
                                },
                            },
                        }).encode())

                    # 处理思考内容（reasoning_content -> thinking block）
                    reasoning_content = delta.get("reasoning_content", "")
                    if reasoning_content:
                        if not thinking_block_open:
                            if content_block_open:
                                await stream_write(resp, sse_event({
                                    "type": "content_block_stop",
                                    "index": content_block_idx,
                                }).encode())
                                content_block_open = False
                                content_block_idx += 1
                            await stream_write(resp, sse_event({
                                "type": "content_block_start",
                                "index": content_block_idx,
                                "content_block": {"type": "thinking", "thinking": ""},
                            }).encode())
                            thinking_block_open = True
                        await stream_write(resp, sse_event({
                            "type": "content_block_delta",
                            "index": content_block_idx,
                            "delta": {"type": "thinking_delta", "thinking": reasoning_content},
                        }).encode())
                        reasoning_bytes += len(reasoning_content)
                    elif thinking_block_open and not reasoning_content:
                        await stream_write(resp, sse_event({
                            "type": "content_block_stop",
                            "index": content_block_idx,
                        }).encode())
                        thinking_block_open = False
                        content_block_idx += 1

                    # 处理文本内容
                    content = delta.get("content", "")
                    if content:
                        if thinking_block_open:
                            await stream_write(resp, sse_event({
                                "type": "content_block_stop",
                                "index": content_block_idx,
                            }).encode())
                            thinking_block_open = False
                            content_block_idx += 1
                        if not content_block_open:
                            await stream_write(resp, sse_event({
                                "type": "content_block_start",
                                "index": content_block_idx,
                                "content_block": {"type": "text", "text": ""},
                            }).encode())
                            content_block_open = True
                        await stream_write(resp, sse_event({
                            "type": "content_block_delta",
                            "index": content_block_idx,
                            "delta": {"type": "text_delta", "text": content},
                        }).encode())
                        text_bytes += len(content)

                    # 处理 tool_calls
                    tool_calls = delta.get("tool_calls", [])
                    for tc in tool_calls:
                        tool_call_index = int(tc.get("index", len(tool_index_to_block_index)))
                        tc_id = tc.get("id", "")
                        tc_func = tc.get("function", {})
                        tc_name = tc_func.get("name", "")
                        tc_args = tc_func.get("arguments", "")

                        if tc_id:
                            # 新的 tool_use 开始
                            if thinking_block_open:
                                await stream_write(resp, sse_event({
                                    "type": "content_block_stop",
                                    "index": content_block_idx,
                                }).encode())
                                thinking_block_open = False
                                content_block_idx += 1
                            if content_block_open:
                                await stream_write(resp, sse_event({
                                    "type": "content_block_stop",
                                    "index": content_block_idx,
                                }).encode())
                                content_block_open = False
                                content_block_idx += 1
                            block_idx = content_block_idx
                            tool_index_to_block_index[tool_call_index] = block_idx
                            await stream_write(resp, sse_event({
                                "type": "content_block_start",
                                "index": block_idx,
                                "content_block": {
                                    "type": "tool_use",
                                    "id": tc_id,
                                    "name": tc_name,
                                    "input": {},
                                },
                            }).encode())
                            tool_input_buf[block_idx] = {
                                "id": tc_id,
                                "name": tc_name,
                                "input_str": tc_args,
                            }
                            content_block_idx += 1
                        elif tc_args:
                            idx = tool_index_to_block_index.get(tool_call_index, content_block_idx)
                            if idx in tool_input_buf:
                                tool_input_buf[idx]["input_str"] += tc_args
                            else:
                                tool_input_buf[idx] = {
                                    "id": "",
                                    "name": tc_name,
                                    "input_str": tc_args,
                                }
                        if tc_args:
                            await stream_write(resp, sse_event({
                                "type": "content_block_delta",
                                "index": tool_index_to_block_index.get(
                                    tool_call_index, content_block_idx),
                                "delta": {
                                    "type": "input_json_delta",
                                    "partial_json": tc_args,
                                },
                            }).encode())
                            text_bytes += len(tc_args)

                    if finish_reason and not finished:
                        pending_finish_reason = finish_reason
                        if thinking_block_open:
                            await stream_write(resp, sse_event({
                                "type": "content_block_stop",
                                "index": content_block_idx,
                            }).encode())
                            thinking_block_open = False
                        for idx in tool_input_buf:
                            await stream_write(resp, sse_event({
                                "type": "content_block_stop",
                                "index": idx,
                            }).encode())
                        tool_input_buf.clear()
                        if content_block_open:
                            await stream_write(resp, sse_event({
                                "type": "content_block_stop",
                                "index": content_block_idx,
                            }).encode())
                            content_block_open = False

                # 上游关闭但未收到 [DONE]
                if not finished:
                    logger.info(f"[STREAM] upstream closed without [DONE] chunks={n_chunks} "
                                f"elapsed={time.time()-req_start:.2f}s "
                                f"finished={finished}")
            except ClientGone:
                logger.info(f"Client disconnected (stream)")
            except asyncio.CancelledError:
                raise
            except Exception as e:
                logger.error(f"Stream error: {e}", exc_info=True)
                try:
                    if not finished and started:
                        await stream_write(resp, sse_event({
                            "type": "error",
                            "error": {"type": "api_error", "message": str(e)},
                        }).encode())
                    elif not started:
                        await resp.write(json.dumps({
                            "type": "error",
                            "error": {"type": "api_error", "message": str(e)},
                        }).encode("utf-8"))
                except (ClientGone, ConnectionResetError):
                    pass
    except aiohttp.ClientError as e:
        err_body = str(e)
        logger.error(f"Upstream request failed: {err_body[:500]}")
        logger.error(f"Sent request: {_payload_summary(openai_request, body)}")
        if resp is None:
            # 尚未发出响应头，可以正常返回 502
            return error_response(502, f"upstream error: {err_body[:300]}")
        # 流已开始，只能结束当前响应
        try:
            await resp.write_eof()
        except Exception:
            pass
        return resp
    return resp


async def handle_claude_non_stream(request: web.Request, service: Service,
                                   openai_request: dict, req_start: float):
    """非流式请求：转发上游 JSON 响应并转换为 Anthropic 格式。"""
    session = request.app["session"]
    target = build_target(service, request)
    body = json.dumps(openai_request).encode("utf-8")
    logger.info(f"[FWD] forwarding(non-stream) model={openai_request.get('model')} "
                f"url={target} timeout=120s payload_bytes={len(body)} "
                f"elapsed_since_req={time.time()-req_start:.3f}s")

    try:
        async with session.post(
            target, data=body,
            timeout=upstream_timeout(),
            **upstream_kwargs(service),
        ) as up:
            if up.status != 200:
                err_body = await up.text()
                logger.error(f"Upstream error {up.status}: {err_body}")
                logger.error(f"Sent request: {_payload_summary(openai_request, body)}")
                return error_response(up.status, f"upstream error: {err_body}")
            raw = await up.json(content_type=None)
    except aiohttp.ClientError as e:
        err_body = str(e)
        logger.error(f"Upstream request failed: {err_body[:500]}")
        logger.error(f"Sent request: {_payload_summary(openai_request, body)}")
        return error_response(502, f"upstream error: {err_body[:300]}")

    logger.info(f"[NONSTREAM] completed elapsed={time.time()-req_start:.2f}s "
                f"model={raw.get('model')} usage={raw.get('usage')}")

    content = ""
    finish_reason = "stop"
    tool_calls = []
    if raw.get("choices"):
        choice = raw["choices"][0]
        message = choice.get("message", {})
        content = message.get("content", "")
        finish_reason = choice.get("finish_reason", "stop")

        for tc in message.get("tool_calls") or []:
            try:
                tc_input = json.loads(tc.get("function", {}).get("arguments", "{}"))
            except (json.JSONDecodeError, ValueError):
                tc_input = tc.get("function", {}).get("arguments", "{}")
            tool_calls.append({
                "type": "tool_use",
                "id": tc.get("id", ""),
                "name": tc.get("function", {}).get("name", ""),
                "input": tc_input,
            })

    usage = raw.get("usage", {})
    converted_usage = _convert_usage(usage)
    input_tokens = converted_usage["input_tokens"]
    output_tokens = converted_usage["output_tokens"]
    cached_tokens = converted_usage["cache_read_input_tokens"]
    cache_write_tokens = converted_usage["cache_creation_input_tokens"]

    content_list = []
    reasoning_content = ""
    if raw.get("choices"):
        reasoning_content = raw["choices"][0].get("message", {}).get("reasoning_content", "")
    if reasoning_content:
        content_list.append({"type": "thinking", "thinking": reasoning_content})
    if content:
        content_list.append({"type": "text", "text": content})
    content_list.extend(tool_calls)

    stop_reason = _anthropic_stop_reason(finish_reason, bool(tool_calls))
    await record_stats(
        service,
        raw.get("model", service.model),
        {"input_tokens": input_tokens,
         "output_tokens": output_tokens,
         "cache_read_input_tokens": cached_tokens,
         "cache_creation_input_tokens": cache_write_tokens,
         "reasoning_tokens": converted_usage.get("reasoning_tokens", 0)},
    )

    return json_response({
        "id": raw.get("id", "proxy-msg"),
        "type": "message",
        "role": "assistant",
        "content": content_list if content_list else [],
        "model": raw.get("model", service.model),
        "stop_reason": stop_reason,
        "stop_sequence": None,
        "usage": {
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
            "cache_creation_input_tokens": cache_write_tokens,
            "cache_read_input_tokens": cached_tokens,
        },
    })


# ---------------------------------------------------------------------------
# Chat / Responses 整包透传（api=openai-completions 或 api=openai-responses+upstream=responses）
# ---------------------------------------------------------------------------

async def handle_passthrough(request: web.Request, service: Service,
                             body: bytes, stream: bool, req_start: float,
                             target: str = None, responses_usage: bool = False):
    """整包透传：不重建请求体、不补字段，仅缺失 model 时注入服务配置。
    响应原样返回（流式逐行、非流式整包），仅顺带提取 usage 记统计。
    responses_usage=True 时从 response.completed 事件提取 usage（Responses 格式）。"""
    session = request.app["session"]
    if target is None:
        target = build_target(service, request)
    try:
        payload = json.loads(body)
        if not payload.get("model") and service.model:
            payload["model"] = service.model
            body = json.dumps(payload).encode("utf-8")
        model = payload.get("model") or service.model
    except json.JSONDecodeError:
        return openai_error_response(400, "invalid json")
    logger.info(f"[FWD][passthrough] stream={stream} model={model} "
                f"url={target} payload_bytes={len(body)}")
    try:
        async with session.post(
            target, data=body,
            timeout=upstream_timeout(),
            **upstream_kwargs(service),
        ) as up:
            if up.status != 200:
                err = await up.text()
                return openai_error_response(up.status, f"upstream error: {err[:300]}")
            if not stream:
                raw = await up.read()
                try:
                    data = json.loads(raw)
                    converted = _convert_usage(data.get("usage") or {})
                    if converted.get("input_tokens"):
                        await record_stats(service, data.get("model") or model, converted)
                except Exception:
                    pass
                logger.info(f"[passthrough] completed bytes={len(raw)}")
                return web.Response(body=raw, status=200, content_type="application/json")
            resp = web.StreamResponse(status=200, headers={
                "Content-Type": "text/event-stream; charset=utf-8",
                "Cache-Control": "no-cache",
                "X-Accel-Buffering": "no",
            })
            await resp.prepare(request)
            latest_usage = None
            try:
                while True:
                    line = await up.content.readline()
                    if not line:
                        break
                    stripped = line.strip()
                    if stripped.startswith(b"data:"):
                        piece = stripped[5:].strip()
                        if piece and piece != b"[DONE]":
                            try:
                                chunk = json.loads(piece.decode("utf-8", errors="replace"))
                                if responses_usage:
                                    # Responses：usage 在 response.completed 事件中
                                    if chunk.get("type") == "response.completed":
                                        u = (chunk.get("response") or {}).get("usage") or {}
                                        if u:
                                            latest_usage = _convert_usage(u)
                                else:
                                    usage = chunk.get("usage")
                                    if usage:
                                        latest_usage = _convert_usage(usage)
                            except Exception:
                                pass
                    await stream_write(resp, line)
            except ClientGone:
                logger.info("Client disconnected (passthrough)")
            except asyncio.CancelledError:
                raise
            except Exception as e:
                logger.error(f"[passthrough] stream error: {e}", exc_info=True)
            finally:
                await record_stats(service, model, latest_usage)
            return resp
    except aiohttp.ClientError as e:
        err_body = str(e)
        logger.error(f"[passthrough] upstream failed: {err_body[:500]}")
        return openai_error_response(502, f"upstream error: {err_body[:300]}")


# ---------------------------------------------------------------------------
# Codex 模式（OpenAI Responses -> Chat Completions 透传）
# ---------------------------------------------------------------------------

async def handle_openai_stream(request: web.Request, service: Service,
                               req: dict, chat_body: bytes, req_start: float):
    session = request.app["session"]
    target = build_target(service, request)
    logger.info(f"[FWD][codex] forwarding stream model={service.model} "
                f"url={target} payload_bytes={len(chat_body)}")

    # 请求格式决定响应方向：Responses 入（有 input）→ 转 Responses 出；
    # Chat Completions 入（messages）→ 上游 SSE 原样透传
    is_responses = bool(req.get("input"))
    resp = None
    try:
        async with session.post(
            target, data=chat_body,
            timeout=upstream_timeout(),
            **upstream_kwargs(service),
        ) as up:
            if up.status != 200:
                err = await up.text()
                return openai_error_response(up.status, f"upstream error: {err[:300]}")

            resp = web.StreamResponse(status=200, headers={
                "Content-Type": "text/event-stream; charset=utf-8",
                "Cache-Control": "no-cache",
                "X-Accel-Buffering": "no",
            })
            await resp.prepare(request)

            model = service.model
            latest_usage = None
            if is_responses:
                translator = _ResponsesStreamTranslator(req.get("model") or service.model)
            try:
                while True:
                    line = await up.content.readline()
                    if not line:
                        break
                    if not is_responses:
                        # Chat 直通：原样转发每行，仅顺带提取 usage 供统计
                        stripped = line.strip()
                        if stripped.startswith(b"data:") :
                            piece = stripped[5:].strip()
                            if piece and piece != b"[DONE]":
                                try:
                                    chunk = json.loads(piece.decode("utf-8", errors="replace"))
                                    usage = chunk.get("usage")
                                    if usage:
                                        latest_usage = _convert_usage(usage)
                                except Exception:
                                    pass
                        await stream_write(resp, line)
                        continue
                    if line.startswith(b"data:"):
                        data_str = line[5:].strip().decode("utf-8", errors="replace")
                        if data_str == "[DONE]":
                            break
                        try:
                            chunk = json.loads(data_str)
                            usage = chunk.get("usage")
                            if usage:
                                latest_usage = _convert_usage(usage)
                        except Exception:
                            continue
                        for ev in translator.translate(chunk):
                            await stream_write(resp, sse_event(ev).encode())
            except ClientGone:
                logger.info("Client disconnected (codex stream)")
            except asyncio.CancelledError:
                raise
            except Exception as e:
                logger.error(f"[codex] stream error: {e}", exc_info=True)
            finally:
                await record_stats(service, model, latest_usage)
    except aiohttp.ClientError as e:
        err_body = str(e)
        logger.error(f"[codex] upstream failed: {err_body[:500]}")
        if resp is None:
            return openai_error_response(502, f"upstream error: {err_body[:300]}")
        try:
            await resp.write_eof()
        except Exception:
            pass
        return resp
    return resp


async def handle_openai_non_stream(request: web.Request, service: Service,
                                   req: dict, chat_body: bytes, req_start: float):
    session = request.app["session"]
    target = build_target(service, request)
    logger.info(f"[FWD][codex] forwarding(non-stream) model={service.model} "
                f"url={target} payload_bytes={len(chat_body)}")

    try:
        async with session.post(
            target, data=chat_body,
            timeout=upstream_timeout(),
            **upstream_kwargs(service),
        ) as up:
            if up.status != 200:
                err = await up.text()
                return openai_error_response(up.status, f"upstream error: {err[:300]}")
            raw = await up.read()
    except aiohttp.ClientError as e:
        err_body = str(e)
        logger.error(f"[codex] upstream failed: {err_body[:500]}")
        return openai_error_response(502, f"upstream error: {err_body[:300]}")

    try:
        data = json.loads(raw)
        converted = _convert_usage(data.get("usage") or {})
        if converted.get("input_tokens") and is_cache_stats_enabled():
            model = service.model
            await asyncio.to_thread(
                get_stats(service.name, service.account.id).record, model, converted
            )
        if not req.get("input"):
            # Chat 直通：上游本就是 chat.completion，原样返回
            return web.Response(body=raw, status=200, content_type="application/json")
        out = _chat_to_responses_json(data, req.get("model") or service.model)
        raw = json.dumps(out).encode("utf-8")
    except Exception as e:
        logger.warning(f"[codex] response convert failed: {e}")

    logger.info(f"[codex][nonstream] completed bytes={len(raw)}")
    return web.Response(body=raw, status=200, content_type="application/json")


# ---------------------------------------------------------------------------
# Direct 模式（Anthropic Messages -> Anthropic 原生透传，直连）
# ---------------------------------------------------------------------------

def upstream_direct_headers(service: Service) -> dict:
    headers = {
        "Content-Type": "application/json",
        "Authorization": f"Bearer {service.api_key}",
        "x-api-key": service.api_key,
        "anthropic-version": "2023-06-01",
    }
    return {"headers": headers}


async def handle_direct_non_stream(request: web.Request, service: Service,
                                   body: bytes, req_start: float):
    """直连非流式：原样透传 Anthropic 请求，响应原样返回。"""
    session = request.app["session"]
    target = build_target(service, request)
    logger.info(f"[FWD][direct] forwarding(non-stream) url={target} "
                f"bytes={len(body)} elapsed={time.time()-req_start:.3f}s")
    try:
        async with session.post(
            target, data=body,
            timeout=upstream_timeout(),
            **upstream_direct_headers(service),
        ) as up:
            if up.status != 200:
                err = await up.text()
                logger.error(f"[direct] upstream error {up.status}: {err[:300]}")
                return error_response(up.status, f"upstream error: {err[:300]}")
            raw = await up.read()
    except aiohttp.ClientError as e:
        err_body = str(e)
        logger.error(f"[direct] upstream failed: {err_body[:500]}")
        return error_response(502, f"upstream error: {err_body[:300]}")

    try:
        data = json.loads(raw)
        model = data.get("model") or service.model
        usage = data.get("usage") or {}
        # 兼容 Anthropic(input_tokens) 与 OpenAI(prompt_tokens/completion_tokens) 两种 usage 格式
        if "prompt_tokens" in usage or "completion_tokens" in usage:
            usage = _convert_usage(usage)
        if (usage.get("input_tokens") or usage.get("output_tokens")) and is_cache_stats_enabled():
            await asyncio.to_thread(
                get_stats(service.name, service.account.id).record, model, usage
            )
    except Exception as e:
        logger.warning(f"[direct] response parse failed: {e}")

    logger.info(f"[direct][nonstream] completed bytes={len(raw)}")
    return web.Response(body=raw, status=200, content_type="application/json")


async def handle_direct_stream(request: web.Request, service: Service,
                               body: bytes, req_start: float):
    """直连流式：原样透传 Anthropic 的 SSE 事件，同时抓取 usage 记统计。"""
    session = request.app["session"]
    target = build_target(service, request)
    logger.info(f"[FWD][direct] forwarding stream url={target} bytes={len(body)}")

    resp = None
    try:
        async with session.post(
            target, data=body,
            timeout=upstream_timeout(),
            **upstream_direct_headers(service),
        ) as up:
            if up.status != 200:
                err = await up.text()
                logger.error(f"[direct] upstream error {up.status}: {err[:300]}")
                return error_response(up.status, f"upstream error: {err[:300]}")

            resp = web.StreamResponse(status=200, headers={
                "Content-Type": "text/event-stream; charset=utf-8",
                "Cache-Control": "no-cache",
                "X-Accel-Buffering": "no",
            })
            await resp.prepare(request)

            model = service.model
            latest_usage = None
            try:
                while True:
                    line = await up.content.readline()
                    if not line:
                        break
                    await stream_write(resp, line)
                    if line.startswith(b"data:"):
                        data_str = line[5:].strip().decode("utf-8", errors="replace")
                        if data_str == "[DONE]":
                            break
                        try:
                            ev = json.loads(data_str)
                        except json.JSONDecodeError:
                            continue
                        # Anthropic 全量 usage 在 message_start / message_delta
                        if ev.get("type") == "message_start":
                            latest_usage = (ev.get("message") or {}).get("usage") or {}
                        elif ev.get("type") == "message_delta":
                            u = ev.get("usage") or {}
                            if u:
                                if latest_usage is None:
                                    latest_usage = {}
                                latest_usage.update(u)
                        # OpenAI 兼容流式：usage 通常出现在最后一个 chunk
                        else:
                            u = ev.get("usage")
                            if u:
                                if latest_usage is None:
                                    latest_usage = {}
                                latest_usage.update(u)
            except ClientGone:
                logger.info("Client disconnected (direct stream)")
            except asyncio.CancelledError:
                raise
            except Exception as e:
                logger.error(f"[direct] stream error: {e}", exc_info=True)
            finally:
                if latest_usage and ("prompt_tokens" in latest_usage or "completion_tokens" in latest_usage):
                    latest_usage = _convert_usage(latest_usage)
                await record_stats(service, model, latest_usage)
    except aiohttp.ClientError as e:
        err_body = str(e)
        logger.error(f"[direct] upstream failed: {err_body[:500]}")
        if resp is None:
            return error_response(502, f"upstream error: {err_body[:300]}")
        try:
            await resp.write_eof()
        except Exception:
            pass
        return resp
    return resp


# ---------------------------------------------------------------------------
# 入口分发
# ---------------------------------------------------------------------------

async def handle_request(request: web.Request):
    service = request.app["service"]
    if request.method == "GET":
        return await handle_get(request, service)

    req_start = time.time()
    try:
        body = await request.read()
    except Exception:
        return error_response(400, "read error")
    try:
        payload = json.loads(body)
    except (json.JSONDecodeError, UnicodeDecodeError):
        if service.mode == "codex":
            return openai_error_response(400, "invalid json")
        return error_response(400, "invalid json")

    # 确定分派模式（api 显式时直接采用推导，不再按请求体识别）
    mode = resolve_mode(service, request, payload)
    if mode is None:
        return openai_error_response(
            400,
            "该账号没有 OpenAI 端点，无法服务 OpenAI 客户端（Codex）。"
            "请为该账号配置 openai_url，或改用 Claude Code / Anthropic 客户端。",
        )
    if service.client == "auto" and not service.api:
        service = service.with_mode(mode)

    if mode == "codex":
        stream = bool(payload.get("stream", False))
        logger.info(f"[REQ][codex] model={service.model} stream={stream} api={service.api or 'auto'} "
                    f"upstream={service.upstream_api} bytes={len(body)} "
                    f"elapsed={time.time()-req_start:.3f}s")
        # api=openai-completions：Chat 整包透传（直连上游，零转换）
        if service.api == "openai-completions":
            return await handle_passthrough(request, service, body, stream, req_start)
        # api=openai-responses + upstream=responses：Responses 整包透传（如 DeepSeek 官方）
        if (service.api == "openai-responses"
                and service.upstream_api == "openai-responses"):
            target = _responses_url(service.account.openai_url)
            return await handle_passthrough(request, service, body, stream, req_start,
                                            target=target, responses_usage=True)
        chat = _responses_to_chat(payload, service)
        chat_body = json.dumps(chat).encode("utf-8")
        if stream:
            return await handle_openai_stream(request, service, payload, chat_body, req_start)
        return await handle_openai_non_stream(request, service, payload, chat_body, req_start)

    if service.mode == "direct":
        stream = bool(payload.get("stream", False))
        logger.info(f"[REQ][direct] model={payload.get('model')} stream={stream} "
                    f"bytes={len(body)} elapsed={time.time()-req_start:.3f}s")
        if stream:
            return await handle_direct_stream(request, service, body, req_start)
        return await handle_direct_non_stream(request, service, body, req_start)

    stream = bool(payload.get("stream", False))
    logger.info(f"[REQ] received model={payload.get('model')} stream={stream} "
                f"bytes={len(body)} messages={len(payload.get('messages', []))} "
                f"tools={len(payload.get('tools', []))} "
                f"has_thinking={'thinking' in payload} "
                f"elapsed={time.time()-req_start:.3f}s")
    openai_request = convert_request(payload, service)
    openai_request = _strip_cache_control(openai_request)
    logger.debug(f"Converted: {json.dumps(openai_request)[:500]}")
    if stream:
        return await handle_claude_stream(request, service, openai_request, req_start)
    return await handle_claude_non_stream(request, service, openai_request, req_start)


async def handle_get(request: web.Request, service: Service):
    path = request.path
    if path == "/stats":
        if not is_cache_stats_enabled():
            return json_response({"error": "cache stats is disabled"})
        period = request.query.get("period", "day")
        account_id = request.query.get("account")
        if account_id:
            # 账号级聚合：归并该账号下所有服务的 summary
            summary = await asyncio.to_thread(get_account_summary, account_id, period)
        else:
            summary = await asyncio.to_thread(get_stats(service.name).get_summary, period)
        return json_response(summary)
    if path == "/health":
        return json_response({"status": "ok"})
    return json_response({
        "status": "ok",
        "mode": service.mode,
        "client": service.client,
        "account": service.account.name,
        "target": service.target_url,
        "endpoints": ["/stats?period=hour|day|all", "/stats?account=<账号id>&period=day", "/health"],
    })


# ---------------------------------------------------------------------------
# 服务启动
# ---------------------------------------------------------------------------

def _parent_watchdog(parent_pid: int) -> None:
    """父进程退出后自动关闭代理，避免成为无法被管理的孤儿进程。"""
    while True:
        time.sleep(2.0)
        # 直接父进程已退出（被重新挂到 PID 1，或 PID 已不存在）
        if os.getppid() != parent_pid:
            logger.info("检测到父进程已退出，代理自动关闭")
            os._exit(0)

async def serve(service_filter: str = None) -> int:
    services = load_config()
    enabled = [s for s in services if s.account.valid]
    if service_filter:
        enabled = [s for s in enabled
                   if s.name == service_filter or str(s.port) == service_filter]
    if not enabled:
        logger.error("没有可用的服务（请检查 config.json 的 services 与 API key）")
        return 1

    connector = aiohttp.TCPConnector(
        limit=UPSTREAM_POOL_LIMIT,
        limit_per_host=UPSTREAM_POOL_LIMIT,
        ttl_dns_cache=300,
        enable_cleanup_closed=True,
    )
    session = aiohttp.ClientSession(connector=connector)

    runners = []
    try:
        for svc in enabled:
            app = web.Application(client_max_size=MAX_BODY_SIZE)
            app["service"] = svc
            app["session"] = session
            app.router.add_route("*", "/{tail:.*}", handle_request)
            runner = web.AppRunner(app, access_log=None)
            await runner.setup()
            site = web.TCPSite(runner, svc.host, svc.port)
            await site.start()
            runners.append(runner)
            logger.info(f"代理启动: http://{svc.host}:{svc.port} mode={svc.mode} "
                        f"target={svc.target_url} model={svc.model}")

        logger.info("asyncio 代理就绪，Ctrl+C 停止")
        # 守护：父进程（桌面应用）退出后自动关闭，防止孤儿进程占用端口
        parent_pid = os.getppid()
        watchdog = asyncio.create_task(asyncio.to_thread(_parent_watchdog, parent_pid))
        while True:
            await asyncio.sleep(3600)
    except KeyboardInterrupt:
        logger.info("收到中断信号，关闭代理")
    finally:
        await session.close()
        for r in runners:
            await r.cleanup()
    return 0


def main():
    service_filter = None
    if "--service" in sys.argv:
        i = sys.argv.index("--service")
        if i + 1 < len(sys.argv):
            service_filter = sys.argv[i + 1]
    rc = asyncio.run(serve(service_filter))
    sys.exit(rc)


if __name__ == "__main__":
    main()
