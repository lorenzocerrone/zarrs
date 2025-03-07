//! Zarr codecs.
//!
//! Array chunks can be encoded using a sequence of codecs, each of which specifies a bidirectional transform (an encode transform and a decode transform).
//! A codec can map array to an array, an array to bytes, or bytes to bytes.
//! A codec may support partial decoding to extract a byte range or array subset without needing to decode the entire input.
//!
//! A [`CodecChain`] represents a codec sequence consisting of any number of array to array and bytes to bytes codecs, and one array to bytes codec.
//! A codec chain is itself an array to bytes codec.
//! A [`ArrayPartialDecoderCache`] or [`BytesPartialDecoderCache`] may be inserted into a codec chain to optimise partial decoding where appropriate.
//!
//! See <https://zarr-specs.readthedocs.io/en/latest/v3/core/v3.0.html#id18>.

pub mod array_to_array;
pub mod array_to_bytes;
pub mod bytes_to_bytes;
pub mod options;

pub use options::{CodecOptions, CodecOptionsBuilder};

// Array to array
#[cfg(feature = "bitround")]
pub use array_to_array::bitround::{
    BitroundCodec, BitroundCodecConfiguration, BitroundCodecConfigurationV1,
};
#[cfg(feature = "transpose")]
pub use array_to_array::transpose::{
    TransposeCodec, TransposeCodecConfiguration, TransposeCodecConfigurationV1,
};

// Array to bytes
#[cfg(feature = "sharding")]
pub use array_to_bytes::sharding::{
    ShardingCodec, ShardingCodecConfiguration, ShardingCodecConfigurationV1,
};
#[cfg(feature = "zfp")]
pub use array_to_bytes::zfp::{ZfpCodec, ZfpCodecConfiguration, ZfpCodecConfigurationV1};
pub use array_to_bytes::{
    bytes::{BytesCodec, BytesCodecConfiguration, BytesCodecConfigurationV1},
    codec_chain::CodecChain,
};

// Bytes to bytes
#[cfg(feature = "blosc")]
pub use bytes_to_bytes::blosc::{BloscCodec, BloscCodecConfiguration, BloscCodecConfigurationV1};
#[cfg(feature = "crc32c")]
pub use bytes_to_bytes::crc32c::{
    Crc32cCodec, Crc32cCodecConfiguration, Crc32cCodecConfigurationV1,
};
#[cfg(feature = "gzip")]
pub use bytes_to_bytes::gzip::{GzipCodec, GzipCodecConfiguration, GzipCodecConfigurationV1};
#[cfg(feature = "zstd")]
pub use bytes_to_bytes::zstd::{ZstdCodec, ZstdCodecConfiguration, ZstdCodecConfigurationV1};

use itertools::Itertools;
use thiserror::Error;

mod array_partial_decoder_cache;
mod bytes_partial_decoder_cache;
pub use array_partial_decoder_cache::ArrayPartialDecoderCache;
pub use bytes_partial_decoder_cache::BytesPartialDecoderCache;

mod byte_interval_partial_decoder;
pub use byte_interval_partial_decoder::ByteIntervalPartialDecoder;

#[cfg(feature = "async")]
pub use byte_interval_partial_decoder::AsyncByteIntervalPartialDecoder;

use crate::{
    array_subset::{ArraySubset, IncompatibleArraySubsetAndShapeError},
    byte_range::{ByteOffset, ByteRange, InvalidByteRangeError},
    metadata::Metadata,
    plugin::{Plugin, PluginCreateError},
    storage::{ReadableStorage, StorageError, StoreKey},
};

#[cfg(feature = "async")]
use crate::storage::AsyncReadableStorage;

use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Read, Seek, SeekFrom},
};

use super::{
    concurrency::RecommendedConcurrency, ArrayView, BytesRepresentation, ChunkRepresentation,
    DataType, MaybeBytes,
};

/// A codec plugin.
pub type CodecPlugin = Plugin<Codec>;
inventory::collect!(CodecPlugin);

/// A generic array to array, array to bytes, or bytes to bytes codec.
#[derive(Debug)]
pub enum Codec {
    /// An array to array codec.
    ArrayToArray(Box<dyn ArrayToArrayCodecTraits>),
    /// An array to bytes codec.
    ArrayToBytes(Box<dyn ArrayToBytesCodecTraits>),
    /// A bytes to bytes codec.
    BytesToBytes(Box<dyn BytesToBytesCodecTraits>),
}

impl Codec {
    /// Create a codec from metadata.
    ///
    /// # Errors
    /// Returns [`PluginCreateError`] if the metadata is invalid or not associated with a registered codec plugin.
    pub fn from_metadata(metadata: &Metadata) -> Result<Self, PluginCreateError> {
        for plugin in inventory::iter::<CodecPlugin> {
            if plugin.match_name(metadata.name()) {
                return plugin.create(metadata);
            }
        }
        #[cfg(miri)]
        {
            // Inventory does not work in miri, so manually handle all known codecs
            match metadata.name() {
                #[cfg(feature = "transpose")]
                array_to_array::transpose::IDENTIFIER => {
                    return array_to_array::transpose::create_codec_transpose(metadata);
                }
                #[cfg(feature = "bitround")]
                array_to_array::bitround::IDENTIFIER => {
                    return array_to_array::bitround::create_codec_bitround(metadata);
                }
                array_to_bytes::bytes::IDENTIFIER => {
                    return array_to_bytes::bytes::create_codec_bytes(metadata);
                }
                #[cfg(feature = "pcodec")]
                array_to_bytes::pcodec::IDENTIFIER => {
                    return array_to_bytes::pcodec::create_codec_pcodec(metadata);
                }
                #[cfg(feature = "sharding")]
                array_to_bytes::sharding::IDENTIFIER => {
                    return array_to_bytes::sharding::create_codec_sharding(metadata);
                }
                #[cfg(feature = "zfp")]
                array_to_bytes::zfp::IDENTIFIER => {
                    return array_to_bytes::zfp::create_codec_zfp(metadata);
                }
                #[cfg(feature = "blosc")]
                bytes_to_bytes::blosc::IDENTIFIER => {
                    return bytes_to_bytes::blosc::create_codec_blosc(metadata);
                }
                #[cfg(feature = "bz2")]
                bytes_to_bytes::bz2::IDENTIFIER => {
                    return bytes_to_bytes::bz2::create_codec_bz2(metadata);
                }
                #[cfg(feature = "crc32c")]
                bytes_to_bytes::crc32c::IDENTIFIER => {
                    return bytes_to_bytes::crc32c::create_codec_crc32c(metadata);
                }
                #[cfg(feature = "gzip")]
                bytes_to_bytes::gzip::IDENTIFIER => {
                    return bytes_to_bytes::gzip::create_codec_gzip(metadata);
                }
                #[cfg(feature = "zstd")]
                bytes_to_bytes::zstd::IDENTIFIER => {
                    return bytes_to_bytes::zstd::create_codec_zstd(metadata);
                }
                _ => {}
            }
        }
        Err(PluginCreateError::Unsupported {
            name: metadata.name().to_string(),
            plugin_type: "codec".to_string(),
        })
    }
}

/// Codec traits.
pub trait CodecTraits: Send + Sync {
    /// Create metadata.
    ///
    /// A hidden codec (e.g. a cache) will return [`None`], since it will not have any associated metadata.
    fn create_metadata(&self) -> Option<Metadata>;

    /// Indicates if the input to a codecs partial decoder should be cached for optimal performance.
    /// If true, a cache may be inserted *before* it in a [`CodecChain`] partial decoder.
    fn partial_decoder_should_cache_input(&self) -> bool;

    /// Indicates if a partial decoder decodes all bytes from its input handle and its output should be cached for optimal performance.
    /// If true, a cache will be inserted at some point *after* it in a [`CodecChain`] partial decoder.
    fn partial_decoder_decodes_all(&self) -> bool;
}

/// Traits for both array to array and array to bytes codecs.
pub trait ArrayCodecTraits: CodecTraits {
    /// Return the recommended concurrency for the requested decoded representation.
    ///
    /// # Errors
    /// Returns [`CodecError`] if the decoded representation is not valid for the codec.
    fn recommended_concurrency(
        &self,
        decoded_representation: &ChunkRepresentation,
    ) -> Result<RecommendedConcurrency, CodecError>;

    /// Encode a chunk.
    ///
    /// # Errors
    /// Returns [`CodecError`] if a codec fails or `decoded_value` is incompatible with `decoded_representation`.
    fn encode(
        &self,
        decoded_value: Vec<u8>,
        decoded_representation: &ChunkRepresentation,
        options: &CodecOptions,
    ) -> Result<Vec<u8>, CodecError>;

    /// Decode a chunk.
    ///
    /// # Errors
    /// Returns [`CodecError`] if a codec fails or the decoded output is incompatible with `decoded_representation`.
    fn decode(
        &self,
        encoded_value: Vec<u8>,
        decoded_representation: &ChunkRepresentation,
        options: &CodecOptions,
    ) -> Result<Vec<u8>, CodecError>;

    /// Decode into the subset of an array.
    ///
    /// The default implementation decodes the chunk as normal then copies it into the array subset.
    /// Codecs can override this method to avoid allocations where possible.
    ///
    /// # Errors
    /// Returns an error if the internal call to [`decode`](ArrayCodecTraits::decode) fails.
    fn decode_into_array_view(
        &self,
        encoded_value: &[u8],
        decoded_representation: &ChunkRepresentation,
        array_view: &ArrayView,
        options: &CodecOptions,
    ) -> Result<(), CodecError> {
        let decoded_bytes = self.decode(encoded_value.to_vec(), decoded_representation, options)?;
        let contiguous_indices = unsafe {
            array_view
                .subset()
                .contiguous_linearised_indices_unchecked(array_view.array_shape())
        };
        let element_size = decoded_representation.element_size();
        let length = contiguous_indices.contiguous_elements_usize() * element_size;
        let mut decoded_offset = 0;
        // FIXME: Par iteration?
        let output = unsafe { array_view.bytes_mut() };
        for (array_subset_element_index, _num_elements) in &contiguous_indices {
            let output_offset = usize::try_from(array_subset_element_index).unwrap() * element_size;
            debug_assert!((output_offset + length) <= output.len());
            debug_assert!((decoded_offset + length) <= decoded_bytes.len());
            output[output_offset..output_offset + length]
                .copy_from_slice(&decoded_bytes[decoded_offset..decoded_offset + length]);
            decoded_offset += length;
        }
        Ok(())
    }
}

/// Partial bytes decoder traits.
pub trait BytesPartialDecoderTraits: Send + Sync {
    /// Partially decode bytes.
    ///
    /// Returns [`None`] if partial decoding of the input handle returns [`None`].
    ///
    /// # Errors
    /// Returns [`CodecError`] if a codec fails or a byte range is invalid.
    fn partial_decode(
        &self,
        decoded_regions: &[ByteRange],
        options: &CodecOptions,
    ) -> Result<Option<Vec<Vec<u8>>>, CodecError>;

    /// Decode all bytes.
    ///
    /// Returns [`None`] if partial decoding of the input handle returns [`None`].
    ///
    /// # Errors
    /// Returns [`CodecError`] if a codec fails.
    fn decode(&self, options: &CodecOptions) -> Result<MaybeBytes, CodecError> {
        Ok(self
            .partial_decode(&[ByteRange::FromStart(0, None)], options)?
            .map(|mut v| v.remove(0)))
    }
}

#[cfg(feature = "async")]
/// Asynchronous partial bytes decoder traits.
#[async_trait::async_trait]
pub trait AsyncBytesPartialDecoderTraits: Send + Sync {
    /// Partially decode bytes.
    ///
    /// Returns [`None`] if partial decoding of the input handle returns [`None`].
    ///
    /// # Errors
    /// Returns [`CodecError`] if a codec fails or a byte range is invalid.
    async fn partial_decode(
        &self,
        decoded_regions: &[ByteRange],
        options: &CodecOptions,
    ) -> Result<Option<Vec<Vec<u8>>>, CodecError>;

    /// Decode all bytes.
    ///
    /// Returns [`None`] if partial decoding of the input handle returns [`None`].
    ///
    /// # Errors
    /// Returns [`CodecError`] if a codec fails.
    async fn decode(&self, options: &CodecOptions) -> Result<MaybeBytes, CodecError> {
        Ok(self
            .partial_decode(&[ByteRange::FromStart(0, None)], options)
            .await?
            .map(|mut v| v.remove(0)))
    }
}

/// Partial array decoder traits.
pub trait ArrayPartialDecoderTraits: Send + Sync {
    /// Return the element size of the partial decoder.
    fn element_size(&self) -> usize;

    /// Partially decode a chunk with default codec options.
    ///
    /// If the inner `input_handle` is a bytes decoder and partial decoding returns [`None`], then the array subsets have the fill value.
    /// Use [`partial_decode_opt`](ArrayPartialDecoderTraits::partial_decode_opt) to control codec options.
    ///
    /// # Errors
    /// Returns [`CodecError`] if a codec fails or an array subset is invalid.
    fn partial_decode(&self, array_subsets: &[ArraySubset]) -> Result<Vec<Vec<u8>>, CodecError> {
        self.partial_decode_opt(array_subsets, &CodecOptions::default())
    }

    /// Explicit options version of [`partial_decode`](ArrayPartialDecoderTraits::partial_decode).
    #[allow(clippy::missing_errors_doc, clippy::missing_panics_doc)]
    fn partial_decode_opt(
        &self,
        array_subsets: &[ArraySubset],
        options: &CodecOptions,
    ) -> Result<Vec<Vec<u8>>, CodecError>;

    /// Partially decode a subset of an array into an array view with default codec options.
    ///
    /// Use [`partial_decode_into_array_view_opt`](ArrayPartialDecoderTraits::partial_decode_into_array_view_opt) to control codec options.
    ///
    /// # Errors
    /// Returns [`CodecError`] if a codec fails, array subset is invalid, or the array subset shape does not match array view subset shape.
    fn partial_decode_into_array_view(
        &self,
        array_subset: &ArraySubset,
        array_view: &ArrayView,
    ) -> Result<(), CodecError> {
        self.partial_decode_into_array_view_opt(array_subset, array_view, &CodecOptions::default())
    }

    // TODO: Override partial_decode_into_array_view_opt for CodecChain/Sharding
    /// Explicit options version of [`partial_decode_into_array_view`](ArrayPartialDecoderTraits::partial_decode_into_array_view).
    #[allow(clippy::missing_errors_doc)]
    fn partial_decode_into_array_view_opt(
        &self,
        array_subset: &ArraySubset,
        array_view: &ArrayView,
        options: &CodecOptions,
    ) -> Result<(), CodecError> {
        if array_subset.shape() != array_view.subset().shape() {
            return Err(CodecError::InvalidArraySubsetError(
                IncompatibleArraySubsetAndShapeError::new(
                    array_subset.clone(),
                    array_view.array_shape().to_vec(),
                ),
            ));
        }

        let decoded_bytes = self
            .partial_decode_opt(&[array_subset.clone()], options)?
            .pop()
            .unwrap();
        let contiguous_indices = unsafe {
            array_view
                .subset()
                .contiguous_linearised_indices_unchecked(array_view.array_shape())
        };
        let element_size = self.element_size();
        let length = contiguous_indices.contiguous_elements_usize() * element_size;
        let mut decoded_offset = 0;
        // FIXME: Par iteration?
        let output = unsafe { array_view.bytes_mut() };
        for (array_subset_element_index, _num_elements) in &contiguous_indices {
            let output_offset = usize::try_from(array_subset_element_index).unwrap() * element_size;
            debug_assert!((output_offset + length) <= output.len());
            debug_assert!((decoded_offset + length) <= decoded_bytes.len());
            output[output_offset..output_offset + length]
                .copy_from_slice(&decoded_bytes[decoded_offset..decoded_offset + length]);
            decoded_offset += length;
        }
        Ok(())
    }
}

#[cfg(feature = "async")]
/// Asynchronous partial array decoder traits.
#[async_trait::async_trait]
pub trait AsyncArrayPartialDecoderTraits: Send + Sync {
    /// Return the element size of the partial decoder.
    fn element_size(&self) -> usize;

    /// Partially decode a chunk with default codec options.
    ///
    /// # Errors
    /// Returns [`CodecError`] if a codec fails, array subset is invalid, or the array subset shape does not match array view subset shape.
    async fn partial_decode(
        &self,
        array_subsets: &[ArraySubset],
    ) -> Result<Vec<Vec<u8>>, CodecError> {
        self.partial_decode_opt(array_subsets, &CodecOptions::default())
            .await
    }

    /// Explicit options variant of [`partial_decode`](AsyncArrayPartialDecoderTraits::partial_decode).
    async fn partial_decode_opt(
        &self,
        array_subsets: &[ArraySubset],
        options: &CodecOptions,
    ) -> Result<Vec<Vec<u8>>, CodecError>;

    /// Partially decode a subset of an array into an array view with default codec options.
    ///
    /// # Errors
    /// Returns [`CodecError`] if a codec fails, array subset is invalid, or the array subset shape does not match array view subset shape.
    async fn partial_decode_into_array_view(
        &self,
        array_subset: &ArraySubset,
        array_view: &ArrayView,
    ) -> Result<(), CodecError> {
        self.partial_decode_into_array_view_opt(array_subset, array_view, &CodecOptions::default())
            .await
    }

    /// Explicit options variant of [`partial_decode_into_array_view`](AsyncArrayPartialDecoderTraits::partial_decode_into_array_view).
    // TODO: Override this for CodecChain/Sharding
    async fn partial_decode_into_array_view_opt(
        &self,
        array_subset: &ArraySubset,
        array_view: &ArrayView,
        options: &CodecOptions,
    ) -> Result<(), CodecError> {
        if array_subset.shape() != array_view.subset().shape() {
            return Err(CodecError::InvalidArraySubsetError(
                IncompatibleArraySubsetAndShapeError::new(
                    array_subset.clone(),
                    array_view.array_shape().to_vec(),
                ),
            ));
        }

        let decoded_bytes = self
            .partial_decode_opt(&[array_subset.clone()], options)
            .await?
            .pop()
            .unwrap();
        let contiguous_indices = unsafe {
            array_view
                .subset()
                .contiguous_linearised_indices_unchecked(array_view.array_shape())
        };
        let element_size = self.element_size();
        let length = contiguous_indices.contiguous_elements_usize() * element_size;
        let mut decoded_offset = 0;
        // FIXME: Par iteration?
        let output = unsafe { array_view.bytes_mut() };
        for (array_subset_element_index, _num_elements) in &contiguous_indices {
            let output_offset = usize::try_from(array_subset_element_index).unwrap() * element_size;
            debug_assert!((output_offset + length) <= output.len());
            debug_assert!((decoded_offset + length) <= decoded_bytes.len());
            output[output_offset..output_offset + length]
                .copy_from_slice(&decoded_bytes[decoded_offset..decoded_offset + length]);
            decoded_offset += length;
        }
        Ok(())
    }
}

/// A [`ReadableStorage`] partial decoder.
pub struct StoragePartialDecoder {
    storage: ReadableStorage,
    key: StoreKey,
}

impl StoragePartialDecoder {
    /// Create a new storage partial decoder.
    pub fn new(storage: ReadableStorage, key: StoreKey) -> Self {
        Self { storage, key }
    }
}

impl BytesPartialDecoderTraits for StoragePartialDecoder {
    fn partial_decode(
        &self,
        decoded_regions: &[ByteRange],
        _options: &CodecOptions,
    ) -> Result<Option<Vec<Vec<u8>>>, CodecError> {
        Ok(self
            .storage
            .get_partial_values_key(&self.key, decoded_regions)?)
    }
}

#[cfg(feature = "async")]
/// A [`ReadableStorage`] partial decoder.
pub struct AsyncStoragePartialDecoder {
    storage: AsyncReadableStorage,
    key: StoreKey,
}

#[cfg(feature = "async")]
impl AsyncStoragePartialDecoder {
    /// Create a new storage partial decoder.
    pub fn new(storage: AsyncReadableStorage, key: StoreKey) -> Self {
        Self { storage, key }
    }
}

#[cfg(feature = "async")]
#[async_trait::async_trait]
impl AsyncBytesPartialDecoderTraits for AsyncStoragePartialDecoder {
    async fn partial_decode(
        &self,
        decoded_regions: &[ByteRange],
        _options: &CodecOptions,
    ) -> Result<Option<Vec<Vec<u8>>>, CodecError> {
        Ok(self
            .storage
            .get_partial_values_key(&self.key, decoded_regions)
            .await?)
    }
}

/// Traits for array to array codecs.
#[cfg_attr(feature = "async", async_trait::async_trait)]
pub trait ArrayToArrayCodecTraits:
    ArrayCodecTraits + dyn_clone::DynClone + core::fmt::Debug
{
    /// Returns the size of the encoded representation given a size of the decoded representation.
    ///
    /// # Errors
    ///
    /// Returns a [`CodecError`] if the decoded representation is not supported by this codec.
    fn compute_encoded_size(
        &self,
        decoded_representation: &ChunkRepresentation,
    ) -> Result<ChunkRepresentation, CodecError>;

    /// Initialise a partial decoder.
    ///
    /// `parallel` only affects parallelism on initialisation, which is irrelevant for most codecs.
    /// Parallel partial decoding support is independent of how the partial decoder is initialised.
    ///
    /// # Errors
    /// Returns a [`CodecError`] if initialisation fails.
    fn partial_decoder<'a>(
        &'a self,
        input_handle: Box<dyn ArrayPartialDecoderTraits + 'a>,
        decoded_representation: &ChunkRepresentation,
        options: &CodecOptions,
    ) -> Result<Box<dyn ArrayPartialDecoderTraits + 'a>, CodecError>;

    #[cfg(feature = "async")]
    /// Initialise an asynchronous partial decoder.
    ///
    /// `parallel` only affects parallelism on initialisation, which is irrelevant for most codecs.
    /// Parallel partial decoding support is independent of how the partial decoder is initialised.
    ///
    /// # Errors
    /// Returns a [`CodecError`] if initialisation fails.
    async fn async_partial_decoder<'a>(
        &'a self,
        input_handle: Box<dyn AsyncArrayPartialDecoderTraits + 'a>,
        decoded_representation: &ChunkRepresentation,
        options: &CodecOptions,
    ) -> Result<Box<dyn AsyncArrayPartialDecoderTraits + 'a>, CodecError>;
}

dyn_clone::clone_trait_object!(ArrayToArrayCodecTraits);

/// Traits for array to bytes codecs.
#[cfg_attr(feature = "async", async_trait::async_trait)]
pub trait ArrayToBytesCodecTraits:
    ArrayCodecTraits + dyn_clone::DynClone + core::fmt::Debug
{
    /// Returns the size of the encoded representation given a size of the decoded representation.
    ///
    /// # Errors
    /// Returns a [`CodecError`] if the decoded representation is not supported by this codec.
    fn compute_encoded_size(
        &self,
        decoded_representation: &ChunkRepresentation,
    ) -> Result<BytesRepresentation, CodecError>;

    /// Initialise a partial decoder.
    ///
    /// # Errors
    /// Returns a [`CodecError`] if initialisation fails.
    fn partial_decoder<'a>(
        &'a self,
        input_handle: Box<dyn BytesPartialDecoderTraits + 'a>,
        decoded_representation: &ChunkRepresentation,
        options: &CodecOptions,
    ) -> Result<Box<dyn ArrayPartialDecoderTraits + 'a>, CodecError>;

    #[cfg(feature = "async")]
    /// Initialise an asynchronous partial decoder.
    ///
    /// # Errors
    /// Returns a [`CodecError`] if initialisation fails.
    async fn async_partial_decoder<'a>(
        &'a self,
        mut input_handle: Box<dyn AsyncBytesPartialDecoderTraits + 'a>,
        decoded_representation: &ChunkRepresentation,
        options: &CodecOptions,
    ) -> Result<Box<dyn AsyncArrayPartialDecoderTraits + 'a>, CodecError>;
}

dyn_clone::clone_trait_object!(ArrayToBytesCodecTraits);

/// Traits for bytes to bytes codecs.
#[cfg_attr(feature = "async", async_trait::async_trait)]
pub trait BytesToBytesCodecTraits: CodecTraits + dyn_clone::DynClone + core::fmt::Debug {
    /// Return the maximum internal concurrency supported for the requested decoded representation.
    ///
    /// # Errors
    /// Returns [`CodecError`] if the decoded representation is not valid for the codec.
    fn recommended_concurrency(
        &self,
        decoded_representation: &BytesRepresentation,
    ) -> Result<RecommendedConcurrency, CodecError>;

    /// Returns the size of the encoded representation given a size of the decoded representation.
    fn compute_encoded_size(
        &self,
        decoded_representation: &BytesRepresentation,
    ) -> BytesRepresentation;

    /// Encode chunk bytes.
    ///
    /// # Errors
    /// Returns [`CodecError`] if a codec fails.
    fn encode(&self, decoded_value: Vec<u8>, options: &CodecOptions)
        -> Result<Vec<u8>, CodecError>;

    /// Decode chunk bytes.
    //
    /// # Errors
    /// Returns [`CodecError`] if a codec fails.
    fn decode(
        &self,
        encoded_value: Vec<u8>,
        decoded_representation: &BytesRepresentation,
        options: &CodecOptions,
    ) -> Result<Vec<u8>, CodecError>;

    /// Initialises a partial decoder.
    ///
    /// # Errors
    /// Returns a [`CodecError`] if initialisation fails.
    fn partial_decoder<'a>(
        &'a self,
        input_handle: Box<dyn BytesPartialDecoderTraits + 'a>,
        decoded_representation: &BytesRepresentation,
        options: &CodecOptions,
    ) -> Result<Box<dyn BytesPartialDecoderTraits + 'a>, CodecError>;

    #[cfg(feature = "async")]
    /// Initialises an asynchronous partial decoder.
    ///
    /// # Errors
    /// Returns a [`CodecError`] if initialisation fails.
    async fn async_partial_decoder<'a>(
        &'a self,
        input_handle: Box<dyn AsyncBytesPartialDecoderTraits + 'a>,
        decoded_representation: &BytesRepresentation,
        options: &CodecOptions,
    ) -> Result<Box<dyn AsyncBytesPartialDecoderTraits + 'a>, CodecError>;
}

dyn_clone::clone_trait_object!(BytesToBytesCodecTraits);

impl BytesPartialDecoderTraits for std::io::Cursor<&[u8]> {
    fn partial_decode(
        &self,
        decoded_regions: &[ByteRange],
        _parallel: &CodecOptions,
    ) -> Result<Option<Vec<Vec<u8>>>, CodecError> {
        Ok(Some(extract_byte_ranges_read_seek(
            &mut self.clone(),
            decoded_regions,
        )?))
    }
}

impl BytesPartialDecoderTraits for std::io::Cursor<Vec<u8>> {
    fn partial_decode(
        &self,
        decoded_regions: &[ByteRange],
        _parallel: &CodecOptions,
    ) -> Result<Option<Vec<Vec<u8>>>, CodecError> {
        Ok(Some(extract_byte_ranges_read_seek(
            &mut self.clone(),
            decoded_regions,
        )?))
    }
}

#[cfg(feature = "async")]
#[async_trait::async_trait]
impl AsyncBytesPartialDecoderTraits for std::io::Cursor<&[u8]> {
    async fn partial_decode(
        &self,
        decoded_regions: &[ByteRange],
        _parallel: &CodecOptions,
    ) -> Result<Option<Vec<Vec<u8>>>, CodecError> {
        Ok(Some(extract_byte_ranges_read_seek(
            &mut self.clone(),
            decoded_regions,
        )?))
    }
}

#[cfg(feature = "async")]
#[async_trait::async_trait]
impl AsyncBytesPartialDecoderTraits for std::io::Cursor<Vec<u8>> {
    async fn partial_decode(
        &self,
        decoded_regions: &[ByteRange],
        _parallel: &CodecOptions,
    ) -> Result<Option<Vec<Vec<u8>>>, CodecError> {
        Ok(Some(extract_byte_ranges_read_seek(
            &mut self.clone(),
            decoded_regions,
        )?))
    }
}

/// A codec error.
#[derive(Debug, Error)]
pub enum CodecError {
    /// An IO error.
    #[error(transparent)]
    IOError(#[from] std::io::Error),
    /// An invalid byte range was requested.
    #[error(transparent)]
    InvalidByteRangeError(#[from] InvalidByteRangeError),
    /// An invalid array subset was requested.
    #[error(transparent)]
    InvalidArraySubsetError(#[from] IncompatibleArraySubsetAndShapeError),
    /// An invalid array subset was requested with the wrong dimensionality.
    #[error("the array subset {_0} has the wrong dimensionality, expected {_1}")]
    InvalidArraySubsetDimensionalityError(ArraySubset, usize),
    /// The decoded size of a chunk did not match what was expected.
    #[error("the size of a decoded chunk is {_0}, expected {_1}")]
    UnexpectedChunkDecodedSize(usize, u64),
    /// An embedded checksum does not match the decoded value.
    #[error("the checksum is invalid")]
    InvalidChecksum,
    /// A store error.
    #[error(transparent)]
    StorageError(#[from] StorageError),
    /// Unsupported data type
    #[error("Unsupported data type {0} for codec {1}")]
    UnsupportedDataType(DataType, String),
    /// Other
    #[error("{_0}")]
    Other(String),
}

impl From<&str> for CodecError {
    fn from(err: &str) -> Self {
        Self::Other(err.to_string())
    }
}

impl From<String> for CodecError {
    fn from(err: String) -> Self {
        Self::Other(err)
    }
}

/// Extract byte ranges from bytes implementing [`Read`] and [`Seek`].
///
/// # Errors
///
/// Returns a [`std::io::Error`] if there is an error reading or seeking from `bytes`.
/// This can occur if the byte range is out-of-bounds of the `bytes`.
///
/// # Panics
///
/// Panics if a byte has length exceeding [`usize::MAX`].
pub fn extract_byte_ranges_read_seek<T: Read + Seek>(
    bytes: &mut T,
    byte_ranges: &[ByteRange],
) -> std::io::Result<Vec<Vec<u8>>> {
    let len: u64 = bytes.seek(SeekFrom::End(0))?;
    let mut out = Vec::with_capacity(byte_ranges.len());
    for byte_range in byte_ranges {
        let data: Vec<u8> = match byte_range {
            ByteRange::FromStart(offset, None) => {
                bytes.seek(SeekFrom::Start(*offset))?;
                let length = usize::try_from(len).unwrap();
                let mut data = vec![0; length];
                bytes.read_exact(&mut data)?;
                data
            }
            ByteRange::FromStart(offset, Some(length)) => {
                bytes.seek(SeekFrom::Start(*offset))?;
                let length = usize::try_from(*length).unwrap();
                let mut data = vec![0; length];
                bytes.read_exact(&mut data)?;
                data
            }
            ByteRange::FromEnd(offset, None) => {
                bytes.seek(SeekFrom::Start(0))?;
                let length = usize::try_from(len - offset).unwrap();
                let mut data = vec![0; length];
                bytes.read_exact(&mut data)?;
                data
            }
            ByteRange::FromEnd(offset, Some(length)) => {
                bytes.seek(SeekFrom::End(-i64::try_from(*offset + *length).unwrap()))?;
                let length = usize::try_from(*length).unwrap();
                let mut data = vec![0; length];
                bytes.read_exact(&mut data)?;
                data
            }
        };
        out.push(data);
    }
    Ok(out)
}

/// Extract byte ranges from bytes implementing [`Read`].
///
/// # Errors
///
/// Returns a [`std::io::Error`] if there is an error reading from `bytes`.
/// This can occur if the byte range is out-of-bounds of the `bytes`.
///
/// # Panics
///
/// Panics if a byte has length exceeding [`usize::MAX`].
pub fn extract_byte_ranges_read<T: Read>(
    bytes: &mut T,
    size: u64,
    byte_ranges: &[ByteRange],
) -> std::io::Result<Vec<Vec<u8>>> {
    // Could this be cleaner/more efficient?

    // Allocate output and find the endpoints of the "segments" of bytes which must be read
    let mut out = Vec::with_capacity(byte_ranges.len());
    let mut segments_endpoints = BTreeSet::<u64>::new();
    for byte_range in byte_ranges {
        out.push(vec![0; usize::try_from(byte_range.length(size)).unwrap()]);
        segments_endpoints.insert(byte_range.start(size));
        segments_endpoints.insert(byte_range.end(size));
    }

    // Find the overlapping part of each byte range with each segment
    //                 SEGMENT start     , end        OUTPUT index, offset
    let mut overlap: BTreeMap<(ByteOffset, ByteOffset), Vec<(usize, ByteOffset)>> = BTreeMap::new();
    for (byte_range_index, byte_range) in byte_ranges.iter().enumerate() {
        let byte_range_start = byte_range.start(size);
        let range = segments_endpoints.range((
            std::ops::Bound::Included(byte_range_start),
            std::ops::Bound::Included(byte_range.end(size)),
        ));
        for (segment_start, segment_end) in range.tuple_windows() {
            let byte_range_offset = *segment_start - byte_range_start;
            overlap
                .entry((*segment_start, *segment_end))
                .or_default()
                .push((byte_range_index, byte_range_offset));
        }
    }

    let mut bytes_offset = 0u64;
    for ((segment_start, segment_end), outputs) in overlap {
        // Go to the start of the segment
        if segment_start > bytes_offset {
            std::io::copy(
                &mut bytes.take(segment_start - bytes_offset),
                &mut std::io::sink(),
            )
            .unwrap();
        }

        let segment_length = segment_end - segment_start;
        if outputs.is_empty() {
            // No byte ranges are associated with this segment, so just read it to sink
            std::io::copy(&mut bytes.take(segment_length), &mut std::io::sink()).unwrap();
        } else {
            // Populate all byte ranges in this segment with data
            let segment_length_usize = usize::try_from(segment_length).unwrap();
            let mut segment_bytes = vec![0; segment_length_usize];
            bytes.take(segment_length).read_exact(&mut segment_bytes)?;
            for (byte_range_index, byte_range_offset) in outputs {
                let byte_range_offset = usize::try_from(byte_range_offset).unwrap();
                out[byte_range_index][byte_range_offset..byte_range_offset + segment_length_usize]
                    .copy_from_slice(&segment_bytes);
            }
        }

        // Offset is now the end of the segment
        bytes_offset = segment_end;
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_byte_ranges_read() {
        let data: Vec<u8> = (0..10).collect();
        let size = data.len() as u64;
        let mut read = std::io::Cursor::new(data);
        let byte_ranges = vec![
            ByteRange::FromStart(3, Some(3)),
            ByteRange::FromStart(4, Some(1)),
            ByteRange::FromStart(1, Some(1)),
            ByteRange::FromEnd(1, Some(5)),
        ];
        let out = extract_byte_ranges_read(&mut read, size, &byte_ranges).unwrap();
        assert_eq!(
            out,
            vec![vec![3, 4, 5], vec![4], vec![1], vec![4, 5, 6, 7, 8]]
        );
    }
}
