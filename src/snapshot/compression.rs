//! Snapshot Compression Utilities
//!
//! High-performance compression for WASM memory snapshots.

use std::io::{Read, Write};

/// Compression algorithm selection
#[derive(Debug, Clone, Copy, Default)]
pub enum CompressionAlgo {
    /// Zstd compression (good balance of speed and ratio)
    #[default]
    Zstd,
    /// LZ4 (fastest, lower ratio)
    Lz4,
    /// LZMA (slowest, highest ratio)
    Lzma,
    /// No compression
    None,
}

/// Compression configuration
#[derive(Debug, Clone)]
pub struct CompressionConfig {
    pub algorithm: CompressionAlgo,
    pub level: i32,
    pub checksum: bool,
}

impl Default for CompressionConfig {
    fn default() -> Self {
        CompressionConfig {
            algorithm: CompressionAlgo::Zstd,
            level: 3,
            checksum: true,
        }
    }
}

/// Compression result with metadata
#[derive(Debug, Clone)]
pub struct CompressedData {
    pub data: Vec<u8>,
    pub original_size: usize,
    pub compressed_size: usize,
    pub algorithm: CompressionAlgo,
    pub level: i32,
}

impl CompressedData {
    pub fn compression_ratio(&self) -> f64 {
        if self.original_size == 0 {
            return 1.0;
        }
        self.compressed_size as f64 / self.original_size as f64
    }

    pub fn space_saved(&self) -> usize {
        self.original_size.saturating_sub(self.compressed_size)
    }
}

/// Compress data with configured algorithm
pub fn compress(data: &[u8], config: &CompressionConfig) -> Result<CompressedData, String> {
    match config.algorithm {
        CompressionAlgo::Zstd => compress_zstd(data, config.level),
        CompressionAlgo::None => Ok(CompressedData {
            data: data.to_vec(),
            original_size: data.len(),
            compressed_size: data.len(),
            algorithm: config.algorithm,
            level: config.level,
        }),
        _ => Err(format!(
            "Algorithm {:?} not yet implemented",
            config.algorithm
        )),
    }
}

/// Decompress data
pub fn decompress(
    data: &[u8],
    algorithm: CompressionAlgo,
    original_size: usize,
) -> Result<Vec<u8>, String> {
    match algorithm {
        CompressionAlgo::Zstd => decompress_zstd(data, original_size),
        CompressionAlgo::None => Ok(data.to_vec()),
        _ => Err(format!("Algorithm {:?} not yet implemented", algorithm)),
    }
}

/// Zstd compression
fn compress_zstd(data: &[u8], level: i32) -> Result<CompressedData, String> {
    let original_size = data.len();

    let mut compressed = Vec::new();

    // Use zstd streaming API
    zstd::stream::copy_encode(data, &mut compressed, level)
        .map_err(|e| format!("Failed to compress: {}", e))?;

    let compressed_size = compressed.len();

    Ok(CompressedData {
        data: compressed,
        original_size,
        compressed_size,
        algorithm: CompressionAlgo::Zstd,
        level,
    })
}

/// Zstd decompression
fn decompress_zstd(data: &[u8], _original_size: usize) -> Result<Vec<u8>, String> {
    let mut decompressed = Vec::new();

    zstd::stream::copy_decode(data, &mut decompressed)
        .map_err(|e| format!("Failed to decompress: {}", e))?;

    Ok(decompressed)
}

/// Streaming compression for large data (simplified)
pub struct StreamingCompressor {
    buffer: Vec<u8>,
    level: i32,
}

impl StreamingCompressor {
    pub fn new(level: i32) -> Result<Self, String> {
        Ok(StreamingCompressor {
            buffer: Vec::new(),
            level,
        })
    }

    pub fn write(&mut self, data: &[u8]) -> Result<(), String> {
        self.buffer.extend_from_slice(data);
        Ok(())
    }

    pub fn finish(self) -> Result<CompressedData, String> {
        let original_size = self.buffer.len();
        let mut compressed = Vec::new();
        zstd::stream::copy_encode(&self.buffer[..], &mut compressed, self.level)
            .map_err(|e| format!("Failed to compress: {}", e))?;

        let compressed_size = compressed.len();

        Ok(CompressedData {
            data: compressed,
            original_size,
            compressed_size,
            algorithm: CompressionAlgo::Zstd,
            level: self.level,
        })
    }
}

/// Memory-efficient snapshot compression
pub fn compress_snapshot_memory(memory: &[u8]) -> Result<(Vec<u8>, usize), String> {
    // For WASM memory, we can use several optimization strategies:

    // 1. Delta encoding - store only changed pages
    // 2. Zero-page detection - skip zero pages
    // 3. LZ4 for fast decompression

    let page_size = 65536; // 64KB WASM page
    if !memory.len().is_multiple_of(page_size) {
        return Err("Snapshot memory length must be a multiple of the WASM page size".to_string());
    }

    let mut result = Vec::new();
    let mut zero_pages: usize = 0;
    let mut nonzero_pages: usize = 0;

    for chunk in memory.chunks(page_size) {
        if chunk.iter().all(|&b| b == 0) {
            // Zero page - store single marker
            result.push(0x00); // Zero page marker
            zero_pages += 1;
        } else {
            // Non-zero page - store raw with marker
            result.push(0x01); // Non-zero page marker
            result.extend_from_slice(chunk);
            nonzero_pages += 1;
        }
    }

    // Add header with page counts
    let mut header = Vec::new();
    header.extend_from_slice(&zero_pages.to_le_bytes());
    header.extend_from_slice(&nonzero_pages.to_le_bytes());

    let mut final_result = header;
    final_result.extend_from_slice(&result);

    Ok((final_result, memory.len()))
}

/// Decompress snapshot memory
pub fn decompress_snapshot_memory(compressed: &[u8]) -> Result<Vec<u8>, String> {
    let page_size = 65536;

    // Read header
    if compressed.len() < 16 {
        return Err("Invalid compressed data".to_string());
    }

    let zero_pages = usize::from_le_bytes(compressed[0..8].try_into().unwrap());
    let nonzero_pages = usize::from_le_bytes(compressed[8..16].try_into().unwrap());
    let total_pages = zero_pages
        .checked_add(nonzero_pages)
        .ok_or_else(|| "Page count overflow".to_string())?;
    let expected_payload_len = total_pages
        .checked_add(
            nonzero_pages
                .checked_mul(page_size)
                .ok_or_else(|| "Compressed size overflow".to_string())?,
        )
        .ok_or_else(|| "Compressed size overflow".to_string())?;
    let expected_len = 16usize
        .checked_add(expected_payload_len)
        .ok_or_else(|| "Compressed size overflow".to_string())?;
    if compressed.len() != expected_len {
        return Err("Compressed data length does not match header".to_string());
    }
    let capacity = total_pages
        .checked_mul(page_size)
        .ok_or_else(|| "Decompressed size overflow".to_string())?;

    let mut result = Vec::with_capacity(capacity);
    let mut pos = 16;
    let mut seen_zero_pages = 0usize;
    let mut seen_nonzero_pages = 0usize;

    for _ in 0..total_pages {
        if pos >= compressed.len() {
            return Err("Unexpected end of data".to_string());
        }

        let marker = compressed[pos];
        pos += 1;

        match marker {
            0x00 => {
                seen_zero_pages += 1;
                result.extend(std::iter::repeat_n(0u8, page_size));
            }
            0x01 => {
                seen_nonzero_pages += 1;
                let end = pos
                    .checked_add(page_size)
                    .ok_or_else(|| "Compressed position overflow".to_string())?;
                if end > compressed.len() {
                    return Err("Unexpected end of non-zero page".to_string());
                }
                result.extend_from_slice(&compressed[pos..end]);
                pos = end;
            }
            _ => return Err(format!("Invalid page marker: {marker:#04x}")),
        }
    }

    if seen_zero_pages != zero_pages || seen_nonzero_pages != nonzero_pages {
        return Err("Page marker counts do not match header".to_string());
    }

    if pos != compressed.len() {
        return Err("Trailing compressed data".to_string());
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zstd_compression() {
        let data = vec![1u8; 10000];
        let config = CompressionConfig::default();

        let compressed = compress(&data, &config).unwrap();
        assert!(compressed.compressed_size < data.len());

        let decompressed = decompress(
            &compressed.data,
            compressed.algorithm,
            compressed.original_size,
        )
        .unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn test_snapshot_memory_compression() {
        let page_size = 65536;

        // Create memory with zero/non-zero/zero/non-zero pages to verify marker order.
        let mut memory = Vec::new();
        memory.extend(vec![0u8; page_size]); // Zero page
        memory.extend(vec![1u8; page_size]); // Non-zero page
        memory.extend(vec![0u8; page_size]); // Zero page
        memory.extend(vec![2u8; page_size]); // Non-zero page

        let (compressed, _) = compress_snapshot_memory(&memory).unwrap();
        assert!(compressed.len() < memory.len());

        let decompressed = decompress_snapshot_memory(&compressed).unwrap();
        assert_eq!(decompressed, memory);
    }

    #[test]
    fn test_snapshot_memory_rejects_invalid_marker() {
        let mut compressed = Vec::new();
        compressed.extend_from_slice(&1usize.to_le_bytes());
        compressed.extend_from_slice(&0usize.to_le_bytes());
        compressed.push(0x02);

        let err = decompress_snapshot_memory(&compressed).unwrap_err();
        assert!(err.contains("Invalid page marker"));
    }

    #[test]
    fn test_snapshot_memory_rejects_trailing_data() {
        let mut compressed = Vec::new();
        compressed.extend_from_slice(&1usize.to_le_bytes());
        compressed.extend_from_slice(&0usize.to_le_bytes());
        compressed.push(0x00);
        compressed.push(0x00);

        let err = decompress_snapshot_memory(&compressed).unwrap_err();
        assert_eq!(err, "Compressed data length does not match header");
    }
}
