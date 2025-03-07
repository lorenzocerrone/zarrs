use zstd::zstd_safe;

use crate::{
    array::{
        codec::{
            BytesPartialDecoderTraits, BytesToBytesCodecTraits, CodecError, CodecOptions,
            CodecTraits, RecommendedConcurrency,
        },
        BytesRepresentation,
    },
    metadata::Metadata,
};

#[cfg(feature = "async")]
use crate::array::codec::AsyncBytesPartialDecoderTraits;

use super::{zstd_partial_decoder, ZstdCodecConfiguration, ZstdCodecConfigurationV1, IDENTIFIER};

/// A `zstd` codec implementation.
#[derive(Clone, Debug)]
pub struct ZstdCodec {
    compression: zstd_safe::CompressionLevel,
    checksum: bool,
}

impl ZstdCodec {
    /// Create a new `Zstd` codec.
    #[must_use]
    pub const fn new(compression: zstd_safe::CompressionLevel, checksum: bool) -> Self {
        Self {
            compression,
            checksum,
        }
    }

    /// Create a new `Zstd` codec from configuration.
    #[must_use]
    pub fn new_with_configuration(configuration: &ZstdCodecConfiguration) -> Self {
        let ZstdCodecConfiguration::V1(configuration) = configuration;
        Self {
            compression: configuration.level.clone().into(),
            checksum: configuration.checksum,
        }
    }
}

impl CodecTraits for ZstdCodec {
    fn create_metadata(&self) -> Option<Metadata> {
        let configuration = ZstdCodecConfigurationV1 {
            level: self.compression.into(),
            checksum: self.checksum,
        };
        Some(Metadata::new_with_serializable_configuration(IDENTIFIER, &configuration).unwrap())
    }

    fn partial_decoder_should_cache_input(&self) -> bool {
        false
    }

    fn partial_decoder_decodes_all(&self) -> bool {
        true
    }
}

#[cfg_attr(feature = "async", async_trait::async_trait)]
impl BytesToBytesCodecTraits for ZstdCodec {
    fn recommended_concurrency(
        &self,
        _decoded_representation: &BytesRepresentation,
    ) -> Result<RecommendedConcurrency, CodecError> {
        // TODO: zstd supports multithread, but at what point is it good to kick in?
        Ok(RecommendedConcurrency::new_maximum(1))
    }

    fn encode(
        &self,
        decoded_value: Vec<u8>,
        _options: &CodecOptions,
    ) -> Result<Vec<u8>, CodecError> {
        let mut result = Vec::<u8>::new();
        let mut encoder = zstd::Encoder::new(&mut result, self.compression)?;
        encoder.include_checksum(self.checksum)?;
        // if parallel {
        //     let n_threads = std::thread::available_parallelism().unwrap().get();
        //     encoder.multithread(u32::try_from(n_threads).unwrap())?; // TODO: Check overhead of zstd par_encode
        // }
        std::io::copy(&mut decoded_value.as_slice(), &mut encoder)?;
        encoder.finish()?;
        Ok(result)
    }

    fn decode(
        &self,
        encoded_value: Vec<u8>,
        _decoded_representation: &BytesRepresentation,
        _options: &CodecOptions,
    ) -> Result<Vec<u8>, CodecError> {
        zstd::decode_all(encoded_value.as_slice()).map_err(CodecError::IOError)
    }

    fn partial_decoder<'a>(
        &self,
        r: Box<dyn BytesPartialDecoderTraits + 'a>,
        _decoded_representation: &BytesRepresentation,
        _options: &CodecOptions,
    ) -> Result<Box<dyn BytesPartialDecoderTraits + 'a>, CodecError> {
        Ok(Box::new(zstd_partial_decoder::ZstdPartialDecoder::new(r)))
    }

    #[cfg(feature = "async")]
    async fn async_partial_decoder<'a>(
        &'a self,
        r: Box<dyn AsyncBytesPartialDecoderTraits + 'a>,
        _decoded_representation: &BytesRepresentation,
        _options: &CodecOptions,
    ) -> Result<Box<dyn AsyncBytesPartialDecoderTraits + 'a>, CodecError> {
        Ok(Box::new(
            zstd_partial_decoder::AsyncZstdPartialDecoder::new(r),
        ))
    }

    fn compute_encoded_size(
        &self,
        decoded_representation: &BytesRepresentation,
    ) -> BytesRepresentation {
        decoded_representation
            .size()
            .map_or(BytesRepresentation::UnboundedSize, |size| {
                // https://github.com/facebook/zstd/blob/dev/doc/zstd_compression_format.md
                // TODO: Validate the window/block relationship
                const HEADER_TRAILER_OVERHEAD: u64 = 4 + 14 + 4;
                const MIN_WINDOW_SIZE: u64 = 1000; // 1KB
                const BLOCK_OVERHEAD: u64 = 3;
                let blocks_overhead =
                    BLOCK_OVERHEAD * ((size + MIN_WINDOW_SIZE - 1) / MIN_WINDOW_SIZE);
                BytesRepresentation::BoundedSize(size + HEADER_TRAILER_OVERHEAD + blocks_overhead)
            })
    }
}
