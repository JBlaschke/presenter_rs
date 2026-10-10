//! File responses of the dev server: whole files, single byte ranges (RFC 9110 §14) and
//! file-backed bodies streamed from disk.
//!
//! Browsers fetch media with `Range` requests, and Safari plays no `<video>` from a server
//! without them, so every asset route answers `Range: bytes=…` (`a-b`, `a-` or `-n`) with
//! `206 Partial Content`, sends `Accept-Ranges: bytes` with the whole file and answers a
//! range past the end with `416 Range Not Satisfiable`. Several ranges in one request, an
//! unknown unit, an invalid range or an `If-Range` (the server sends no validators to
//! match) get the whole file, as RFC 9110 allows.
//!
//! A file-backed deck asset is never loaded into memory as a whole: [`file_response`]
//! streams the requested bytes in chunks, so that a long video costs one chunk per
//! connection at a time and nothing once the browser has closed it.

use std::fs::File;
use std::io::{self, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::{Context, Poll};

use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use tokio::io::{AsyncRead, ReadBuf};
use tokio_stream::Stream;

/// Size of the chunks a streamed file is read in.
const CHUNK: usize = 64 * 1024;

/// A `200 OK` with all of `bytes`, `Content-Type: mime` (`; charset=utf-8` for text) and
/// `Cache-Control: cache`; no range support (for documents and stylesheets).
pub(crate) fn whole_response(bytes: Vec<u8>, mime: &str, cache: &'static str) -> Response {
    (headers(mime, cache), bytes).into_response()
}

/// `bytes` as the response to a request with `request` headers: the requested byte range
/// (`206`), `416` for a range past the end, else all of it (`200`, `Accept-Ranges: bytes`).
pub(crate) fn bytes_response(
    bytes: Vec<u8>,
    mime: &str,
    cache: &'static str,
    request: &HeaderMap,
) -> Response {
    let len = bytes.len() as u64;
    match ByteRange::resolve(range_spec(request), len) {
        ByteRange::Full => ranged(mime, cache, Span::Whole(len), Body::from(bytes)),
        ByteRange::Partial { start, end } => {
            // `start <= end < len`, which came from a `usize`: the conversions are exact.
            let slice = bytes
                .get(start as usize..=end as usize)
                .unwrap_or_default()
                .to_vec();
            ranged(mime, cache, Span::Part(start, end, len), Body::from(slice))
        }
        ByteRange::Unsatisfiable => unsatisfiable(len, cache),
    }
}

/// The file at `path` as the response to a request with `request` headers, like
/// [`bytes_response`], with the body streamed from disk. Errors: opening or seeking the
/// file; a directory is [`io::ErrorKind::NotFound`].
pub(crate) async fn file_response(
    path: PathBuf,
    mime: &str,
    cache: &'static str,
    request: &HeaderMap,
) -> io::Result<Response> {
    let spec = range_spec(request);
    let opened = tokio::task::spawn_blocking(move || open_range(&path, spec))
        .await
        .map_err(io::Error::other)??;
    let (file, len, range) = opened;
    let response = match range {
        ByteRange::Full => {
            let body = Body::from_stream(FileStream::new(file, len));
            ranged(mime, cache, Span::Whole(len), body)
        }
        ByteRange::Partial { start, end } => {
            let body = Body::from_stream(FileStream::new(file, end - start + 1));
            ranged(mime, cache, Span::Part(start, end, len), body)
        }
        ByteRange::Unsatisfiable => unsatisfiable(len, cache),
    };
    Ok(response)
}

/// Opens `path`, resolves `spec` against its length and seeks to the range's start.
fn open_range(path: &Path, spec: Option<RangeSpec>) -> io::Result<(File, u64, ByteRange)> {
    let not_a_file = || io::Error::new(io::ErrorKind::NotFound, "not a file");
    let mut file = match File::open(path) {
        Ok(file) => file,
        // Windows refuses to open a directory (permission denied); Unix opens it, and the
        // metadata below says what it is. The same answer on both: not found.
        Err(_) if std::fs::metadata(path).is_ok_and(|metadata| metadata.is_dir()) => {
            return Err(not_a_file());
        }
        Err(err) => return Err(err),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(not_a_file());
    }
    let len = metadata.len();
    let range = ByteRange::resolve(spec, len);
    if let ByteRange::Partial { start, .. } = range {
        file.seek(SeekFrom::Start(start))?;
    }
    Ok((file, len, range))
}

/// What a `200`/`206` response's body is.
#[derive(Debug, Clone, Copy)]
enum Span {
    /// All `len` bytes (`200`).
    Whole(u64),
    /// Bytes `start..=end` of `len` (`206`).
    Part(u64, u64, u64),
}

/// A `200` or `206` response with `body`, which holds exactly the bytes of `span`.
fn ranged(mime: &str, cache: &'static str, span: Span, body: Body) -> Response {
    let mut headers = headers(mime, cache);
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    let (status, length) = match span {
        Span::Whole(len) => (StatusCode::OK, len),
        Span::Part(start, end, len) => {
            let range = format!("bytes {start}-{end}/{len}");
            headers.insert(header::CONTENT_RANGE, header_value(range));
            (StatusCode::PARTIAL_CONTENT, end - start + 1)
        }
    };
    // A streamed body has no size of its own; the browser needs it to seek.
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
    (status, headers, body).into_response()
}

/// `416 Range Not Satisfiable` for a representation of `len` bytes.
fn unsatisfiable(len: u64, cache: &'static str) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(
        header::CONTENT_RANGE,
        header_value(format!("bytes */{len}")),
    );
    (StatusCode::RANGE_NOT_SATISFIABLE, headers).into_response()
}

/// `Content-Type` (with `; charset=utf-8` for text) and `Cache-Control`.
fn headers(mime: &str, cache: &'static str) -> HeaderMap {
    let text = mime.starts_with("text/") || matches!(mime, "application/javascript");
    let content_type = if text {
        format!("{mime}; charset=utf-8")
    } else {
        mime.to_string()
    };
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, header_value(content_type));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    headers
}

/// `value` as a header value; a value that is not one (never the case for MIME types and
/// ranges) becomes `application/octet-stream`.
fn header_value(value: String) -> HeaderValue {
    HeaderValue::try_from(value)
        .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"))
}

/// A single byte range as requested, before the length is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RangeSpec {
    /// `bytes=start-` or `bytes=start-end` (inclusive).
    From {
        /// First byte.
        start: u64,
        /// Last byte, if given.
        end: Option<u64>,
    },
    /// `bytes=-n`: the last `n` bytes.
    Suffix(u64),
}

/// The single byte range `request` asks for; `None` for the whole file: no `Range`, an
/// `If-Range`, a unit other than `bytes`, several ranges or an invalid one.
fn range_spec(request: &HeaderMap) -> Option<RangeSpec> {
    if request.contains_key(header::IF_RANGE) {
        return None;
    }
    let value = request.get(header::RANGE)?.to_str().ok()?.trim();
    let (unit, set) = value.split_once('=')?;
    if !unit.trim().eq_ignore_ascii_case("bytes") || set.contains(',') {
        return None;
    }
    let (first, last) = set.trim().split_once('-')?;
    let (first, last) = (first.trim(), last.trim());
    let number = |text: &str| -> Option<u64> {
        // `u64::from_str` would also take a leading `+`.
        text.bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| text.parse().ok())?
    };
    if first.is_empty() {
        return Some(RangeSpec::Suffix(number(last)?));
    }
    let start = number(first)?;
    if last.is_empty() {
        return Some(RangeSpec::From { start, end: None });
    }
    let end = number(last)?;
    (end >= start).then_some(RangeSpec::From {
        start,
        end: Some(end),
    })
}

/// A requested range resolved against the representation's length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ByteRange {
    /// The whole representation.
    Full,
    /// Bytes `start..=end`, both within the representation.
    Partial {
        /// First byte.
        start: u64,
        /// Last byte (inclusive).
        end: u64,
    },
    /// Nothing of the range exists (`416`).
    Unsatisfiable,
}

impl ByteRange {
    /// `spec` against a representation of `len` bytes (RFC 9110 §14.1.1): a range that
    /// starts at or after the end, or an empty suffix, is unsatisfiable; an end past the
    /// end and a suffix longer than the representation are cut to it.
    fn resolve(spec: Option<RangeSpec>, len: u64) -> ByteRange {
        match spec {
            None => ByteRange::Full,
            Some(RangeSpec::Suffix(0)) => ByteRange::Unsatisfiable,
            // A non-empty suffix of nothing: the whole (empty) representation.
            Some(RangeSpec::Suffix(_)) if len == 0 => ByteRange::Full,
            Some(RangeSpec::Suffix(n)) => ByteRange::Partial {
                start: len.saturating_sub(n),
                end: len - 1,
            },
            Some(RangeSpec::From { start, .. }) if start >= len => ByteRange::Unsatisfiable,
            Some(RangeSpec::From { start, end }) => ByteRange::Partial {
                start,
                end: end.map_or(len - 1, |end| end.min(len - 1)),
            },
        }
    }
}

/// The next `remaining` bytes of a file, read in [`CHUNK`]s on tokio's blocking pool.
struct FileStream {
    file: tokio::fs::File,
    remaining: u64,
    buf: Vec<u8>,
}

impl FileStream {
    /// Streams `remaining` bytes of `file` from its current position.
    fn new(file: File, remaining: u64) -> Self {
        FileStream {
            file: tokio::fs::File::from_std(file),
            remaining,
            buf: Vec::new(),
        }
    }
}

impl Stream for FileStream {
    type Item = io::Result<Vec<u8>>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.remaining == 0 {
            return Poll::Ready(None);
        }
        let want = usize::try_from(this.remaining).map_or(CHUNK, |n| n.min(CHUNK));
        this.buf.resize(want, 0);
        let mut buf = ReadBuf::new(&mut this.buf);
        match Pin::new(&mut this.file).poll_read(cx, &mut buf) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(err)) => {
                this.remaining = 0;
                Poll::Ready(Some(Err(err)))
            }
            Poll::Ready(Ok(())) if buf.filled().is_empty() => {
                // The file got shorter than its announced length.
                this.remaining = 0;
                let err = io::Error::new(io::ErrorKind::UnexpectedEof, "file truncated");
                Poll::Ready(Some(Err(err)))
            }
            Poll::Ready(Ok(())) => {
                let chunk = buf.filled().to_vec();
                this.remaining -= chunk.len() as u64;
                Poll::Ready(Some(Ok(chunk)))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use tokio_stream::StreamExt;

    use super::*;

    fn request(range: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::RANGE, HeaderValue::from_str(range).unwrap());
        headers
    }

    fn spec(range: &str) -> Option<RangeSpec> {
        range_spec(&request(range))
    }

    #[test]
    fn single_byte_ranges_are_parsed() {
        let from = |start, end| Some(RangeSpec::From { start, end });
        assert_eq!(spec("bytes=0-1"), from(0, Some(1)));
        assert_eq!(spec("bytes=5-"), from(5, None));
        assert_eq!(spec("bytes=-3"), Some(RangeSpec::Suffix(3)));
        assert_eq!(spec(" Bytes = 2 - 4 "), from(2, Some(4)));
        assert_eq!(spec("bytes=7-7"), from(7, Some(7)));
        // The whole file instead: invalid, several ranges, other units.
        for whole in [
            "bytes=4-2",
            "bytes=0-1,5-6",
            "items=0-1",
            "bytes=",
            "bytes=-",
            "bytes=a-b",
            "bytes=+1-2",
            "bytes=1",
            "0-1",
        ] {
            assert_eq!(spec(whole), None, "{whole}");
        }
        assert_eq!(range_spec(&HeaderMap::new()), None);
        // No validators are sent, so an If-Range can never match.
        let mut conditional = request("bytes=0-1");
        conditional.insert(header::IF_RANGE, HeaderValue::from_static("\"x\""));
        assert_eq!(range_spec(&conditional), None);
    }

    #[test]
    fn ranges_resolve_against_the_length() {
        let resolve = |range: &str, len| ByteRange::resolve(spec(range), len);
        let partial = |start, end| ByteRange::Partial { start, end };
        assert_eq!(resolve("bytes=0-1", 10), partial(0, 1));
        assert_eq!(resolve("bytes=8-20", 10), partial(8, 9));
        assert_eq!(resolve("bytes=3-", 10), partial(3, 9));
        assert_eq!(resolve("bytes=-4", 10), partial(6, 9));
        assert_eq!(resolve("bytes=-40", 10), partial(0, 9));
        assert_eq!(resolve("bytes=10-", 10), ByteRange::Unsatisfiable);
        assert_eq!(resolve("bytes=10-12", 10), ByteRange::Unsatisfiable);
        assert_eq!(resolve("bytes=-0", 10), ByteRange::Unsatisfiable);
        assert_eq!(resolve("bytes=0-", 0), ByteRange::Unsatisfiable);
        assert_eq!(resolve("bytes=-5", 0), ByteRange::Full);
        assert_eq!(ByteRange::resolve(None, 10), ByteRange::Full);
    }

    fn header_of(response: &Response, name: header::HeaderName) -> Option<&str> {
        response.headers().get(name).and_then(|v| v.to_str().ok())
    }

    async fn body_of(response: Response) -> Vec<u8> {
        let mut body = Vec::new();
        let mut stream = response.into_body().into_data_stream();
        while let Some(chunk) = stream.next().await {
            body.extend_from_slice(&chunk.unwrap());
        }
        body
    }

    #[tokio::test]
    async fn byte_responses_answer_ranges() {
        let bytes: Vec<u8> = (0..10).collect();
        let whole = bytes_response(bytes.clone(), "video/mp4", "no-store", &HeaderMap::new());
        assert_eq!(whole.status(), StatusCode::OK);
        assert_eq!(header_of(&whole, header::ACCEPT_RANGES), Some("bytes"));
        assert_eq!(header_of(&whole, header::CONTENT_LENGTH), Some("10"));
        assert_eq!(header_of(&whole, header::CONTENT_RANGE), None);
        assert_eq!(body_of(whole).await, bytes);

        let part = bytes_response(
            bytes.clone(),
            "video/mp4",
            "no-store",
            &request("bytes=2-4"),
        );
        assert_eq!(part.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            header_of(&part, header::CONTENT_RANGE),
            Some("bytes 2-4/10")
        );
        assert_eq!(header_of(&part, header::CONTENT_LENGTH), Some("3"));
        assert_eq!(header_of(&part, header::CONTENT_TYPE), Some("video/mp4"));
        assert_eq!(header_of(&part, header::CACHE_CONTROL), Some("no-store"));
        assert_eq!(body_of(part).await, [2, 3, 4]);

        let past = bytes_response(bytes, "video/mp4", "no-store", &request("bytes=10-"));
        assert_eq!(past.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(header_of(&past, header::CONTENT_RANGE), Some("bytes */10"));
        assert!(body_of(past).await.is_empty());
    }

    #[tokio::test]
    async fn file_responses_stream_the_requested_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clip.mp4");
        // Several chunks, the last one partial.
        let bytes: Vec<u8> = (0..(2 * CHUNK + 1000)).map(|i| (i % 251) as u8).collect();
        std::fs::write(&path, &bytes).unwrap();
        let get = |range: Option<&str>| {
            let request = range.map(request).unwrap_or_default();
            let path = path.clone();
            async move {
                file_response(path, "video/mp4", "no-store", &request)
                    .await
                    .unwrap()
            }
        };

        let whole = get(None).await;
        assert_eq!(whole.status(), StatusCode::OK);
        let len = bytes.len().to_string();
        assert_eq!(
            header_of(&whole, header::CONTENT_LENGTH),
            Some(len.as_str())
        );
        assert_eq!(header_of(&whole, header::ACCEPT_RANGES), Some("bytes"));
        assert_eq!(body_of(whole).await, bytes);

        let start = CHUNK - 3;
        let part = get(Some(&format!("bytes={start}-{}", start + 9))).await;
        assert_eq!(part.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            header_of(&part, header::CONTENT_RANGE),
            Some(format!("bytes {start}-{}/{len}", start + 9).as_str())
        );
        assert_eq!(header_of(&part, header::CONTENT_LENGTH), Some("10"));
        assert_eq!(body_of(part).await, bytes[start..start + 10]);

        let tail = get(Some("bytes=-1500")).await;
        assert_eq!(body_of(tail).await, bytes[bytes.len() - 1500..]);
        let open = get(Some("bytes=0-")).await;
        assert_eq!(open.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(body_of(open).await, bytes);
        let past = get(Some(&format!("bytes={len}-"))).await;
        assert_eq!(past.status(), StatusCode::RANGE_NOT_SATISFIABLE);

        let missing = file_response(
            dir.path().join("gone.mp4"),
            "video/mp4",
            "no-store",
            &HeaderMap::new(),
        )
        .await;
        assert_eq!(missing.unwrap_err().kind(), io::ErrorKind::NotFound);
        let directory = file_response(
            dir.path().to_path_buf(),
            "video/mp4",
            "no-store",
            &HeaderMap::new(),
        )
        .await;
        assert_eq!(directory.unwrap_err().kind(), io::ErrorKind::NotFound);
    }
}
