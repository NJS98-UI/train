//! 手写最小 HTTP/1.1 解析与响应写出（LocalFakeServer 的 readLineRaw/writeRaw/SSE 头 移植）。
//! 依据 `.monkeycode/analysis/gateway-server.md` §1.3 / §1.4 / §1.7 / §1.8 / §3.3(头)。
//! 说明：tw-core 的 Cargo.toml 未直接依赖 hyper/hyper-util（hyper 仅 reqwest 传递依赖，
//! server 路径不可用）；按任务约束不改 Cargo.toml，用 tokio + 手写解析实现同一逐字节语义。

use serde_json::Value;
use std::io;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::time::{timeout, Duration};

/// 单连接读/写操作超时（对齐 `soTimeout=15_000`：既约束读也约束阻塞写）。
pub const IO_TIMEOUT: Duration = Duration::from_secs(15);

/// 请求体上限（§7.1 `MAX_BODY_BYTES = 8*1024*1024`，安全边界；chunked 路径统一补齐上限）。
pub const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

/// 解析出的请求头（只关心 Kotlin 侧那 3 个头；同头重复时最后一个值生效）。
#[derive(Debug, Clone, Default)]
pub struct RequestHead {
    pub method: String,
    pub path: String,
    pub auth: Option<String>,
    pub content_length: i64,
    pub chunked: bool,
}

/// 逐字节读一行到 `\n`，去掉尾部 `\r`；EOF 且无内容返回 None；EOF 有内容返回已读部分。
/// 每行整体限时 `IO_TIMEOUT`（对齐 soTimeout 的"15s 无数据"意图，慢速滴漏被上限得更严）。
pub async fn read_line_raw<R: AsyncRead + Unpin>(rd: &mut BufReader<R>) -> io::Result<Option<String>> {
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let r = timeout(IO_TIMEOUT, rd.read_until(b'\n', &mut buf)).await;
        match r {
            Err(_) => return Err(io::Error::new(io::ErrorKind::TimedOut, "read timeout")),
            Ok(Ok(0)) => {
                if buf.is_empty() {
                    return Ok(None);
                }
                let s = String::from_utf8_lossy(&buf).to_string();
                return Ok(Some(strip_cr(&s)));
            }
            Ok(Ok(n)) => {
                if buf[n - 1] == b'\n' {
                    buf.truncate(n - 1);
                    let s = String::from_utf8_lossy(&buf).to_string();
                    return Ok(Some(strip_cr(&s)));
                }
                // 还没读到换行，继续
            }
            Ok(Err(e)) => return Err(e),
        }
    }
}

fn strip_cr(s: &str) -> String {
    s.strip_suffix('\r').map(|x| x.to_string()).unwrap_or_else(|| s.to_string())
}

/// 读请求行 + 头部。None = 静默关连接（请求行 EOF 或不足 2 段，无响应——对齐 Kotlin）。
pub async fn read_request_head<R: AsyncRead + Unpin>(
    rd: &mut BufReader<R>,
) -> io::Result<Option<RequestHead>> {
    // 跳过空行后取请求行
    loop {
        let line = match read_line_raw(rd).await? {
            Some(l) => l,
            None => return Ok(None),
        };
        if !line.is_empty() {
            let parts: Vec<&str> = line.split(' ').collect();
            if parts.len() < 2 {
                return Ok(None); // 不足 2 段：静默关闭
            }
            let mut head = RequestHead { method: parts[0].to_string(), path: parts[1].to_string(), ..Default::default() };
            // 其余头直到空行/EOF
            loop {
                match read_line_raw(rd).await? {
                    None => break,
                    Some(l) if l.is_empty() => break,
                    Some(l) => {
                        // 前缀匹配大小写不敏感；值 = 第一个 ':' 之后 trim
                        if let Some(v) = header_value(&l, "authorization") {
                            head.auth = Some(v);
                        } else if let Some(v) = header_value(&l, "content-length") {
                            head.content_length = v.parse::<i64>().unwrap_or(0);
                        } else if let Some(v) = header_value(&l, "transfer-encoding") {
                            if v.to_lowercase().contains("chunked") {
                                head.chunked = true;
                            }
                        }
                    }
                }
            }
            return Ok(Some(head));
        }
    }
}

/// `Name: value` 大小写不敏感前缀匹配，取第一个 ':' 后全部并 trim。
fn header_value(line: &str, name_lower: &str) -> Option<String> {
    let (name, rest) = line.split_once(':')?;
    if name.trim().to_lowercase() == name_lower {
        Some(rest.trim().to_string())
    } else {
        None
    }
}

/// 路径归一化（§1.4）：去查询串 → 连续 `/` 折叠 → 长度>1 去尾 `/`（去空退化 `/`）。
/// 不碰大小写、不做模糊匹配。
pub fn normalize_path(path: &str) -> String {
    let no_query = path.split('?').next().unwrap_or(path);
    let mut folded = String::with_capacity(no_query.len());
    let mut prev_slash = false;
    for c in no_query.chars() {
        if c == '/' {
            if !prev_slash {
                folded.push('/');
            }
            prev_slash = true;
        } else {
            folded.push(c);
            prev_slash = false;
        }
    }
    if folded.len() > 1 {
        let trimmed = folded.trim_end_matches('/');
        if trimmed.is_empty() {
            "/".to_string()
        } else {
            trimmed.to_string()
        }
    } else {
        folded
    }
}

/// `Content-Length` 精确按字节读满（§1.8）。EOF 提前则返回已收部分；超限 Err（上层 → 413）。
pub async fn read_body_exact<R: AsyncRead + Unpin>(
    rd: &mut BufReader<R>,
    content_length: i64,
) -> io::Result<String> {
    if content_length <= 0 {
        return Ok(String::new());
    }
    let len = content_length as usize;
    if len > MAX_BODY_BYTES {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "请求体过大"));
    }
    let mut bytes = vec![0u8; len];
    let mut off = 0usize;
    while off < len {
        let r = timeout(IO_TIMEOUT, rd.read(&mut bytes[off..len])).await;
        match r {
            Err(_) => return Err(io::Error::new(io::ErrorKind::TimedOut, "read timeout")),
            Ok(Ok(0)) => break, // EOF：短读，已收部分即全部（对齐 Kotlin）
            Ok(Ok(n)) => off += n,
            Ok(Err(e)) => return Err(e),
        }
    }
    Ok(String::from_utf8_lossy(&bytes[..off]).to_string())
}

/// `Transfer-Encoding: chunked` 请求体（§1.8，Chatbox 实测这样发）。
/// 分块长度按**字节**算；长度行取 `;` 前段按 16 进制；0 块终止（trailer 不读）；
/// EOF 短读截断。相对 Kotlin 修复：累计超 `MAX_BODY_BYTES` → Err（上层 → 413）。
pub async fn read_chunked_body<R: AsyncRead + Unpin>(rd: &mut BufReader<R>) -> io::Result<String> {
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let size_line = match read_line_raw(rd).await? {
            Some(l) => l,
            None => break,
        };
        let size = match size_line.split(';').next().map(str::trim).and_then(|s| u64::from_str_radix(s, 16).ok()) {
            Some(sz) => sz,
            None => break, // 非法长度行：按 Kotlin 语义 break，返回已收部分
        };
        if size == 0 {
            break; // 末块
        }
        if buf.len() + size as usize > MAX_BODY_BYTES {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "请求体过大"));
        }
        let need = size as usize;
        let start = buf.len();
        buf.resize(start + need, 0);
        let mut off = start;
        while off < start + need {
            let r = timeout(IO_TIMEOUT, rd.read(&mut buf[off..start + need])).await;
            match r {
                Err(_) => return Err(io::Error::new(io::ErrorKind::TimedOut, "read timeout")),
                Ok(Ok(0)) => break,
                Ok(Ok(n)) => off += n,
                Ok(Err(e)) => return Err(e),
            }
        }
        buf.truncate(off);
        let _ = read_line_raw(rd).await?; // 吃掉本块末尾 CRLF 行
        if off < start + need {
            break; // 连接提前断开：已收部分就是全部
        }
    }
    Ok(String::from_utf8_lossy(&buf).to_string())
}

fn reason_for(status: u16) -> &'static str {
    // §1.7：200→OK，204→No Content，401→Unauthorized，404→Not Found，其余（含 413/502/400）→Error
    match status {
        200 => "OK",
        204 => "No Content",
        401 => "Unauthorized",
        404 => "Not Found",
        _ => "Error",
    }
}

/// writeRaw（§1.7 逐字节格式）：状态行 + Content-Type + 全套 4 个 CORS 头 + 长度 + close。
pub async fn write_raw<W: AsyncWrite + Unpin>(
    w: &mut W,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> io::Result<()> {
    let mut out: Vec<u8> = Vec::with_capacity(body.len() + 256);
    out.extend_from_slice(format!("HTTP/1.1 {} {}\r\n", status, reason_for(status)).as_bytes());
    out.extend_from_slice(format!("Content-Type: {}\r\n", content_type).as_bytes());
    // CORS：每一个非流式响应都要带（§1.6）
    out.extend_from_slice(b"Access-Control-Allow-Origin: *\r\n");
    out.extend_from_slice(b"Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n");
    out.extend_from_slice(b"Access-Control-Allow-Headers: *\r\n");
    out.extend_from_slice(b"Access-Control-Max-Age: 86400\r\n");
    out.extend_from_slice(format!("Content-Length: {}\r\n", body.len()).as_bytes());
    out.extend_from_slice(b"Connection: close\r\n\r\n");
    out.extend_from_slice(body);
    timeout(IO_TIMEOUT, w.write_all(&out)).await.map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "write timeout"))??;
    Ok(())
}

/// writeJson = writeRaw(status, "application/json", body)
pub async fn write_json<W: AsyncWrite + Unpin>(w: &mut W, status: u16, obj: &Value) -> io::Result<()> {
    write_raw(w, status, "application/json", obj.to_string().as_bytes()).await
}

/// 流式响应头（§3.3 步骤 1，手写、只带两个 CORS 头，无长度无 chunked、EOF 界定）。
pub const SSE_HEADER: &[u8] = b"HTTP/1.1 200 OK\r\n\
Content-Type: text/event-stream\r\n\
Cache-Control: no-cache\r\n\
Access-Control-Allow-Origin: *\r\n\
Access-Control-Allow-Headers: *\r\n\
Connection: close\r\n\r\n";

pub async fn write_sse_header<W: AsyncWrite + Unpin>(w: &mut W) -> io::Result<()> {
    timeout(IO_TIMEOUT, w.write_all(SSE_HEADER)).await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "write timeout"))?
}

/// §2.2 openAIError：统一形状 `{"error":{"message","type":"api_error","code"}}`，status 不进 body。
pub fn openai_error(code: &str, msg: &str) -> Value {
    serde_json::json!({"error": {"message": msg, "type": "api_error", "code": code}})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_path_cases() {
        assert_eq!(normalize_path("/v1/models"), "/v1/models");
        assert_eq!(normalize_path("/v1//models/"), "/v1/models");
        assert_eq!(normalize_path("/v1///models///"), "/v1/models");
        assert_eq!(normalize_path("/"), "/");
        assert_eq!(normalize_path("/v1/"), "/v1");
        assert_eq!(normalize_path("//"), "/"); // 折叠后长度>1？"//"→折叠"/"，长度1保持
        assert_eq!(normalize_path("/healthz?api_key=secret"), "/healthz");
        assert_eq!(normalize_path("/v1/chat/completions?x=1"), "/v1/chat/completions");
        assert_eq!(normalize_path("/chat/completions"), "/chat/completions");
        assert_eq!(normalize_path("/models/"), "/models");
        assert_eq!(normalize_path("/v1/models/"), "/v1/models");
    }

    #[tokio::test]
    async fn read_head_basic() {
        let raw = b"POST /v1/chat/completions?x=1 HTTP/1.1\r\nHost: localhost\r\n\
Authorization: Bearer sk-abc\r\nContent-Length: 5\r\n\r\n12345";
        let mut rd = BufReader::new(&raw[..]);
        let head = read_request_head(&mut rd).await.unwrap().unwrap();
        assert_eq!(head.method, "POST");
        assert_eq!(head.path, "/v1/chat/completions?x=1");
        assert_eq!(head.auth.as_deref(), Some("Bearer sk-abc"));
        assert_eq!(head.content_length, 5);
        assert!(!head.chunked);
        let body = read_body_exact(&mut rd, head.content_length).await.unwrap();
        assert_eq!(body, "12345");
    }

    #[tokio::test]
    async fn read_head_skips_leading_empty_lines_last_header_wins() {
        let raw = b"\r\nGET /healthz HTTP/1.0\r\nauthorization: a\r\nAuthorization: b\r\n\r\n";
        let mut rd = BufReader::new(&raw[..]);
        let head = read_request_head(&mut rd).await.unwrap().unwrap();
        assert_eq!(head.method, "GET");
        assert_eq!(head.auth.as_deref(), Some("b"), "同头重复最后一个值生效");
    }

    #[tokio::test]
    async fn read_head_short_line_silent_close() {
        let raw = b"GARBAGE\r\n\r\n";
        let mut rd = BufReader::new(&raw[..]);
        assert!(read_request_head(&mut rd).await.unwrap().is_none(), "不足 2 段应静默关闭");
    }

    #[tokio::test]
    async fn read_head_eof_returns_none() {
        let raw: &[u8] = b"";
        let mut rd = BufReader::new(raw);
        assert!(read_request_head(&mut rd).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn read_body_utf8_read_by_bytes_selfcheck() {
        // selfCheck #1：中文 body 字节数 > 字符数，必须按字节读满并回读一致
        let json = r#"{"model":"glm-5.2","messages":[{"role":"user","content":"你好，续期"}]}"#;
        let bytes = json.as_bytes();
        assert!(bytes.len() > json.chars().count(), "自检前提：中文 body 字节数应大于字符数");
        let mut rd = BufReader::new(&bytes[..]);
        let got = read_body_exact(&mut rd, bytes.len() as i64).await.unwrap();
        assert_eq!(got, json);
        // 短 EOF：返回已收部分（对齐 Kotlin break 语义）
        let mut rd2 = BufReader::new(&bytes[..5]);
        let got2 = read_body_exact(&mut rd2, bytes.len() as i64).await.unwrap();
        assert_eq!(got2.as_bytes().len(), 5);
        // 超限 → Err → 413
        let mut rd3 = BufReader::new(&bytes[..]);
        assert!(read_body_exact(&mut rd3, (MAX_BODY_BYTES + 1) as i64).await.is_err());
    }

    #[tokio::test]
    async fn chunked_parsing_selfcheck() {
        // selfCheck #3：`5\r\n{"a":\r\n9\r\n"你好"}\r\n0\r\n\r\n` → `{"a":"你好"}`（分块长度是字节数）
        let raw = "5\r\n{\"a\":\r\n9\r\n\"你好\"}\r\n0\r\n\r\n";
        let mut rd = BufReader::new(raw.as_bytes());
        let got = read_chunked_body(&mut rd).await.unwrap();
        assert_eq!(got, "{\"a\":\"你好\"}", "chunked 解析结果不对");
        // 3 字节中文按字节切块不被劈开
        let raw2 = "3\r\n你\r\n0\r\n\r\n";
        let mut rd2 = BufReader::new(raw2.as_bytes());
        assert_eq!(read_chunked_body(&mut rd2).await.unwrap(), "你");
        // 带 chunk 扩展（;ext）
        let raw3 = "5;ext=1\r\nhello\r\n0\r\n\r\n";
        let mut rd3 = BufReader::new(raw3.as_bytes());
        assert_eq!(read_chunked_body(&mut rd3).await.unwrap(), "hello");
        // EOF 短读：截断返回已收部分
        let raw4 = "a\r\n1234";
        let mut rd4 = BufReader::new(raw4.as_bytes());
        assert_eq!(read_chunked_body(&mut rd4).await.unwrap(), "1234");
    }

    #[tokio::test]
    async fn chunked_respects_max_body() {
        // Rust 移植修复：chunked 路径统一 8MiB 上限（Kotlin 已知缺口）
        let fake_len = "800001\r\n"; // 8,388,609 > 8MiB
        let raw = format!("{}x", fake_len);
        let mut rd = BufReader::new(raw.as_bytes());
        assert!(read_chunked_body(&mut rd).await.is_err(), "chunked 应受 8MiB 上限约束");
    }

    #[tokio::test]
    async fn write_raw_format_is_byte_exact() {
        // §1.7 逐字节格式 + §1.6 CORS 4 头 + reason 映射
        let mut w: Vec<u8> = Vec::new();
        write_raw(&mut w, 200, "text/plain", b"ok").await.unwrap();
        assert_eq!(
            String::from_utf8(w).unwrap(),
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Allow-Headers: *\r\nAccess-Control-Max-Age: 86400\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"
        );
        let mut w: Vec<u8> = Vec::new();
        write_raw(&mut w, 204, "text/plain", b"").await.unwrap();
        assert!(String::from_utf8_lossy(&w).starts_with("HTTP/1.1 204 No Content\r\n"));
        let mut w: Vec<u8> = Vec::new();
        write_raw(&mut w, 401, "application/json", b"{}").await.unwrap();
        assert!(String::from_utf8_lossy(&w).starts_with("HTTP/1.1 401 Unauthorized\r\n"));
        let mut w: Vec<u8> = Vec::new();
        write_raw(&mut w, 404, "application/json", b"{}").await.unwrap();
        assert!(String::from_utf8_lossy(&w).starts_with("HTTP/1.1 404 Not Found\r\n"));
        // 400/413/502 → reason "Error"
        for st in [400u16, 413, 502] {
            let mut w: Vec<u8> = Vec::new();
            write_raw(&mut w, st, "application/json", b"{}").await.unwrap();
            assert!(String::from_utf8_lossy(&w).starts_with(&format!("HTTP/1.1 {} Error\r\n", st)), "{}", st);
        }
        // Content-Length 按 UTF-8 字节数而非字符数
        let mut w: Vec<u8> = Vec::new();
        write_raw(&mut w, 200, "text/plain", "你好".as_bytes()).await.unwrap();
        assert!(String::from_utf8_lossy(&w).contains("Content-Length: 6\r\n"));
    }

    #[test]
    fn sse_header_is_byte_exact() {
        // §3.3：流式头手写，只带 Origin/Headers 两个 CORS 头
        let want = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Headers: *\r\nConnection: close\r\n\r\n";
        // 常量里的续行拼接必须与此逐字节一致
        assert_eq!(std::str::from_utf8(SSE_HEADER).unwrap(), want);
    }

    #[test]
    fn openai_error_shape() {
        // §2.2：type 恒 api_error；status 数字不进 body
        let v = openai_error("invalid_api_key", "missing or invalid local API key");
        assert_eq!(v["error"]["message"], "missing or invalid local API key");
        assert_eq!(v["error"]["type"], "api_error");
        assert_eq!(v["error"]["code"], "invalid_api_key");
        assert!(v["error"].get("status").is_none());
        assert_eq!(v["error"].as_object().unwrap().len(), 3);
    }
}
