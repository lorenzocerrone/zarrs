//! An array to bytes codec formed by joining an array to array sequence, array to bytes, and bytes to bytes sequence of codecs.

use crate::{
    array::{
        codec::{
            ArrayCodecTraits, ArrayPartialDecoderCache, ArrayPartialDecoderTraits,
            ArrayToArrayCodecTraits, ArrayToBytesCodecTraits, BytesPartialDecoderCache,
            BytesPartialDecoderTraits, BytesToBytesCodecTraits, Codec, CodecError, CodecOptions,
            CodecTraits,
        },
        concurrency::RecommendedConcurrency,
        ArrayView, BytesRepresentation, ChunkRepresentation,
    },
    metadata::Metadata,
    plugin::PluginCreateError,
};

#[cfg(feature = "async")]
use crate::array::codec::{AsyncArrayPartialDecoderTraits, AsyncBytesPartialDecoderTraits};

/// A codec chain is a sequence of array to array, a bytes to bytes, and a sequence of array to bytes codecs.
///
/// A codec chain partial decoder may insert a cache: [`ArrayPartialDecoderCache`] or [`BytesPartialDecoderCache`].
/// For example, the output of the `blosc`/`gzip` codecs should be cached since they read and decode an entire chunk.
/// If decoding (i.e. going backwards through a codec chain), then a cache may be inserted
///    - following the last codec with [`partial_decoder_decodes_all`](crate::array::codec::CodecTraits::partial_decoder_decodes_all) true, or
///    - preceding the first codec with [`partial_decoder_should_cache_input`](crate::array::codec::CodecTraits::partial_decoder_should_cache_input), whichever is further.
#[derive(Debug, Clone)]
pub struct CodecChain {
    array_to_array: Vec<Box<dyn ArrayToArrayCodecTraits>>,
    array_to_bytes: Box<dyn ArrayToBytesCodecTraits>,
    bytes_to_bytes: Vec<Box<dyn BytesToBytesCodecTraits>>,
    cache_index: Option<usize>, // for partial decoders
}

impl CodecChain {
    /// Create a new codec chain.
    #[must_use]
    pub fn new(
        array_to_array: Vec<Box<dyn ArrayToArrayCodecTraits>>,
        array_to_bytes: Box<dyn ArrayToBytesCodecTraits>,
        bytes_to_bytes: Vec<Box<dyn BytesToBytesCodecTraits>>,
    ) -> Self {
        let mut cache_index_must = None;
        let mut cache_index_should = None;
        let mut codec_index = 0;
        for codec in bytes_to_bytes.iter().rev() {
            if cache_index_should.is_none() && codec.partial_decoder_should_cache_input() {
                cache_index_should = Some(codec_index);
            }
            if codec.partial_decoder_decodes_all() {
                cache_index_must = Some(codec_index + 1);
            }
            codec_index += 1;
        }

        if cache_index_should.is_none() && array_to_bytes.partial_decoder_should_cache_input() {
            cache_index_should = Some(codec_index);
        }
        if array_to_bytes.partial_decoder_decodes_all() {
            cache_index_must = Some(codec_index + 1);
        }
        codec_index += 1;

        for codec in array_to_array.iter().rev() {
            if cache_index_should.is_none() && codec.partial_decoder_should_cache_input() {
                cache_index_should = Some(codec_index);
            }
            if codec.partial_decoder_decodes_all() {
                cache_index_must = Some(codec_index + 1);
            }
            codec_index += 1;
        }

        let cache_index = if let (Some(cache_index_must), Some(cache_index_should)) =
            (cache_index_must, cache_index_should)
        {
            Some(std::cmp::max(cache_index_must, cache_index_should))
        } else if cache_index_must.is_some() {
            cache_index_must
        } else if cache_index_should.is_some() {
            cache_index_should
        } else {
            None
        };

        Self {
            array_to_array,
            array_to_bytes,
            bytes_to_bytes,
            cache_index,
        }
    }

    /// Create a new codec chain from a list of metadata.
    ///
    /// # Errors
    /// Returns a [`PluginCreateError`] if:
    ///  - a codec could not be created,
    ///  - no array to bytes codec is supplied, or
    ///  - more than one array to bytes codec is supplied.
    pub fn from_metadata(metadatas: &[Metadata]) -> Result<Self, PluginCreateError> {
        let mut array_to_array: Vec<Box<dyn ArrayToArrayCodecTraits>> = vec![];
        let mut array_to_bytes: Option<Box<dyn ArrayToBytesCodecTraits>> = None;
        let mut bytes_to_bytes: Vec<Box<dyn BytesToBytesCodecTraits>> = vec![];
        for metadata in metadatas {
            let codec = Codec::from_metadata(metadata)?;
            match codec {
                Codec::ArrayToArray(codec) => {
                    array_to_array.push(codec);
                }
                Codec::ArrayToBytes(codec) => {
                    if array_to_bytes.is_none() {
                        array_to_bytes = Some(codec);
                    } else {
                        return Err(PluginCreateError::from("multiple array to bytes codecs"));
                    }
                }
                Codec::BytesToBytes(codec) => {
                    bytes_to_bytes.push(codec);
                }
            }
        }

        array_to_bytes.map_or_else(
            || Err(PluginCreateError::from("missing array to bytes codec")),
            |array_to_bytes| Ok(Self::new(array_to_array, array_to_bytes, bytes_to_bytes)),
        )
    }

    /// Create codec chain metadata.
    #[must_use]
    pub fn create_metadatas(&self) -> Vec<Metadata> {
        let mut metadatas =
            Vec::with_capacity(self.array_to_array.len() + 1 + self.bytes_to_bytes.len());
        for codec in &self.array_to_array {
            if let Some(metadata) = codec.create_metadata() {
                metadatas.push(metadata);
            }
        }
        if let Some(metadata) = self.array_to_bytes.create_metadata() {
            metadatas.push(metadata);
        }
        for codec in &self.bytes_to_bytes {
            if let Some(metadata) = codec.create_metadata() {
                metadatas.push(metadata);
            }
        }
        metadatas
    }

    /// Get the array to array codecs
    #[must_use]
    pub fn array_to_array_codecs(&self) -> &[Box<dyn ArrayToArrayCodecTraits>] {
        &self.array_to_array
    }

    /// Get the array to bytes codec
    #[allow(clippy::borrowed_box)]
    #[must_use]
    pub fn array_to_bytes_codec(&self) -> &Box<dyn ArrayToBytesCodecTraits> {
        &self.array_to_bytes
    }

    /// Get the bytes to bytes codecs
    #[must_use]
    pub fn bytes_to_bytes_codecs(&self) -> &[Box<dyn BytesToBytesCodecTraits>] {
        &self.bytes_to_bytes
    }

    fn get_array_representations(
        &self,
        decoded_representation: ChunkRepresentation,
    ) -> Result<Vec<ChunkRepresentation>, CodecError> {
        let mut array_representations = Vec::with_capacity(self.array_to_array.len() + 1);
        array_representations.push(decoded_representation);
        for codec in &self.array_to_array {
            array_representations
                .push(codec.compute_encoded_size(array_representations.last().unwrap())?);
        }
        Ok(array_representations)
    }

    fn get_bytes_representations(
        &self,
        array_representation_last: &ChunkRepresentation,
    ) -> Result<Vec<BytesRepresentation>, CodecError> {
        let mut bytes_representations = Vec::with_capacity(self.bytes_to_bytes.len() + 1);
        bytes_representations.push(
            self.array_to_bytes
                .compute_encoded_size(array_representation_last)?,
        );
        for codec in &self.bytes_to_bytes {
            bytes_representations
                .push(codec.compute_encoded_size(bytes_representations.last().unwrap()));
        }
        Ok(bytes_representations)
    }
}

impl CodecTraits for CodecChain {
    /// Returns [`None`] since a codec chain does not have standard codec metadata.
    ///
    /// Note that usage of the codec chain is explicit in [`Array`](crate::array::Array) and [`CodecChain::create_metadatas()`] will call [`CodecTraits::create_metadata()`] from for each codec.
    fn create_metadata(&self) -> Option<Metadata> {
        None
    }

    fn partial_decoder_should_cache_input(&self) -> bool {
        false
    }

    fn partial_decoder_decodes_all(&self) -> bool {
        false
    }
}

#[cfg_attr(feature = "async", async_trait::async_trait)]
impl ArrayToBytesCodecTraits for CodecChain {
    fn partial_decoder<'a>(
        &'a self,
        mut input_handle: Box<dyn BytesPartialDecoderTraits + 'a>,
        decoded_representation: &ChunkRepresentation,
        options: &CodecOptions,
    ) -> Result<Box<dyn ArrayPartialDecoderTraits + 'a>, CodecError> {
        let array_representations =
            self.get_array_representations(decoded_representation.clone())?;
        let bytes_representations =
            self.get_bytes_representations(array_representations.last().unwrap())?;

        let mut codec_index = 0;
        for (codec, bytes_representation) in std::iter::zip(
            self.bytes_to_bytes.iter().rev(),
            bytes_representations.iter().rev().skip(1),
        ) {
            if Some(codec_index) == self.cache_index {
                input_handle = Box::new(BytesPartialDecoderCache::new(&*input_handle, options)?);
            }
            codec_index += 1;
            input_handle = codec.partial_decoder(input_handle, bytes_representation, options)?;
        }

        if Some(codec_index) == self.cache_index {
            input_handle = Box::new(BytesPartialDecoderCache::new(&*input_handle, options)?);
        };

        let mut input_handle = {
            let array_representation = array_representations.last().unwrap();
            let codec = &self.array_to_bytes;
            codec_index += 1;
            codec.partial_decoder(input_handle, array_representation, options)?
        };

        for (codec, array_representation) in std::iter::zip(
            self.array_to_array.iter().rev(),
            array_representations.iter().rev().skip(1),
        ) {
            if Some(codec_index) == self.cache_index {
                input_handle = Box::new(ArrayPartialDecoderCache::new(
                    &*input_handle,
                    array_representation.clone(),
                    options,
                )?);
            }
            codec_index += 1;
            input_handle = codec.partial_decoder(input_handle, array_representation, options)?;
        }

        if Some(codec_index) == self.cache_index {
            input_handle = Box::new(ArrayPartialDecoderCache::new(
                &*input_handle,
                array_representations.first().unwrap().clone(),
                options,
            )?);
        }

        Ok(input_handle)
    }

    #[cfg(feature = "async")]
    async fn async_partial_decoder<'a>(
        &'a self,
        mut input_handle: Box<dyn AsyncBytesPartialDecoderTraits + 'a>,
        decoded_representation: &ChunkRepresentation,
        options: &CodecOptions,
    ) -> Result<Box<dyn AsyncArrayPartialDecoderTraits + 'a>, CodecError> {
        let array_representations =
            self.get_array_representations(decoded_representation.clone())?;
        let bytes_representations =
            self.get_bytes_representations(array_representations.last().unwrap())?;

        let mut codec_index = 0;
        for (codec, bytes_representation) in std::iter::zip(
            self.bytes_to_bytes.iter().rev(),
            bytes_representations.iter().rev().skip(1),
        ) {
            if Some(codec_index) == self.cache_index {
                input_handle =
                    Box::new(BytesPartialDecoderCache::async_new(&*input_handle, options).await?);
            }
            codec_index += 1;
            input_handle = codec
                .async_partial_decoder(input_handle, bytes_representation, options)
                .await?;
        }

        if Some(codec_index) == self.cache_index {
            input_handle =
                Box::new(BytesPartialDecoderCache::async_new(&*input_handle, options).await?);
        };

        let mut input_handle = {
            let array_representation = array_representations.last().unwrap();
            let codec = &self.array_to_bytes;
            codec_index += 1;
            codec
                .async_partial_decoder(input_handle, array_representation, options)
                .await?
        };

        for (codec, array_representation) in std::iter::zip(
            self.array_to_array.iter().rev(),
            array_representations.iter().rev().skip(1),
        ) {
            if Some(codec_index) == self.cache_index {
                input_handle = Box::new(
                    ArrayPartialDecoderCache::async_new(
                        &*input_handle,
                        array_representation.clone(),
                        options,
                    )
                    .await?,
                );
            }
            codec_index += 1;
            input_handle = codec
                .async_partial_decoder(input_handle, array_representation, options)
                .await?;
        }

        if Some(codec_index) == self.cache_index {
            input_handle = Box::new(
                ArrayPartialDecoderCache::async_new(
                    &*input_handle,
                    array_representations.first().unwrap().clone(),
                    options,
                )
                .await?,
            );
        }

        Ok(input_handle)
    }

    fn compute_encoded_size(
        &self,
        decoded_representation: &ChunkRepresentation,
    ) -> Result<BytesRepresentation, CodecError> {
        let mut decoded_representation = decoded_representation.clone();
        for codec in &self.array_to_array {
            decoded_representation = codec.compute_encoded_size(&decoded_representation)?;
        }

        let mut bytes_representation = self
            .array_to_bytes
            .compute_encoded_size(&decoded_representation)?;

        for codec in &self.bytes_to_bytes {
            bytes_representation = codec.compute_encoded_size(&bytes_representation);
        }

        Ok(bytes_representation)
    }
}

impl ArrayCodecTraits for CodecChain {
    fn recommended_concurrency(
        &self,
        decoded_representation: &ChunkRepresentation,
    ) -> Result<RecommendedConcurrency, CodecError> {
        let mut concurrency_min = usize::MAX;
        let mut concurrency_max = 0;

        let array_representations =
            self.get_array_representations(decoded_representation.clone())?;
        let bytes_representations =
            self.get_bytes_representations(array_representations.last().unwrap())?;

        // bytes->bytes
        for (codec, bytes_representation) in std::iter::zip(
            self.bytes_to_bytes.iter().rev(),
            bytes_representations.iter().rev().skip(1),
        ) {
            let recommended_concurrency = &codec.recommended_concurrency(bytes_representation)?;
            concurrency_min = std::cmp::min(concurrency_min, recommended_concurrency.min());
            concurrency_max = std::cmp::max(concurrency_max, recommended_concurrency.max());
        }

        let recommended_concurrency = &self
            .array_to_bytes
            .recommended_concurrency(array_representations.last().unwrap())?;
        concurrency_min = std::cmp::min(concurrency_min, recommended_concurrency.min());
        concurrency_max = std::cmp::max(concurrency_max, recommended_concurrency.max());

        // array->array
        for (codec, array_representation) in std::iter::zip(
            self.array_to_array.iter().rev(),
            array_representations.iter().rev().skip(1),
        ) {
            let recommended_concurrency = codec.recommended_concurrency(array_representation)?;
            concurrency_min = std::cmp::min(concurrency_min, recommended_concurrency.min());
            concurrency_max = std::cmp::max(concurrency_max, recommended_concurrency.max());
        }

        let recommended_concurrency = RecommendedConcurrency::new(
            std::cmp::min(concurrency_min, concurrency_max)
                ..std::cmp::max(concurrency_max, concurrency_max),
        );

        Ok(recommended_concurrency)
    }

    fn encode(
        &self,
        decoded_value: Vec<u8>,
        decoded_representation: &ChunkRepresentation,
        options: &CodecOptions,
    ) -> Result<Vec<u8>, CodecError> {
        if decoded_value.len() as u64 != decoded_representation.size() {
            return Err(CodecError::UnexpectedChunkDecodedSize(
                decoded_value.len(),
                decoded_representation.size(),
            ));
        }

        let mut decoded_representation = decoded_representation.clone();

        let mut value = decoded_value;
        // array->array
        for codec in &self.array_to_array {
            value = codec.encode(value, &decoded_representation, options)?;
            decoded_representation = codec.compute_encoded_size(&decoded_representation)?;
        }

        // array->bytes
        value = self
            .array_to_bytes
            .encode(value, &decoded_representation, options)?;
        let mut decoded_representation = self
            .array_to_bytes
            .compute_encoded_size(&decoded_representation)?;

        // bytes->bytes
        for codec in &self.bytes_to_bytes {
            value = codec.encode(value, options)?;
            decoded_representation = codec.compute_encoded_size(&decoded_representation);
        }

        Ok(value)
    }

    fn decode(
        &self,
        mut encoded_value: Vec<u8>,
        decoded_representation: &ChunkRepresentation,
        options: &CodecOptions,
    ) -> Result<Vec<u8>, CodecError> {
        let array_representations =
            self.get_array_representations(decoded_representation.clone())?;
        let bytes_representations =
            self.get_bytes_representations(array_representations.last().unwrap())?;

        // bytes->bytes
        for (codec, bytes_representation) in std::iter::zip(
            self.bytes_to_bytes.iter().rev(),
            bytes_representations.iter().rev().skip(1),
        ) {
            encoded_value = codec.decode(encoded_value, bytes_representation, options)?;
        }

        // bytes->array
        encoded_value = self.array_to_bytes.decode(
            encoded_value,
            array_representations.last().unwrap(),
            options,
        )?;

        // array->array
        for (codec, array_representation) in std::iter::zip(
            self.array_to_array.iter().rev(),
            array_representations.iter().rev().skip(1),
        ) {
            encoded_value = codec.decode(encoded_value, array_representation, options)?;
        }

        if encoded_value.len() as u64 != decoded_representation.size() {
            return Err(CodecError::UnexpectedChunkDecodedSize(
                encoded_value.len(),
                decoded_representation.size(),
            ));
        }

        Ok(encoded_value)
    }

    fn decode_into_array_view(
        &self,
        encoded_value: &[u8],
        decoded_representation: &ChunkRepresentation,
        array_view: &ArrayView,
        options: &CodecOptions,
    ) -> Result<(), CodecError> {
        let array_representations =
            self.get_array_representations(decoded_representation.clone())?;
        let bytes_representations =
            self.get_bytes_representations(array_representations.last().unwrap())?;

        if self.bytes_to_bytes.is_empty() && self.array_to_array.is_empty() {
            // Shortcut path if no bytes to bytes or array to array codecs
            // TODO: This shouldn't be necessary with appropriate optimisations detailed in below FIXME
            return self.array_to_bytes.decode_into_array_view(
                encoded_value,
                array_representations.last().unwrap(),
                array_view,
                options,
            );
        }

        // Default path
        let mut encoded_value = encoded_value.to_vec();

        // bytes->bytes
        for (codec, bytes_representation) in std::iter::zip(
            self.bytes_to_bytes.iter().rev(),
            bytes_representations.iter().rev().skip(1),
        ) {
            encoded_value = codec.decode(encoded_value, bytes_representation, options)?;
        }

        if self.array_to_array.is_empty() {
            // bytes->array
            self.array_to_bytes.decode_into_array_view(
                &encoded_value,
                array_representations.last().unwrap(),
                array_view,
                options,
            )
        } else {
            // bytes->array
            encoded_value = self.array_to_bytes.decode(
                encoded_value,
                array_representations.last().unwrap(),
                options,
            )?;

            // array->array
            for (codec, array_representation) in std::iter::zip(
                self.array_to_array.iter().rev(),
                array_representations.iter().rev().skip(1),
            ) {
                encoded_value = codec.decode(encoded_value, array_representation, options)?;
            }

            if encoded_value.len() as u64 != decoded_representation.size() {
                return Err(CodecError::UnexpectedChunkDecodedSize(
                    encoded_value.len(),
                    decoded_representation.size(),
                ));
            }

            // FIXME: the last array to array can decode into array_view
            //        Could also identify which filters are passthrough (e.g. bytes if endianness is native/none, transpose in C order, etc.)
            let decoded_value = encoded_value;
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
                let output_offset =
                    usize::try_from(array_subset_element_index).unwrap() * element_size;
                debug_assert!((output_offset + length) <= output.len());
                debug_assert!((decoded_offset + length) <= decoded_value.len());
                output[output_offset..output_offset + length]
                    .copy_from_slice(&decoded_value[decoded_offset..decoded_offset + length]);
                decoded_offset += length;
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use crate::{
        array::{DataType, FillValue},
        array_subset::ArraySubset,
    };

    use super::*;

    #[cfg(feature = "transpose")]
    const JSON_TRANSPOSE1: &str = r#"{
    "name": "transpose",
    "configuration": {
      "order": [0, 2, 1]
    }
}"#;

    #[cfg(feature = "transpose")]
    const JSON_TRANSPOSE2: &str = r#"{
    "name": "transpose",
    "configuration": {
        "order": [2, 0, 1]
    }
}"#;

    #[cfg(feature = "blosc")]
    const JSON_BLOSC: &str = r#"{
    "name": "blosc",
    "configuration": {
        "cname": "lz4",
        "clevel": 5,
        "shuffle": "shuffle",
        "typesize": 2,
        "blocksize": 0
    }
}"#;

    #[cfg(feature = "gzip")]
    const JSON_GZIP: &str = r#"{
    "name": "gzip",
    "configuration": {
        "level": 1
    }
}"#;

    #[cfg(feature = "zstd")]
    const JSON_ZSTD: &str = r#"{
    "name": "zstd",
    "configuration": {
        "level": 1,
        "checksum": false
    }
}"#;

    #[cfg(feature = "bz2")]
    const JSON_BZ2: &str = r#"{ 
    "name": "bz2",
    "configuration": {
        "level": 5
    }
}"#;

    const JSON_BYTES: &str = r#"{
    "name": "bytes",
    "configuration": {
        "endian": "big"
    }
}"#;

    #[cfg(feature = "crc32c")]
    const JSON_CRC32C: &str = r#"{ 
    "name": "crc32c"
}"#;

    #[cfg(feature = "pcodec")]
    const JSON_PCODEC: &str = r#"{ 
    "name": "pcodec"
}"#;

    fn codec_chain_round_trip_impl(
        chunk_representation: ChunkRepresentation,
        elements: Vec<f32>,
        json_array_to_bytes: &str,
        decoded_regions: &[ArraySubset],
        decoded_partial_chunk_true: Vec<f32>,
    ) {
        let bytes = crate::array::transmute_to_bytes_vec(elements);

        let codec_configurations: Vec<Metadata> = vec![
            #[cfg(feature = "transpose")]
            serde_json::from_str(JSON_TRANSPOSE1).unwrap(),
            #[cfg(feature = "transpose")]
            serde_json::from_str(JSON_TRANSPOSE2).unwrap(),
            serde_json::from_str(json_array_to_bytes).unwrap(),
            #[cfg(feature = "blosc")]
            serde_json::from_str(JSON_BLOSC).unwrap(),
            #[cfg(feature = "gzip")]
            serde_json::from_str(JSON_GZIP).unwrap(),
            #[cfg(feature = "zstd")]
            serde_json::from_str(JSON_ZSTD).unwrap(),
            #[cfg(feature = "bz2")]
            serde_json::from_str(JSON_BZ2).unwrap(),
            #[cfg(feature = "crc32c")]
            serde_json::from_str(JSON_CRC32C).unwrap(),
        ];
        println!("{codec_configurations:?}");
        let not_just_bytes = codec_configurations.len() > 1;
        let codec = CodecChain::from_metadata(&codec_configurations).unwrap();

        let encoded = codec
            .encode(
                bytes.clone(),
                &chunk_representation,
                &CodecOptions::default(),
            )
            .unwrap();
        let decoded = codec
            .decode(
                encoded.clone(),
                &chunk_representation,
                &CodecOptions::default(),
            )
            .unwrap();
        if not_just_bytes {
            assert_ne!(encoded, decoded);
        }
        assert_eq!(bytes, decoded);

        // let encoded = codec
        //     .par_encode(bytes.clone(), &chunk_representation)
        //     .unwrap();
        // let decoded = codec
        //     .par_decode(encoded.clone(), &chunk_representation)
        //     .unwrap();
        // if not_just_bytes {
        //     assert_ne!(encoded, decoded);
        // }
        // assert_eq!(bytes, decoded);

        let input_handle = Box::new(std::io::Cursor::new(encoded));
        let partial_decoder = codec
            .partial_decoder(
                input_handle,
                &chunk_representation,
                &CodecOptions::default(),
            )
            .unwrap();
        let decoded_partial_chunk = partial_decoder
            .partial_decode_opt(&decoded_regions, &CodecOptions::default())
            .unwrap();

        let decoded_partial_chunk: Vec<f32> = decoded_partial_chunk
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .chunks(std::mem::size_of::<f32>())
            .map(|b| f32::from_ne_bytes(b.try_into().unwrap()))
            .collect();
        println!("decoded_partial_chunk {decoded_partial_chunk:?}");
        assert_eq!(decoded_partial_chunk_true, decoded_partial_chunk);

        // println!("{} {}", encoded_chunk.len(), decoded_chunk.len());
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn codec_chain_round_trip_bytes() {
        let chunk_shape = vec![
            NonZeroU64::new(2).unwrap(),
            NonZeroU64::new(2).unwrap(),
            NonZeroU64::new(2).unwrap(),
        ];
        let chunk_representation =
            ChunkRepresentation::new(chunk_shape, DataType::Float32, FillValue::from(0f32))
                .unwrap();
        let elements: Vec<f32> = (0..chunk_representation.num_elements())
            .map(|i| i as f32)
            .collect();
        let decoded_regions = [ArraySubset::new_with_ranges(&[0..2, 1..2, 0..1])];
        let decoded_partial_chunk_true = vec![2.0, 6.0];
        codec_chain_round_trip_impl(
            chunk_representation,
            elements,
            JSON_BYTES,
            &decoded_regions,
            decoded_partial_chunk_true,
        );
    }

    #[cfg(feature = "pcodec")]
    #[test]
    #[cfg_attr(miri, ignore)]
    fn codec_chain_round_trip_pcodec() {
        let chunk_shape = vec![
            NonZeroU64::new(2).unwrap(),
            NonZeroU64::new(2).unwrap(),
            NonZeroU64::new(2).unwrap(),
        ];
        let chunk_representation =
            ChunkRepresentation::new(chunk_shape, DataType::Float32, FillValue::from(0f32))
                .unwrap();
        let elements: Vec<f32> = (0..chunk_representation.num_elements())
            .map(|i| i as f32)
            .collect();
        let decoded_regions = [ArraySubset::new_with_ranges(&[0..2, 1..2, 0..1])];
        let decoded_partial_chunk_true = vec![2.0, 6.0];
        codec_chain_round_trip_impl(
            chunk_representation,
            elements,
            JSON_PCODEC,
            &decoded_regions,
            decoded_partial_chunk_true,
        );
    }
}
