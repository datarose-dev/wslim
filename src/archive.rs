//! Compression, integrity checking and extraction for supported TAR archives.
//! Estimated output sizes are calculated from bytes already processed, without
//! sampling or compressing the input a second time.

use crate::{cli::CompressionFormat, progress::Progress, Result};
use flate2::{read::MultiGzDecoder, write::GzEncoder, Compression};
use indicatif::{HumanBytes, ProgressBar};
use lzma_rust2::{XzOptions, XzReader, XzWriter};
use std::{
    fs::File,
    io::{self, BufReader, BufWriter, Read, Write},
    path::Path,
    sync::{atomic::{AtomicU64, Ordering}, Arc},
    time::{Duration, Instant},
};

pub(crate) fn compress(
    input: &Path,
    output: &Path,
    format: CompressionFormat,
    level: u8,
    progress: Progress,
) -> Result<()> {
    let source = File::open(input)?;
    let input_size = source.metadata()?.len();
    let label = format!("Compressing {} (level {level})", format.label());
    let meter = progress.bytes(input_size, &label);
    let started = Instant::now();
    let written = Arc::new(AtomicU64::new(0));
    let mut reader = EstimatingReader::new(
        meter.wrap_read(BufReader::new(source)),
        meter.clone(),
        input_size,
        Arc::clone(&written),
        progress.enabled(),
        format,
    );
    let target = CountingWriter::new(BufWriter::new(File::create(output)?), written);

    let result: Result<()> = (|| {
        match format {
            CompressionFormat::Xz => {
                let mut writer = XzWriter::new(target, XzOptions::with_preset(level.into()))?;
                io::copy(&mut reader, &mut writer)?;
                flush_output(writer.finish()?)
            }
            CompressionFormat::Gz => {
                let mut writer = GzEncoder::new(target, Compression::new(level.into()));
                io::copy(&mut reader, &mut writer)?;
                flush_output(writer.finish()?)
            }
        }
    })();

    if result.is_ok() {
        progress.complete(&meter, &label, started.elapsed());
    } else {
        progress.fail(&meter, &label, started.elapsed());
    }
    result
}

fn flush_output(mut writer: CountingWriter<BufWriter<File>>) -> Result<()> {
    writer.flush()?;
    writer.inner.get_ref().sync_all()?;
    Ok(())
}

pub(crate) fn verify(path: &Path, format: CompressionFormat) -> Result<()> {
    let source = BufReader::new(File::open(path)?);
    match format {
        CompressionFormat::Xz => { io::copy(&mut XzReader::new(source, false), &mut io::sink())?; }
        CompressionFormat::Gz => { io::copy(&mut MultiGzDecoder::new(source), &mut io::sink())?; }
    }
    Ok(())
}

pub(crate) fn decompress(input: &Path, output: &Path, progress: Progress) -> Result<()> {
    let format = CompressionFormat::from_archive_path(input)?;
    let source = File::open(input)?;
    let label = format!("Decompressing {}", format.label());
    let meter = progress.bytes(source.metadata()?.len(), &label);
    let started = Instant::now();

    let result: Result<()> = (|| {
        let reader = meter.wrap_read(BufReader::new(source));
        let mut target = BufWriter::new(File::create(output)?);
        match format {
            CompressionFormat::Xz => { io::copy(&mut XzReader::new(reader, false), &mut target)?; }
            CompressionFormat::Gz => { io::copy(&mut MultiGzDecoder::new(reader), &mut target)?; }
        }
        target.flush()?;
        target.get_ref().sync_all()?;
        Ok(())
    })();

    if result.is_ok() {
        progress.complete(&meter, &label, started.elapsed());
    } else {
        progress.fail(&meter, &label, started.elapsed());
    }
    result
}

/// Returns a projection, not a guaranteed final archive size.
///
/// Input and output must describe the same compression stream. The minimum
/// progress threshold and the encoder's internal buffering are handled by
/// the caller; otherwise early estimates can be wildly misleading.
fn estimate_final_size(total: u64, processed: u64, compressed: u64) -> Option<u64> {
    if total == 0 || processed == 0 || compressed == 0 { return None; }
    let estimate = (total as f64 * compressed as f64 / processed as f64).ceil();
    if !estimate.is_finite() || estimate >= u64::MAX as f64 { return None; }
    Some((estimate as u64).max(compressed))
}

struct CountingWriter<W> {
    inner: W,
    written: Arc<AtomicU64>,
}

impl<W> CountingWriter<W> {
    fn new(inner: W, written: Arc<AtomicU64>) -> Self { Self { inner, written } }
}

impl<W: Write> Write for CountingWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let count = self.inner.write(bytes)?;
        self.written.fetch_add(count as u64, Ordering::Relaxed);
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> { self.inner.flush() }
}

struct EstimatingReader<R> {
    inner: R,
    bar: ProgressBar,
    total: u64,
    processed: u64,
    written: Arc<AtomicU64>,
    enabled: bool,
    format: CompressionFormat,
    last_check_bytes: u64,
    last_update: Instant,
}

impl<R> EstimatingReader<R> {
    fn new(
        inner: R,
        bar: ProgressBar,
        total: u64,
        written: Arc<AtomicU64>,
        enabled: bool,
        format: CompressionFormat,
    ) -> Self {
        Self {
            inner, bar, total, written, enabled, format, processed: 0,
            last_check_bytes: 0, last_update: Instant::now(),
        }
    }
}

impl<R: Read> Read for EstimatingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let count = self.inner.read(buf)?;
        self.processed += count as u64;

        // Limit updates to avoid making progress rendering part of the hot path.
        // Small archives do not need predictions: they typically finish quickly.
        if self.enabled
            && self.processed >= self.total / 5
            && self.processed >= 16 * 1024 * 1024
            && self.processed.saturating_sub(self.last_check_bytes) >= 8 * 1024 * 1024
            && self.last_update.elapsed() >= Duration::from_secs(1)
        {
            self.last_check_bytes = self.processed;
            self.last_update = Instant::now();
            let produced = self.written.load(Ordering::Relaxed);
            if produced >= 64 * 1024 {
                if let Some(size) = estimate_final_size(self.total, self.processed, produced) {
                    self.bar.set_message(format!("Compressing {} (output {}, estimated final ~{})", self.format.label(), HumanBytes(produced), HumanBytes(size)));
                }
            }
        }
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_scales_observed_ratio() {
        assert_eq!(estimate_final_size(1_000, 250, 100), Some(400));
        assert_eq!(estimate_final_size(1_000, 0, 1), None);
        assert_eq!(estimate_final_size(0, 1, 1), None);
        assert_eq!(estimate_final_size(1_000, 500, 0), None);
    }

    #[test]
    fn both_formats_round_trip() {
        let dir = std::env::temp_dir().join(format!("wslim-compression-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("input.tar");
        std::fs::write(&source, vec![b'a'; 256 * 1024]).unwrap();
        for format in [CompressionFormat::Xz, CompressionFormat::Gz] {
            let archive = dir.join(format!("output.tar.{}", format.extension()));
            let result = dir.join("restored.tar");
            compress(&source, &archive, format, 1, Progress::new(true)).unwrap();
            verify(&archive, format).unwrap();
            decompress(&archive, &result, Progress::new(true)).unwrap();
            assert_eq!(std::fs::read(&source).unwrap(), std::fs::read(&result).unwrap());
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
