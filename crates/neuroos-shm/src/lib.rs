//! memfd seqlock ring (reader + writer, Rust side)
mod memfd;
mod ring;

pub use memfd::{SharedMap, create_memfd};
pub use ring::{
    DEFAULT_CAPACITY_SLOTS, DEFAULT_SLOT_SIZE, FLAG_CANCEL, FLAG_EOS, HEADER_SIZE, MAGIC, Ring,
    RingError, RingReader, RingWriter, TokenPiece, VERSION,
};
