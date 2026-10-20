//! Agrupamento do áudio de entrada em chunks de ~100 ms e codificação base64.

use base64::Engine;
use bytes::{Bytes, BytesMut};

/// PCM16 mono 24 kHz: 24000 amostras/s × 2 bytes = 48 bytes/ms.
pub const BYTES_PER_MS: usize = 48;
/// ~100 ms de áudio.
pub const CHUNK_BYTES: usize = 100 * BYTES_PER_MS;

/// Acumula bytes PCM16 e devolve chunks de tamanho fixo.
#[derive(Debug)]
pub struct Chunker {
    buf: BytesMut,
    chunk: usize,
}

impl Default for Chunker {
    fn default() -> Self {
        Self::new(CHUNK_BYTES)
    }
}

impl Chunker {
    pub fn new(chunk: usize) -> Self {
        Self {
            buf: BytesMut::with_capacity(chunk * 2),
            chunk: chunk.max(2),
        }
    }

    /// Adiciona bytes e devolve todos os chunks completos disponíveis.
    pub fn push(&mut self, data: &[u8]) -> Vec<Bytes> {
        self.buf.extend_from_slice(data);
        let mut out = Vec::new();
        while self.buf.len() >= self.chunk {
            out.push(self.buf.split_to(self.chunk).freeze());
        }
        out
    }

    /// Devolve o resto pendente (alinhado a amostras de 2 bytes), se houver.
    pub fn flush(&mut self) -> Option<Bytes> {
        if self.buf.is_empty() {
            return None;
        }
        let aligned = self.buf.len() - (self.buf.len() % 2);
        if aligned == 0 {
            return None;
        }
        Some(self.buf.split_to(aligned).freeze())
    }

    pub fn pending_len(&self) -> usize {
        self.buf.len()
    }

    pub fn clear(&mut self) {
        self.buf.clear();
    }
}

pub fn encode_base64(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(data)
}

pub fn decode_base64(data: &str) -> Result<Vec<u8>, base64::DecodeError> {
    base64::engine::general_purpose::STANDARD.decode(data)
}

/// Duração aproximada (ms) de `bytes` de PCM16 24 kHz mono.
pub fn pcm_ms(bytes: usize) -> u64 {
    (bytes / BYTES_PER_MS) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_in_100ms_pieces() {
        let mut c = Chunker::default();
        assert!(c.push(&[0u8; 1000]).is_empty());
        let chunks = c.push(&[0u8; 4800 * 2]);
        assert_eq!(chunks.len(), 2);
        assert!(chunks.iter().all(|b| b.len() == CHUNK_BYTES));
        assert_eq!(c.pending_len(), 1000);
        let rest = c.flush().unwrap();
        assert_eq!(rest.len(), 1000);
        assert!(c.flush().is_none());
    }

    #[test]
    fn flush_aligns_to_samples() {
        let mut c = Chunker::default();
        c.push(&[1u8; 7]);
        assert_eq!(c.flush().unwrap().len(), 6);
        assert_eq!(c.pending_len(), 1);
    }

    #[test]
    fn base64_roundtrip() {
        let data: Vec<u8> = (0..=255u8).collect();
        let enc = encode_base64(&data);
        assert_eq!(decode_base64(&enc).unwrap(), data);
        assert_eq!(pcm_ms(CHUNK_BYTES), 100);
    }
}
