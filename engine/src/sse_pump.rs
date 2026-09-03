//! SSE 流式泵共享骨架。
//!
//! 四路流式（claude 转换 / codex passthrough / codex 转换 / direct 透传）的
//! 字节循环完全同构：`timeout(STREAM_TIMEOUT, up.chunk())` 四臂分派 + 行缓冲
//! 切分（CRLF）+ 客户端断连即退出（send 失败）。本模块把这段 I/O 骨架收敛为
//! 自由函数；各路的协议差异（翻译器、旁路提取、错误事件门控）保留在各自
//! 的泵循环里，语义就地可见。

use axum::body::Bytes;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Value};
use tokio::sync::mpsc;

use o2a_convert::sse_event;

use crate::state::STREAM_TIMEOUT;

/// 客户端 SSE 通道（接收端被 drop = 客户端断连，send 失败 → 泵退出）。
pub(crate) type SseTx = mpsc::Sender<Result<Bytes, std::io::Error>>;

/// 泵结束语义。
pub(crate) enum PumpOutcome {
    /// 正常收尾（[DONE]/EOF/总超时）
    Completed,
    /// 流内异常（读超时/读错误）
    Error(String),
    /// 客户端断连：静默退出，取消上游
    ClientGone,
}

/// 上游单块读取（带读间隔超时，对齐 aiohttp sock_read=STREAM_TIMEOUT）。
///
/// `Err(msg)` = 读间隔超时（固定文案）或上游读错误（`e.to_string()`）；
/// 四路泵对这两臂的处理本就完全一致（同格式日志 + 同格式错误事件）。
pub(crate) async fn next_chunk(
    up: &mut reqwest::Response,
) -> Result<Option<Bytes>, String> {
    match tokio::time::timeout(STREAM_TIMEOUT, up.chunk()).await {
        Err(_) => Err("upstream read timeout".to_string()),
        Ok(Ok(None)) => Ok(None),
        Ok(Ok(Some(bytes))) => Ok(Some(bytes)),
        Ok(Err(e)) => Err(e.to_string()),
    }
}

/// 发送一个 SSE 事件帧；false = 客户端断连。
pub(crate) async fn sse_send(tx: &SseTx, ev: &Value) -> bool {
    tx.send(Ok(Bytes::from(sse_event(ev)))).await.is_ok()
}

/// 流内错误事件（四路共用格式，对齐 Python 的 type=error / api_error）。
pub(crate) async fn send_stream_error(tx: &SseTx, msg: &str) -> bool {
    sse_send(
        tx,
        &json!({"type": "error", "error": {"type": "api_error", "message": msg}}),
    )
    .await
}

/// 原样转发一行（补回换行符；对齐 Python stream_write(resp, line)）。
pub(crate) async fn raw_line(tx: &SseTx, line: &[u8]) -> bool {
    let mut out = line.to_vec();
    out.push(b'\n');
    tx.send(Ok(Bytes::from(out))).await.is_ok()
}

/// 从行缓冲切出一个完整行（去除行尾 \n / \r\n）。无完整行返回 None，
/// 尾部不完整行留待下一个 chunk。
pub(crate) fn split_line(buf: &mut Vec<u8>) -> Option<Vec<u8>> {
    let pos = buf.iter().position(|&b| b == b'\n')?;
    let mut line: Vec<u8> = buf.drain(..=pos).collect();
    line.pop(); // \n
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    Some(line)
}

/// 客户端 SSE 响应（Content-Type / no-cache / 禁缓冲三件套）。
pub(crate) fn sse_response(rx: mpsc::Receiver<Result<Bytes, std::io::Error>>) -> Response {
    let stream_body = axum::body::Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx));
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, HeaderValue::from_static("text/event-stream; charset=utf-8")),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-cache")),
            (header::HeaderName::from_static("x-accel-buffering"), HeaderValue::from_static("no")),
        ],
        stream_body,
    )
        .into_response()
}
