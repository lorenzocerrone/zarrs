use std::sync::Arc;

use crate::{
    metadata::AdditionalFields,
    node::NodePath,
    storage::storage_transformer::{StorageTransformer, StorageTransformerChain},
};

use super::{
    chunk_key_encoding::{ChunkKeyEncoding, DefaultChunkKeyEncoding},
    codec::{
        ArrayToArrayCodecTraits, ArrayToBytesCodecTraits, BytesCodec, BytesToBytesCodecTraits,
    },
    Array, ArrayCreateError, ArrayShape, ChunkGrid, CodecChain, DataType, DimensionName, FillValue,
};

/// An [`Array`] builder.
///
/// The array builder is initialised from an array shape, data type, chunk grid, and fill value.
///  - The only codec enabled by default is `bytes` (with native endian encoding), so the output is uncompressed.
///  - The default chunk key encoding is `default` with the `/` chunk key separator.
///  - Attributes, storage transformers, and dimension names are empty.
///  - Codecs are configured to use multiple threads where possible.
///
/// Use the methods in the array builder to change the configuration away from these defaults, and then build the array at a path of some storage with [`ArrayBuilder::build`].
/// Note that [`build`](ArrayBuilder::build) does not modify the store; the array metadata has to be explicitly written with [`Array::store_metadata`].
///
/// For example:
///
/// ```rust
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # use std::sync::Arc;
/// use zarrs::array::{ArrayBuilder, DataType, FillValue};
/// # let store = Arc::new(zarrs::storage::store::MemoryStore::default());
/// let mut array = ArrayBuilder::new(
///     vec![8, 8], // array shape
///     DataType::Float32,
///     vec![4, 4].into(), // regular chunk shape
///     FillValue::from(f32::NAN),
/// )
/// .bytes_to_bytes_codecs(vec![
///     #[cfg(feature = "gzip")]
///     Box::new(zarrs::array::codec::GzipCodec::new(5)?),
/// ])
/// .dimension_names(vec!["y".into(), "x".into()])
/// .build(store.clone(), "/group/array")?;
/// array.store_metadata()?; // write metadata to the store
///
/// // array.store_chunk(...)
/// // array.store_array_subset(...)
///
/// array.set_shape(vec![16, 16]); // revise the shape if needed
/// array.store_metadata()?; // update stored metadata
///
/// # Ok(())
/// # }
/// ```
pub struct ArrayBuilder {
    shape: ArrayShape,
    data_type: DataType,
    chunk_grid: ChunkGrid,
    chunk_key_encoding: ChunkKeyEncoding,
    fill_value: FillValue,
    array_to_array_codecs: Vec<Box<dyn ArrayToArrayCodecTraits>>,
    array_to_bytes_codec: Box<dyn ArrayToBytesCodecTraits>,
    bytes_to_bytes_codecs: Vec<Box<dyn BytesToBytesCodecTraits>>,
    storage_transformers: StorageTransformerChain,
    attributes: serde_json::Map<String, serde_json::Value>,
    dimension_names: Option<Vec<DimensionName>>,
    additional_fields: AdditionalFields,
    parallel_codecs: bool,
}

impl ArrayBuilder {
    /// Create a new array builder for an array at `path`.
    ///
    /// The length of the array shape must match the dimensionality of the intended array, but it can be all zeros on initialisation.
    /// The shape of the [`Array`] can be be updated as required.
    #[must_use]
    pub fn new(
        shape: ArrayShape,
        data_type: DataType,
        chunk_grid: ChunkGrid,
        fill_value: FillValue,
    ) -> Self {
        Self {
            shape,
            data_type,
            chunk_grid,
            chunk_key_encoding: Box::<DefaultChunkKeyEncoding>::default(),
            fill_value,
            array_to_array_codecs: Vec::default(),
            array_to_bytes_codec: Box::<BytesCodec>::default(),
            bytes_to_bytes_codecs: Vec::default(),
            attributes: serde_json::Map::default(),
            storage_transformers: Vec::default().into(),
            dimension_names: None,
            additional_fields: AdditionalFields::default(),
            parallel_codecs: true,
        }
    }

    /// Set the array to array codecs.
    ///
    /// If left unmodified, the array will have no array to array codecs.
    #[must_use]
    pub fn array_to_array_codecs(
        mut self,
        array_to_array_codecs: Vec<Box<dyn ArrayToArrayCodecTraits>>,
    ) -> Self {
        self.array_to_array_codecs = array_to_array_codecs;
        self
    }

    /// Set the array to bytes codec.
    ///
    /// If left unmodified, the array will default to using the `bytes` codec with native endian encoding.
    #[must_use]
    pub fn array_to_bytes_codec(
        mut self,
        array_to_bytes_codec: Box<dyn ArrayToBytesCodecTraits>,
    ) -> Self {
        self.array_to_bytes_codec = array_to_bytes_codec;
        self
    }

    /// Set the bytes to bytes codecs.
    ///
    /// If left unmodified, the array will have no bytes to bytes codecs.
    #[must_use]
    pub fn bytes_to_bytes_codecs(
        mut self,
        bytes_to_bytes_codecs: Vec<Box<dyn BytesToBytesCodecTraits>>,
    ) -> Self {
        self.bytes_to_bytes_codecs = bytes_to_bytes_codecs;
        self
    }

    /// Set the user defined attributes.
    ///
    /// If left unmodified, the user defined attributes of the array will be empty.
    #[must_use]
    pub fn attributes(mut self, attributes: serde_json::Map<String, serde_json::Value>) -> Self {
        self.attributes = attributes;
        self
    }

    /// Set the additional fields.
    ///
    /// Set additional fields not defined in the Zarr specification.
    /// Use this cautiously. In general, store user defined attributes using [`ArrayBuilder::attributes`].
    #[must_use]
    pub fn additional_fields(mut self, additional_fields: AdditionalFields) -> Self {
        self.additional_fields = additional_fields;
        self
    }

    /// Set the dimension names.
    ///
    /// If left unmodified, all dimension names are "unnamed".
    #[must_use]
    pub fn dimension_names(mut self, dimension_names: Vec<DimensionName>) -> Self {
        self.dimension_names = Some(dimension_names);
        self
    }

    /// Set the storage transformers.
    ///
    /// If left unmodified, there are no storage transformers.
    #[must_use]
    pub fn storage_transformers(mut self, storage_transformers: Vec<StorageTransformer>) -> Self {
        self.storage_transformers = storage_transformers.into();
        self
    }

    /// Set whether or not to use multithreaded codec encoding and decoding.
    ///
    /// If parallel codecs is not set, it defaults to true.
    #[must_use]
    pub fn parallel_codecs(mut self, parallel_codecs: bool) -> Self {
        self.parallel_codecs = parallel_codecs;
        self
    }

    /// Build into an [`Array`].
    ///
    /// # Errors
    ///
    /// Returns [`ArrayCreateError`] if there is an error creating the array.
    /// This can be due to a storage error, an invalid path, or a problem with array configuration.
    pub fn build<TStorage>(
        self,
        storage: Arc<TStorage>,
        path: &str,
    ) -> Result<Array<TStorage>, ArrayCreateError> {
        let path: NodePath = path.try_into()?;
        if self.chunk_grid.dimensionality() != self.shape.len() {
            return Err(ArrayCreateError::InvalidChunkGridDimensionality(
                self.chunk_grid.dimensionality(),
                self.shape.len(),
            ));
        }
        if let Some(dimension_names) = &self.dimension_names {
            if dimension_names.len() != self.shape.len() {
                return Err(ArrayCreateError::InvalidDimensionNames(
                    dimension_names.len(),
                    self.shape.len(),
                ));
            }
        }

        Ok(Array {
            storage,
            path,
            shape: self.shape,
            data_type: self.data_type,
            chunk_grid: self.chunk_grid,
            chunk_key_encoding: self.chunk_key_encoding,
            fill_value: self.fill_value,
            codecs: CodecChain::new(
                self.array_to_array_codecs,
                self.array_to_bytes_codec,
                self.bytes_to_bytes_codecs,
            ),
            storage_transformers: self.storage_transformers,
            attributes: self.attributes,
            dimension_names: self.dimension_names,
            additional_fields: self.additional_fields,
            parallel_codecs: self.parallel_codecs,
        })
    }
}
