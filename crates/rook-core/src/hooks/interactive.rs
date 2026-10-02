//! Optional NDJSON hook protocol. Command waits have deadlines; asking a person
//! uses the existing input channel's patience and cancellation lifetime.
use super::*;
use crate::extension_ui::{Batch, Source, forms::Form};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

struct Drain(tokio::task::JoinHandle<(String, bool, u64)>);
impl Drop for Drain {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Frame<'a> {
    #[serde(borrow)]
    ui: Option<&'a serde_json::value::RawValue>,
    #[serde(borrow)]
    form: Option<&'a serde_json::value::RawValue>,
    #[serde(borrow)]
    reply: Option<&'a serde_json::value::RawValue>,
}

struct Waiting<'a, F: FnMut(Batch)> {
    form: &'a Form,
    source: &'a Source,
    on_ui: &'a mut F,
    finished: bool,
}
impl<F: FnMut(Batch)> Drop for Waiting<'_, F> {
    fn drop(&mut self) {
        if !self.finished {
            (self.on_ui)(self.form.report(self.source, "interrupted"));
        }
    }
}

#[derive(Serialize)]
struct FormAnswer<'a> {
    form_answer: &'a crate::extension_ui::forms::Reply,
}

pub(super) async fn invoke(
    config: &HookConfig,
    payload: &serde_json::Value,
    ordinal: usize,
    settings: &crate::extension_ui::Settings,
    asker: Option<&dyn rook_tools::ask::Asker>,
    on_ui: &mut impl FnMut(Batch),
) -> std::io::Result<HookReply> {
    if !config.ui {
        return Err(std::io::Error::other("ui_stream requires ui = true"));
    }
    let input = crate::extension_ui::encoded(payload, 8 * 1024 * 1024)
        .map_err(|_| std::io::Error::other("hook input exceeds 8 MiB"))?;
    let mut command = shell(&config.command);
    rook_contain::on_its_own(command.as_std_mut());
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let _group = HookGroup(rook_contain::Group::holding(child.id()));
    let mut stdin = child.stdin.take().ok_or_else(|| std::io::Error::other("missing hook stdin"))?;
    let mut stdout =
        BufReader::new(child.stdout.take().ok_or_else(|| std::io::Error::other("missing hook stdout"))?);
    let mut err = child.stderr.take();
    let mut drain = Drain(tokio::spawn(async move { bounded(&mut err).await }));
    let patience = Duration::from_secs(config.timeout_secs);
    send(&mut stdin, &input, patience).await?;
    let source = Source::hook(config, ordinal);
    let mut consumed = 0usize;
    let mut finished = None;
    for _ in 0..settings.max_entries {
        let line = line(&mut stdout, settings.max_update_bytes, &mut consumed, patience).await?;
        let frame: Frame<'_> = serde_json::from_slice(&line)?;
        if usize::from(frame.ui.is_some())
            + usize::from(frame.form.is_some())
            + usize::from(frame.reply.is_some())
            != 1
        {
            return Err(std::io::Error::other("hook frame requires exactly one of ui, form, reply"));
        }
        if let Some(raw) = frame.ui {
            let batch = Batch::parse(raw.get(), source.clone(), settings).map_err(std::io::Error::other)?;
            on_ui(batch);
        } else if let Some(raw) = frame.form {
            let form = Form::parse(raw.get(), settings)?;
            on_ui(form.report(&source, "waiting for an answer"));
            let mut waiting = Waiting { form: &form, source: &source, on_ui, finished: false };
            let answer = form.ask(&source, settings, asker).await;
            (waiting.on_ui)(form.report(&source, answer.status));
            waiting.finished = true;
            drop(waiting);
            let bytes = crate::extension_ui::encoded(
                &FormAnswer { form_answer: &answer },
                settings.max_update_bytes.saturating_add(128),
            )?;
            send(&mut stdin, &bytes, patience).await?;
        } else if let Some(raw) = frame.reply {
            finished = Some(serde_json::from_str::<HookReply>(raw.get())?);
            break;
        }
    }
    let reply =
        finished.ok_or_else(|| std::io::Error::other("hook stream frame limit reached before reply"))?;
    deadline(patience, stdin.shutdown()).await?;
    drop(stdin);
    let mut remainder = Some(stdout);
    let (tail, status, stderr) =
        deadline(patience, async { Ok(tokio::join!(bounded(&mut remainder), child.wait(), &mut drain.0)) })
            .await?;
    let (tail, truncated, tail_bytes) = tail;
    if truncated || !tail.is_empty() || (consumed as u64).saturating_add(tail_bytes) > MOST_REPLY_BYTES as u64
    {
        return Err(std::io::Error::other("unexpected or oversized output after hook reply"));
    }
    let status = status?;
    if !status.success() {
        let (stderr, _, _) = stderr.map_err(std::io::Error::other)?;
        return Err(std::io::Error::other(format!("hook exit {}: {stderr}", status.code().unwrap_or(-1))));
    }
    Ok(reply)
}

async fn deadline<T>(
    patience: Duration,
    future: impl std::future::Future<Output = std::io::Result<T>>,
) -> std::io::Result<T> {
    tokio::time::timeout(patience, future)
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "hook command wait timed out"))?
}

async fn line(
    reader: &mut (impl tokio::io::AsyncBufRead + Unpin),
    limit: usize,
    consumed: &mut usize,
    patience: Duration,
) -> std::io::Result<Vec<u8>> {
    let mut kept = Vec::new();
    loop {
        let chunk = deadline(patience, reader.fill_buf()).await?;
        if chunk.is_empty() {
            return Err(std::io::Error::other("hook ended before a complete frame"));
        }
        let end = chunk.iter().position(|b| *b == b'\n');
        let n = end.map_or(chunk.len(), |at| at + 1);
        if n > limit.saturating_sub(kept.len()) || n > MOST_REPLY_BYTES.saturating_sub(*consumed) {
            return Err(std::io::Error::other("hook stream exceeds line or total byte limit"));
        }
        kept.extend_from_slice(&chunk[..n]);
        *consumed += n;
        reader.consume(n);
        if end.is_some() {
            return Ok(kept);
        }
    }
}

async fn send(
    writer: &mut (impl tokio::io::AsyncWrite + Unpin),
    bytes: &[u8],
    patience: Duration,
) -> std::io::Result<()> {
    for chunk in bytes.chunks(4096) {
        deadline(patience, writer.write_all(chunk)).await?;
    }
    deadline(patience, async {
        writer.write_all(b"\n").await?;
        writer.flush().await
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn line_and_total_caps_refuse_the_next_copy() {
        let mut reader = BufReader::new(&b"123456\n"[..]);
        let mut consumed = 0;
        assert!(line(&mut reader, 3, &mut consumed, Duration::from_secs(1)).await.is_err());
        assert_eq!(consumed, 0);
        let mut reader = BufReader::new(&b"{}\n"[..]);
        let mut consumed = MOST_REPLY_BYTES - 2;
        assert!(line(&mut reader, 4096, &mut consumed, Duration::from_secs(1)).await.is_err());
        assert_eq!(consumed, MOST_REPLY_BYTES - 2);
    }
    #[tokio::test(start_paused = true)]
    async fn arriving_bytes_reset_patience_and_silence_times_out() {
        let (reader, mut writer) = tokio::io::duplex(16);
        let feed = tokio::spawn(async move {
            for byte in b"{}\n" {
                tokio::time::sleep(Duration::from_millis(50)).await;
                writer.write_all(&[*byte]).await.unwrap();
            }
        });
        let mut consumed = 0;
        let started = tokio::time::Instant::now();
        assert_eq!(
            line(&mut BufReader::new(reader), 32, &mut consumed, Duration::from_millis(100)).await.unwrap(),
            b"{}\n"
        );
        assert!(started.elapsed() >= Duration::from_millis(150));
        feed.await.unwrap();
        let (reader, _writer) = tokio::io::duplex(16);
        assert_eq!(
            line(&mut BufReader::new(reader), 32, &mut consumed, Duration::from_millis(100))
                .await
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::TimedOut
        );
    }
}
