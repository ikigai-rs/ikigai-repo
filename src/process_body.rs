//! Pull-driven process bodies. Only a requested chunk is read; dropping the
//! reader closes its request channel and kills and reaps the direct child.
use super::*;
use async_trait::async_trait;
use futures::{channel::oneshot, StreamExt};
use ikigai_core::{Body, Bytes, Endpoint, FailureClass};
use std::process::Child;
use std::sync::{mpsc, Mutex};

pub(super) enum Kind {
    Log,
    Pr {
        sub: &'static [&'static str],
        enabled: bool,
    },
}

pub(super) struct StreamingEndpoint {
    pub inner: FnEndpoint,
    pub roots: Roots,
    pub kind: Kind,
}

#[async_trait]
impl Endpoint for StreamingEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        self.inner.invoke(inv).await
    }

    async fn invoke_body(&self, inv: &Invocation<'_>) -> Result<Representation<Body>> {
        let (tool, args, json) = match self.kind {
            Kind::Log => {
                let (args, json) = log_args(inv)?;
                ("git", args, json)
            }
            Kind::Pr { sub, enabled: true } => {
                let (args, _) = gh_pr_args(inv, sub, None)?;
                ("gh", args, false)
            }
            _ => return self.inner.invoke_body(inv).await,
        };
        let mut command =
            prepare_command(inv, tool, &args, inv.inline_str("dir").ok(), &self.roots)?;
        let limit = inv
            .memory_budget()
            .map_or(ikigai_core::DEFAULT_STREAM_MEMORY, |b| b.remaining());
        let raw = spawn_body(&mut command, tool, limit)?;
        let body = if json { log_json(raw, limit) } else { raw };
        Ok(Representation::from_body(
            if json {
                ReprType::new("application/json")
            } else {
                ReprType::new("text/plain").with_param("charset", "utf-8")
            },
            body,
        ))
    }

    fn describe(&self) -> Description {
        // Endpoint errors are Unknown, and bounds are explicit. Denied and
        // Permanent are implied by core's action contract.
        self.inner
            .describe()
            .fails(FailureClass::Unknown)
            .fails(FailureClass::OverBudget)
    }

    fn name(&self) -> &str {
        self.inner.name()
    }
}

struct ChildGuard(Arc<Mutex<Child>>);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let mut child = self.0.lock().unwrap();
        let _ = child.kill();
        // Always reap, including after stderr EOF. EOF is not an exit status.
        let _ = child.wait();
    }
}

/// Keep the executor free while the process finishes after closing its pipes.
/// The short status checks release the mutex so cancellation can still kill
/// and reap the child while this future is pending.
async fn wait_for_exit(child: Arc<Mutex<Child>>) -> std::io::Result<std::process::ExitStatus> {
    let (tx, rx) = oneshot::channel();
    std::thread::spawn(move || loop {
        let result = {
            let mut child = child.lock().unwrap();
            match child.try_wait() {
                Ok(None) => None,
                Ok(Some(_)) => Some(child.wait()),
                Err(error) => Some(Err(error)),
            }
        };
        if let Some(result) = result {
            let _ = tx.send(result);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    });
    rx.await
        .map_err(|_| std::io::Error::other("exec: exit waiter stopped"))?
}

const CHUNK: usize = 8192;
type ReadReply = oneshot::Sender<std::io::Result<Vec<u8>>>;

fn read_requests(mut stdout: impl Read, requests: mpsc::Receiver<ReadReply>) {
    for reply in requests {
        let mut buffer = vec![0; CHUNK];
        let result = loop {
            match stdout.read(&mut buffer) {
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                result => break result,
            }
        }
        .map(|n| {
            buffer.truncate(n);
            buffer
        });
        let ended = result.as_ref().map_or(true, Vec::is_empty);
        if reply.send(result).is_err() || ended {
            break;
        }
    }
}

struct Reader {
    child: ChildGuard,
    requests: mpsc::Sender<ReadReply>,
    stderr: Option<oneshot::Receiver<Result<Vec<u8>>>>,
    tool: &'static str,
    carry: Vec<u8>,
    passed: u64,
    limit: u64,
}

/// std validates sequence boundaries and performs lossy decoding, exactly as
/// the bytes path does. Hold only the incomplete UTF-8 tail between reads.
fn lossy_chunk(mut bytes: Vec<u8>, eof: bool) -> (Bytes, Vec<u8>) {
    let mut at = 0;
    let split = loop {
        match std::str::from_utf8(&bytes[at..]) {
            Ok(_) => break bytes.len(),
            Err(e) => match e.error_len() {
                Some(n) => at += e.valid_up_to() + n,
                None if !eof => break at + e.valid_up_to(),
                None => break bytes.len(),
            },
        }
    };
    let carry = bytes.split_off(split);
    (
        String::from_utf8_lossy(&bytes)
            .into_owned()
            .into_bytes()
            .into(),
        carry,
    )
}

fn over_budget() -> Error {
    Error::OverBudget {
        detail: "exec: process body exceeds the run's byte budget".into(),
    }
}

fn spawn_body(command: &mut Command, tool: &'static str, limit: u64) -> Result<Body> {
    Ok(reader_body(spawn_reader(command, tool, limit)?))
}

fn spawn_reader(command: &mut Command, tool: &'static str, limit: u64) -> Result<Reader> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Error::Endpoint(format!("exec: could not run `{tool}`: {e}")))?;
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let child = ChildGuard(Arc::new(Mutex::new(child)));
    let killer = child.0.clone();
    let (err_tx, err_rx) = oneshot::channel();
    std::thread::spawn(move || {
        let result = read_output(stderr, tool, "stderr");
        if result.is_err() {
            let _ = killer.lock().unwrap().kill();
        }
        let _ = err_tx.send(result);
    });
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || read_requests(stdout, rx));
    let reader = Reader {
        child,
        requests: tx,
        stderr: Some(err_rx),
        tool,
        carry: Vec::new(),
        passed: 0,
        limit,
    };
    Ok(reader)
}

fn reader_body(reader: Reader) -> Body {
    Body::once(futures::stream::unfold(Some(reader), |state| async move {
        let mut reader = state?;
        loop {
            let (tx, rx) = oneshot::channel();
            if reader.requests.send(tx).is_err() {
                return Some((
                    Err(Error::Endpoint("exec: stdout reader stopped".into())),
                    None,
                ));
            }
            let raw = match rx.await {
                Ok(Ok(raw)) => raw,
                Ok(Err(e)) => {
                    return Some((
                        Err(Error::Endpoint(format!("exec: stdout read: {e}"))),
                        None,
                    ))
                }
                Err(_) => {
                    return Some((
                        Err(Error::Endpoint("exec: stdout reader stopped".into())),
                        None,
                    ))
                }
            };
            let eof = raw.is_empty();
            let mut joined = std::mem::take(&mut reader.carry);
            joined.extend_from_slice(&raw);
            let (chunk, carry) = lossy_chunk(joined, eof);
            reader.carry = carry;
            reader.passed += chunk.len() as u64;
            if reader.passed > reader.limit {
                return Some((Err(over_budget()), None));
            }
            if eof {
                let stderr = match reader.stderr.take().unwrap().await {
                    Ok(Ok(stderr)) => stderr,
                    Ok(Err(e)) => return Some((Err(e), None)),
                    Err(_) => {
                        return Some((
                            Err(Error::Endpoint("exec: stderr reader stopped".into())),
                            None,
                        ))
                    }
                };
                let status = wait_for_exit(reader.child.0.clone()).await;
                match status {
                    Ok(status) if status.success() => {
                        return if chunk.is_empty() {
                            None
                        } else {
                            Some((Ok(chunk), None))
                        };
                    }
                    Ok(status) => {
                        return Some((
                            Err(Error::Endpoint(format!(
                                "exec: `{}` exited {}: {}",
                                reader.tool,
                                status.code().unwrap_or(-1),
                                String::from_utf8_lossy(&stderr).trim()
                            ))),
                            None,
                        ))
                    }
                    Err(e) => {
                        return Some((
                            Err(Error::Endpoint(format!(
                                "exec: could not wait for `{}`: {e}",
                                reader.tool
                            ))),
                            None,
                        ))
                    }
                }
            }
            if !chunk.is_empty() {
                return Some((Ok(chunk), Some(reader)));
            }
        }
    }))
}

fn log_json(raw: Body, limit: u64) -> Body {
    struct Lines {
        raw: ikigai_core::ByteStream,
        pending: Vec<u8>,
        first: bool,
        ended: bool,
        passed: u64,
    }
    let state = Lines {
        raw: raw.into_stream(),
        pending: Vec::new(),
        first: true,
        ended: false,
        passed: 0,
    };
    Body::once(futures::stream::unfold(
        Some(state),
        move |state| async move {
            let mut s = state?;
            loop {
                let line = if let Some(at) = s.pending.iter().position(|b| *b == b'\n') {
                    let rest = s.pending.split_off(at + 1);
                    let mut line = std::mem::replace(&mut s.pending, rest);
                    line.pop();
                    if line.last() == Some(&b'\r') {
                        line.pop();
                    }
                    Some(line)
                } else if s.ended && !s.pending.is_empty() {
                    Some(std::mem::take(&mut s.pending))
                } else {
                    None
                };
                if let Some(line) = line {
                    let line = std::str::from_utf8(&line).expect("lossy UTF-8 stream");
                    if let Some(entry) = log_entry(line) {
                        let chunk = format!("{}{entry}", if s.first { "[" } else { "," });
                        s.first = false;
                        s.passed += chunk.len() as u64;
                        if s.passed > limit {
                            return Some((Err(over_budget()), None));
                        }
                        return Some((Ok(Bytes::from(chunk)), Some(s)));
                    }
                } else if s.ended {
                    let end = if s.first { "[]" } else { "]" };
                    if s.passed + end.len() as u64 > limit {
                        return Some((Err(over_budget()), None));
                    }
                    return Some((Ok(Bytes::from_static(end.as_bytes())), None));
                } else {
                    match s.raw.next().await {
                        Some(Ok(chunk)) => {
                            if s.pending.len() as u64 + chunk.len() as u64 > limit {
                                return Some((Err(over_budget()), None));
                            }
                            s.pending.extend_from_slice(&chunk);
                        }
                        Some(Err(e)) => return Some((Err(e), None)),
                        None => s.ended = true,
                    }
                }
            }
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn stdout_reads_only_on_demand() {
        struct Counted(Arc<AtomicUsize>);
        impl Read for Counted {
            fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
                self.0.fetch_add(1, Ordering::SeqCst);
                bytes.fill(b'x');
                Ok(bytes.len())
            }
        }
        let count = Arc::new(AtomicUsize::new(0));
        let (tx, rx) = mpsc::channel();
        let input = Counted(count.clone());
        let worker = std::thread::spawn(move || read_requests(input, rx));
        assert_eq!(count.load(Ordering::SeqCst), 0);
        for expected in 1..=3 {
            let (reply, result) = oneshot::channel();
            tx.send(reply).unwrap();
            assert_eq!(block_on(result).unwrap().unwrap().len(), CHUNK);
            assert_eq!(count.load(Ordering::SeqCst), expected);
        }
        drop(tx);
        worker.join().unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn lossy_utf8_matches_whole_buffer_at_every_split() {
        for bytes in [
            b"a\xf0\x9f\xa6\x80z".as_slice(),
            b"a\xff\xe2\x82",
            b"\xf0\x80\x80\x80\xed\xa0\x80\xc2\xa2",
        ] {
            for split in 0..=bytes.len() {
                let (first, mut carry) = lossy_chunk(bytes[..split].to_vec(), false);
                carry.extend_from_slice(&bytes[split..]);
                let (last, carry) = lossy_chunk(carry, true);
                assert!(carry.is_empty());
                assert_eq!(
                    [first.as_ref(), last.as_ref()].concat(),
                    String::from_utf8_lossy(bytes).as_bytes()
                );
            }
        }
    }

    #[cfg(unix)]
    fn script(script: &str, limit: u64) -> Body {
        spawn_body(Command::new("sh").args(["-c", script]), "fixture", limit).unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn early_drop_kills_and_reaps_after_stderr_eof() {
        use std::os::unix::process::ExitStatusExt;
        let mut reader = spawn_reader(
            Command::new("sh").args(["-c", "exec 2>&-; printf ready; exec sleep 30"]),
            "fixture",
            u64::MAX,
        )
        .unwrap();
        // Establish stderr EOF while the direct child is still alive. EOF
        // alone must never be mistaken for process completion.
        let stderr = block_on(reader.stderr.take().unwrap()).unwrap().unwrap();
        assert!(stderr.is_empty());
        let child = reader.child.0.clone();
        let mut stream = reader_body(reader).into_stream();
        assert_eq!(block_on(stream.next()).unwrap().unwrap(), b"ready"[..]);
        let started = std::time::Instant::now();
        drop(stream);
        // wait(), deliberately never try_wait(): prove it was killed and
        // reaped, rather than accepting an instantaneous status observation.
        let status = child.lock().unwrap().wait().unwrap();
        assert_eq!(status.signal(), Some(9), "{status}");
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    #[cfg(unix)]
    #[test]
    fn drop_during_pending_read_kills_and_reaps() {
        use std::os::unix::process::ExitStatusExt;
        let reader = spawn_reader(Command::new("sleep").arg("30"), "fixture", 100).unwrap();
        let child = reader.child.0.clone();
        let mut stream = reader_body(reader).into_stream();
        assert!(futures::FutureExt::now_or_never(stream.next()).is_none());
        drop(stream);
        assert_eq!(child.lock().unwrap().wait().unwrap().signal(), Some(9));
    }

    #[cfg(unix)]
    #[test]
    fn exit_wait_is_pending_and_cancellation_can_still_reap() {
        use std::os::unix::process::ExitStatusExt;
        let reader = spawn_reader(Command::new("sleep").arg("30"), "fixture", 100).unwrap();
        let child = reader.child.0.clone();
        assert!(futures::FutureExt::now_or_never(wait_for_exit(child.clone())).is_none());
        drop(reader);
        assert_eq!(child.lock().unwrap().wait().unwrap().signal(), Some(9));
    }

    #[cfg(unix)]
    #[test]
    fn late_nonzero_exit_is_an_error_item_with_stderr() {
        let mut stream = script("printf partial; printf diagnostic >&2; exit 7", 100).into_stream();
        let mut output = Vec::new();
        loop {
            match block_on(stream.next()).expect("must end with an error item") {
                Ok(bytes) => output.extend_from_slice(&bytes),
                Err(Error::Endpoint(message)) => {
                    assert!(message.contains("exited 7"), "{message}");
                    assert!(message.contains("diagnostic"), "{message}");
                    break;
                }
                Err(e) => panic!("unexpected {e}"),
            }
        }
        assert_eq!(output, b"partial");
        assert!(block_on(stream.next()).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn exact_budget_succeeds_next_byte_is_error_item() {
        assert_eq!(
            block_on(script("printf four", 4).collect(100)).unwrap(),
            b"four"[..]
        );
        let mut stream = script("printf five!", 4).into_stream();
        assert!(matches!(
            block_on(stream.next()),
            Some(Err(Error::OverBudget { .. }))
        ));
        assert!(block_on(stream.next()).is_none());
        assert!(matches!(
            block_on(script("exec yes x", 3).collect(100)),
            Err(Error::OverBudget { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn stderr_overflow_is_not_clean_eof() {
        let mut stream = script("exec yes x >&2", 100).into_stream();
        assert!(matches!(
            block_on(stream.next()),
            Some(Err(Error::OverBudget { .. }))
        ));
        assert!(block_on(stream.next()).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn large_stdout_is_consumed_in_fixed_chunks() {
        // Much larger than the bytes API's bound. No fixture or answer is
        // held whole: the child generates it and this reader counts chunks.
        let size = MAX_OUTPUT_BYTES * 4;
        let mut stream = script(&format!("head -c {size} /dev/zero"), size as u64).into_stream();
        let mut seen = 0;
        let mut chunks = 0;
        while let Some(chunk) = block_on(stream.next()) {
            let chunk = chunk.unwrap();
            assert!(chunk.len() <= CHUNK);
            seen += chunk.len();
            chunks += 1;
        }
        assert_eq!(seen, size);
        assert!(chunks >= size / CHUNK);
    }

    #[test]
    fn json_transform_matches_bytes_and_refuses_overflow_or_late_failure() {
        let record = "hash\x1fauthor\x1fdate\x1fquote \" and slash \\";
        let input = format!("{record}\n{record}");
        let expected = format!(
            "[{},{}]",
            log_entry(record).unwrap(),
            log_entry(record).unwrap()
        );
        // One byte per chunk proves record and UTF-8 chunk boundaries are
        // independent; escaping uses the same renderer as the bytes path.
        let raw = || {
            Body::once(futures::stream::iter(
                input
                    .bytes()
                    .map(|b| Ok(Bytes::from(vec![b])))
                    .collect::<Vec<_>>(),
            ))
        };
        assert_eq!(
            block_on(log_json(raw(), 1000).collect(1000)).unwrap(),
            expected.as_bytes()
        );
        assert!(matches!(
            block_on(log_json(raw(), expected.len() as u64 - 1).collect(1000)),
            Err(Error::OverBudget { .. })
        ));
        let raw = Body::once(futures::stream::iter([
            Ok(Bytes::from(input)),
            Err(Error::Endpoint("late".into())),
        ]));
        assert!(matches!(
            block_on(log_json(raw, 1000).collect(1000)),
            Err(Error::Endpoint(_))
        ));
    }
}
