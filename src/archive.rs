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
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
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

    let result: Result<()> = (|| match format {
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
    let source = File::open(path)?;
    validate_archive_length(format, source.metadata()?.len())?;
    let source = BufReader::new(source);
    decode_copy(source, &mut io::sink(), format)?;
    Ok(())
}

fn validate_archive_length(format: CompressionFormat, length: u64) -> io::Result<()> {
    if matches!(format, CompressionFormat::Xz) && !length.is_multiple_of(4) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "XZ archive length is not a multiple of four bytes",
        ));
    }
    Ok(())
}

fn decode_copy<R: Read, W: Write>(
    reader: R,
    writer: &mut W,
    format: CompressionFormat,
) -> io::Result<u64> {
    match format {
        CompressionFormat::Xz => io::copy(&mut XzReader::new(reader, true), writer),
        CompressionFormat::Gz => io::copy(&mut MultiGzDecoder::new(reader), writer),
    }
}

pub(crate) fn decompress(input: &Path, output: &Path, progress: Progress) -> Result<()> {
    let format = CompressionFormat::from_archive_path(input)?;
    let source = File::open(input)?;
    let input_size = source.metadata()?.len();
    let label = format!("Decompressing {}", format.label());
    let meter = progress.bytes(input_size, &label);
    let started = Instant::now();

    let result: Result<()> = (|| {
        validate_archive_length(format, input_size)?;
        let reader = meter.wrap_read(BufReader::new(source));
        let mut target = BufWriter::new(File::create(output)?);
        decode_copy(reader, &mut target, format)?;
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
    if total == 0 || processed == 0 || compressed == 0 {
        return None;
    }
    let estimate = (total as f64 * compressed as f64 / processed as f64).ceil();
    if !estimate.is_finite() || estimate >= u64::MAX as f64 {
        return None;
    }
    Some((estimate as u64).max(compressed))
}

struct CountingWriter<W> {
    inner: W,
    written: Arc<AtomicU64>,
}

impl<W> CountingWriter<W> {
    fn new(inner: W, written: Arc<AtomicU64>) -> Self {
        Self { inner, written }
    }
}

impl<W: Write> Write for CountingWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let count = self.inner.write(bytes)?;
        self.written.fetch_add(count as u64, Ordering::Relaxed);
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
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
            inner,
            bar,
            total,
            written,
            enabled,
            format,
            processed: 0,
            last_check_bytes: 0,
            last_update: Instant::now(),
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
                    self.bar.set_message(format!(
                        "Compressing {} (output {}, estimated final ~{})",
                        self.format.label(),
                        HumanBytes(produced),
                        HumanBytes(size)
                    ));
                }
            }
        }
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDir(std::path::PathBuf);

    impl TestDir {
        fn new() -> Self {
            static NEXT_ID: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "wslim-compression-test-{}-{}",
                std::process::id(),
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn make_archive(dir: &Path, name: &str, contents: &[u8], format: CompressionFormat) -> Vec<u8> {
        let source = dir.join(format!("{name}.input"));
        let archive = dir.join(format!("{name}.tar.{}", format.extension()));
        std::fs::write(&source, contents).unwrap();
        compress(&source, &archive, format, 1, Progress::new(false)).unwrap();
        std::fs::read(archive).unwrap()
    }

    fn archive_path(
        dir: &Path,
        name: &str,
        format: CompressionFormat,
        bytes: &[u8],
    ) -> std::path::PathBuf {
        let path = dir.join(format!("{name}.tar.{}", format.extension()));
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn assert_rejected(dir: &Path, name: &str, format: CompressionFormat, bytes: &[u8]) {
        let archive = archive_path(dir, name, format, bytes);
        assert!(verify(&archive, format).is_err(), "verify accepted {name}");
        let output = dir.join(format!("{name}.output"));
        assert!(
            decompress(&archive, &output, Progress::new(false)).is_err(),
            "decompress accepted {name}"
        );
    }

    #[test]
    fn estimate_scales_observed_ratio() {
        assert_eq!(estimate_final_size(1_000, 250, 100), Some(400));
        assert_eq!(estimate_final_size(1_000, 0, 1), None);
        assert_eq!(estimate_final_size(0, 1, 1), None);
        assert_eq!(estimate_final_size(1_000, 500, 0), None);
    }

    #[test]
    fn both_formats_round_trip() {
        let dir = TestDir::new();
        let source = dir.path().join("input.tar");
        std::fs::write(&source, vec![b'a'; 256 * 1024]).unwrap();
        for format in [CompressionFormat::Xz, CompressionFormat::Gz] {
            let archive = dir
                .path()
                .join(format!("output.tar.{}", format.extension()));
            let result = dir.path().join("restored.tar");
            compress(&source, &archive, format, 1, Progress::new(true)).unwrap();
            verify(&archive, format).unwrap();
            decompress(&archive, &result, Progress::new(true)).unwrap();
            assert_eq!(
                std::fs::read(&source).unwrap(),
                std::fs::read(&result).unwrap()
            );
        }
    }

    #[test]
    fn concatenated_xz_streams_decode_and_validate_all_streams() {
        let dir = TestDir::new();
        let first_contents = b"first XZ stream";
        let second_contents = b"second XZ stream";
        let first = make_archive(dir.path(), "first", first_contents, CompressionFormat::Xz);
        let second = make_archive(dir.path(), "second", second_contents, CompressionFormat::Xz);

        let mut combined = first.clone();
        combined.extend_from_slice(&[0; 4]);
        combined.extend_from_slice(&second);
        combined.extend_from_slice(&[0; 4]);
        let archive = archive_path(dir.path(), "combined", CompressionFormat::Xz, &combined);
        verify(&archive, CompressionFormat::Xz).unwrap();
        let output = dir.path().join("combined.output");
        decompress(&archive, &output, Progress::new(false)).unwrap();
        let expected = [first_contents.as_slice(), second_contents.as_slice()].concat();
        assert_eq!(std::fs::read(output).unwrap(), expected);

        let mut damaged_second = second;
        *damaged_second.last_mut().unwrap() ^= 1;
        let mut damaged = first;
        damaged.extend_from_slice(&[0; 4]);
        damaged.extend_from_slice(&damaged_second);
        assert_rejected(
            dir.path(),
            "damaged-second",
            CompressionFormat::Xz,
            &damaged,
        );

        let mut trailing_garbage = combined;
        trailing_garbage.extend_from_slice(b"trailing garbage");
        assert_rejected(
            dir.path(),
            "trailing-garbage",
            CompressionFormat::Xz,
            &trailing_garbage,
        );
    }

    #[test]
    fn xz_padding_must_be_a_multiple_of_four_bytes() {
        let dir = TestDir::new();
        let contents = b"XZ stream with padding";
        let stream = make_archive(dir.path(), "stream", contents, CompressionFormat::Xz);

        for padding_len in [0, 4, 8] {
            let mut bytes = stream.clone();
            bytes.resize(bytes.len() + padding_len, 0);
            let archive = archive_path(
                dir.path(),
                &format!("valid-padding-{padding_len}"),
                CompressionFormat::Xz,
                &bytes,
            );
            verify(&archive, CompressionFormat::Xz).unwrap();
            let output = dir
                .path()
                .join(format!("valid-padding-{padding_len}.output"));
            decompress(&archive, &output, Progress::new(false)).unwrap();
            assert_eq!(std::fs::read(output).unwrap(), contents);
        }

        for padding_len in [1, 2, 3] {
            let mut bytes = stream.clone();
            bytes.resize(bytes.len() + padding_len, 0);
            let archive = archive_path(
                dir.path(),
                &format!("invalid-padding-{padding_len}"),
                CompressionFormat::Xz,
                &bytes,
            );
            assert!(verify(&archive, CompressionFormat::Xz).is_err());
            let output = dir
                .path()
                .join(format!("invalid-padding-{padding_len}.output"));
            assert!(decompress(&archive, &output, Progress::new(false)).is_err());
            assert!(!output.exists(), "invalid XZ padding created output");
        }
    }

    #[test]
    fn concatenated_gzip_streams_decode_and_validate_all_streams() {
        let dir = TestDir::new();
        let first_contents = b"first gzip member";
        let second_contents = b"second gzip member";
        let first = make_archive(dir.path(), "first", first_contents, CompressionFormat::Gz);
        let second = make_archive(dir.path(), "second", second_contents, CompressionFormat::Gz);
        let mut combined = first.clone();
        combined.extend_from_slice(&second);
        let archive = archive_path(dir.path(), "combined", CompressionFormat::Gz, &combined);

        verify(&archive, CompressionFormat::Gz).unwrap();
        let output = dir.path().join("combined.output");
        decompress(&archive, &output, Progress::new(false)).unwrap();
        let expected = [first_contents.as_slice(), second_contents.as_slice()].concat();
        assert_eq!(std::fs::read(output).unwrap(), expected);

        let mut damaged_second = second;
        *damaged_second.last_mut().unwrap() ^= 1;
        let mut damaged = first;
        damaged.extend_from_slice(&damaged_second);
        assert_rejected(
            dir.path(),
            "damaged-second",
            CompressionFormat::Gz,
            &damaged,
        );
    }
}
