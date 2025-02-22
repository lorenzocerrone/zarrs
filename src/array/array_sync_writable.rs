use std::sync::Arc;

use rayon::iter::{IntoParallelIterator, ParallelIterator};
use rayon_iter_concurrent_limit::iter_concurrent_limit;

use crate::{
    array_subset::ArraySubset,
    storage::{StorageError, StorageHandle, WritableStorageTraits},
};

use super::{
    codec::{options::CodecOptions, ArrayCodecTraits},
    concurrency::concurrency_chunks_and_codec,
    Array, ArrayError,
};

impl<TStorage: ?Sized + WritableStorageTraits + 'static> Array<TStorage> {
    /// Store metadata.
    ///
    /// # Errors
    /// Returns [`StorageError`] if there is an underlying store error.
    pub fn store_metadata(&self) -> Result<(), StorageError> {
        let storage_handle = Arc::new(StorageHandle::new(self.storage.clone()));
        let storage_transformer = self
            .storage_transformers()
            .create_writable_transformer(storage_handle);
        crate::storage::create_array(&*storage_transformer, self.path(), &self.metadata())
    }

    /// Encode `chunk_bytes` and store at `chunk_indices`.
    ///
    /// Use [`store_chunk_opt`](Array::store_chunk_opt) to control codec options.
    /// A chunk composed entirely of the fill value will not be written to the store.
    ///
    /// # Errors
    /// Returns an [`ArrayError`] if
    ///  - `chunk_indices` are invalid,
    ///  - the length of `chunk_bytes` is not equal to the expected length (the product of the number of elements in the chunk and the data type size in bytes),
    ///  - there is a codec encoding error, or
    ///  - an underlying store error.
    pub fn store_chunk(
        &self,
        chunk_indices: &[u64],
        chunk_bytes: Vec<u8>,
    ) -> Result<(), ArrayError> {
        self.store_chunk_opt(chunk_indices, chunk_bytes, &CodecOptions::default())
    }

    /// Encode `chunk_elements` and store at `chunk_indices`.
    ///
    /// Use [`store_chunk_elements_opt`](Array::store_chunk_elements_opt) to control codec options.
    /// A chunk composed entirely of the fill value will not be written to the store.
    ///
    /// # Errors
    /// Returns an [`ArrayError`] if
    ///  - the size of  `T` does not match the data type size, or
    ///  - a [`store_chunk`](Array::store_chunk) error condition is met.
    pub fn store_chunk_elements<T: bytemuck::Pod>(
        &self,
        chunk_indices: &[u64],
        chunk_elements: Vec<T>,
    ) -> Result<(), ArrayError> {
        self.store_chunk_elements_opt(chunk_indices, chunk_elements, &CodecOptions::default())
    }

    #[cfg(feature = "ndarray")]
    /// Encode `chunk_array` and store at `chunk_indices`.
    ///
    /// Use [`store_chunk_ndarray_opt`](Array::store_chunk_ndarray_opt) to control codec options.
    ///
    /// # Errors
    /// Returns an [`ArrayError`] if
    ///  - the shape of the array does not match the shape of the chunk,
    ///  - a [`store_chunk_elements`](Array::store_chunk_elements) error condition is met.
    #[allow(clippy::missing_errors_doc, clippy::missing_panics_doc)]
    pub fn store_chunk_ndarray<
        T: bytemuck::Pod,
        TArray: Into<ndarray::Array<T, D>>,
        D: ndarray::Dimension,
    >(
        &self,
        chunk_indices: &[u64],
        chunk_array: TArray,
    ) -> Result<(), ArrayError> {
        self.store_chunk_ndarray_opt(chunk_indices, chunk_array, &CodecOptions::default())
    }

    /// Encode `chunks_bytes` and store at the chunks with indices represented by the `chunks` array subset.
    ///
    /// Use [`store_chunks_opt`](Array::store_chunks_opt) to control codec options.
    /// A chunk composed entirely of the fill value will not be written to the store.
    ///
    /// # Errors
    /// Returns an [`ArrayError`] if
    ///  - `chunks` are invalid,
    ///  - the length of `chunk_bytes` is not equal to the expected length (the product of the number of elements in the chunks and the data type size in bytes),
    ///  - there is a codec encoding error, or
    ///  - an underlying store error.
    #[allow(clippy::similar_names)]
    #[allow(clippy::missing_errors_doc, clippy::missing_panics_doc)]
    pub fn store_chunks(
        &self,
        chunks: &ArraySubset,
        chunks_bytes: Vec<u8>,
    ) -> Result<(), ArrayError> {
        self.store_chunks_opt(chunks, chunks_bytes, &CodecOptions::default())
    }

    /// Encode `chunks_elements` and store at the chunks with indices represented by the `chunks` array subset.
    ///
    /// # Errors
    /// Returns an [`ArrayError`] if
    ///  - the size of  `T` does not match the data type size, or
    ///  - a [`store_chunks`](Array::store_chunks) error condition is met.
    #[allow(clippy::missing_errors_doc, clippy::missing_panics_doc)]
    pub fn store_chunks_elements<T: bytemuck::Pod>(
        &self,
        chunks: &ArraySubset,
        chunks_elements: Vec<T>,
    ) -> Result<(), ArrayError> {
        self.store_chunks_elements_opt(chunks, chunks_elements, &CodecOptions::default())
    }

    #[cfg(feature = "ndarray")]
    /// Encode `chunks_array` and store at the chunks with indices represented by the `chunks` array subset.
    ///
    /// # Errors
    /// Returns an [`ArrayError`] if
    ///  - the shape of the array does not match the shape of the chunks,
    ///  - a [`store_chunks_elements`](Array::store_chunks_elements) error condition is met.
    #[allow(clippy::missing_errors_doc, clippy::missing_panics_doc)]
    pub fn store_chunks_ndarray<
        T: bytemuck::Pod,
        TArray: Into<ndarray::Array<T, D>>,
        D: ndarray::Dimension,
    >(
        &self,
        chunks: &ArraySubset,
        chunks_array: TArray,
    ) -> Result<(), ArrayError> {
        self.store_chunks_ndarray_opt(chunks, chunks_array, &CodecOptions::default())
    }

    /// Erase the chunk at `chunk_indices`.
    ///
    /// Succeeds if the chunk does not exist.
    ///
    /// # Errors
    /// Returns a [`StorageError`] if there is an underlying store error.
    pub fn erase_chunk(&self, chunk_indices: &[u64]) -> Result<(), StorageError> {
        let storage_handle = Arc::new(StorageHandle::new(self.storage.clone()));
        let storage_transformer = self
            .storage_transformers()
            .create_writable_transformer(storage_handle);
        crate::storage::erase_chunk(
            &*storage_transformer,
            self.path(),
            chunk_indices,
            self.chunk_key_encoding(),
        )
    }

    /// Erase the chunks in `chunks`.
    ///
    /// # Errors
    /// Returns a [`StorageError`] if there is an underlying store error.
    pub fn erase_chunks(&self, chunks: &ArraySubset) -> Result<(), StorageError> {
        let storage_handle = Arc::new(StorageHandle::new(self.storage.clone()));
        let storage_transformer = self
            .storage_transformers()
            .create_writable_transformer(storage_handle);
        let erase_chunk = |chunk_indices: Vec<u64>| {
            crate::storage::erase_chunk(
                &*storage_transformer,
                self.path(),
                &chunk_indices,
                self.chunk_key_encoding(),
            )
        };

        chunks.indices().into_par_iter().try_for_each(erase_chunk)
    }

    /////////////////////////////////////////////////////////////////////////////
    // Advanced methods
    /////////////////////////////////////////////////////////////////////////////

    /// Explicit options version of [`store_chunk`](Array::store_chunk).
    #[allow(clippy::missing_errors_doc)]
    pub fn store_chunk_opt(
        &self,
        chunk_indices: &[u64],
        chunk_bytes: Vec<u8>,
        options: &CodecOptions,
    ) -> Result<(), ArrayError> {
        // Validation
        let chunk_array_representation = self.chunk_array_representation(chunk_indices)?;
        if chunk_bytes.len() as u64 != chunk_array_representation.size() {
            return Err(ArrayError::InvalidBytesInputSize(
                chunk_bytes.len(),
                chunk_array_representation.size(),
            ));
        }

        let all_fill_value = self.fill_value().equals_all(&chunk_bytes);
        if all_fill_value {
            self.erase_chunk(chunk_indices)?;
            Ok(())
        } else {
            let storage_handle = Arc::new(StorageHandle::new(self.storage.clone()));
            let storage_transformer = self
                .storage_transformers()
                .create_writable_transformer(storage_handle);
            let chunk_encoded: Vec<u8> = self
                .codecs()
                .encode(chunk_bytes, &chunk_array_representation, options)
                .map_err(ArrayError::CodecError)?;
            crate::storage::store_chunk(
                &*storage_transformer,
                self.path(),
                chunk_indices,
                self.chunk_key_encoding(),
                &chunk_encoded,
            )
            .map_err(ArrayError::StorageError)
        }
    }

    /// Explicit options version of [`store_chunk_elements`](Array::store_chunk_elements).
    #[allow(clippy::missing_errors_doc)]
    pub fn store_chunk_elements_opt<T: bytemuck::Pod>(
        &self,
        chunk_indices: &[u64],
        chunk_elements: Vec<T>,
        options: &CodecOptions,
    ) -> Result<(), ArrayError> {
        array_store_elements!(
            self,
            chunk_elements,
            store_chunk_opt(chunk_indices, chunk_elements, options)
        )
    }

    #[cfg(feature = "ndarray")]
    /// Explicit options version of [`store_chunk_ndarray`](Array::store_chunk_ndarray).
    #[allow(clippy::missing_errors_doc)]
    pub fn store_chunk_ndarray_opt<
        T: bytemuck::Pod,
        TArray: Into<ndarray::Array<T, D>>,
        D: ndarray::Dimension,
    >(
        &self,
        chunk_indices: &[u64],
        chunk_array: TArray,
        options: &CodecOptions,
    ) -> Result<(), ArrayError> {
        let chunk_array: ndarray::Array<T, D> = chunk_array.into();
        let chunk_shape = self.chunk_shape_usize(chunk_indices)?;
        if chunk_array.shape() == chunk_shape {
            array_store_ndarray!(
                self,
                chunk_array,
                store_chunk_elements_opt(chunk_indices, chunk_array, options)
            )
        } else {
            Err(ArrayError::InvalidDataShape(
                chunk_array.shape().to_vec(),
                chunk_shape,
            ))
        }
    }

    /// Explicit options version of [`store_chunks`](Array::store_chunks).
    #[allow(clippy::similar_names)]
    #[allow(clippy::missing_errors_doc)]
    pub fn store_chunks_opt(
        &self,
        chunks: &ArraySubset,
        chunks_bytes: Vec<u8>,
        options: &CodecOptions,
    ) -> Result<(), ArrayError> {
        let num_chunks = chunks.num_elements_usize();
        match num_chunks {
            0 => {}
            1 => {
                let chunk_indices = chunks.start();
                self.store_chunk_opt(chunk_indices, chunks_bytes, options)?;
            }
            _ => {
                let array_subset = self.chunks_subset(chunks)?;
                let element_size = self.data_type().size();
                let expected_size = element_size as u64 * array_subset.num_elements();
                if chunks_bytes.len() as u64 != expected_size {
                    return Err(ArrayError::InvalidBytesInputSize(
                        chunks_bytes.len(),
                        expected_size,
                    ));
                }

                // Calculate chunk/codec concurrency
                let chunk_representation =
                    self.chunk_array_representation(&vec![0; self.dimensionality()])?;
                let codec_concurrency =
                    self.recommended_codec_concurrency(&chunk_representation)?;
                let (chunk_concurrent_limit, options) = concurrency_chunks_and_codec(
                    options.concurrent_target(),
                    num_chunks,
                    options,
                    &codec_concurrency,
                );

                let store_chunk = |chunk_indices: Vec<u64>| -> Result<(), ArrayError> {
                    let chunk_subset_in_array = unsafe {
                        self.chunk_grid()
                            .subset_unchecked(&chunk_indices, self.shape())
                            .ok_or_else(|| {
                                ArrayError::InvalidChunkGridIndicesError(chunk_indices.clone())
                            })?
                    };
                    let overlap = unsafe { array_subset.overlap_unchecked(&chunk_subset_in_array) };
                    let chunk_subset_in_array_subset =
                        unsafe { overlap.relative_to_unchecked(array_subset.start()) };
                    #[allow(clippy::similar_names)]
                    let chunk_bytes = unsafe {
                        chunk_subset_in_array_subset.extract_bytes_unchecked(
                            &chunks_bytes,
                            array_subset.shape(),
                            element_size,
                        )
                    };

                    debug_assert_eq!(
                        chunk_subset_in_array.num_elements(),
                        chunk_subset_in_array_subset.num_elements()
                    );

                    self.store_chunk_opt(&chunk_indices, chunk_bytes, &options)
                };
                let indices = chunks.indices();
                iter_concurrent_limit!(
                    chunk_concurrent_limit,
                    indices.into_par_iter(),
                    try_for_each,
                    store_chunk
                )?;
            }
        }

        Ok(())
    }

    /// Explicit options version of [`store_chunks_elements`](Array::store_chunks_elements).
    #[allow(clippy::missing_errors_doc)]
    pub fn store_chunks_elements_opt<T: bytemuck::Pod>(
        &self,
        chunks: &ArraySubset,
        chunks_elements: Vec<T>,
        options: &CodecOptions,
    ) -> Result<(), ArrayError> {
        array_store_elements!(
            self,
            chunks_elements,
            store_chunks_opt(chunks, chunks_elements, options)
        )
    }

    #[cfg(feature = "ndarray")]
    /// Explicit options version of [`store_chunks_ndarray`](Array::store_chunks_ndarray).
    #[allow(clippy::missing_errors_doc)]
    pub fn store_chunks_ndarray_opt<
        T: bytemuck::Pod,
        TArray: Into<ndarray::Array<T, D>>,
        D: ndarray::Dimension,
    >(
        &self,
        chunks: &ArraySubset,
        chunks_array: TArray,
        options: &CodecOptions,
    ) -> Result<(), ArrayError> {
        let chunks_array: ndarray::Array<T, D> = chunks_array.into();
        let chunks_subset = self.chunks_subset(chunks)?;
        let chunks_shape = chunks_subset.shape_usize();
        if chunks_array.shape() == chunks_shape {
            array_store_ndarray!(
                self,
                chunks_array,
                store_chunks_elements_opt(chunks, chunks_array, options)
            )
        } else {
            Err(ArrayError::InvalidDataShape(
                chunks_array.shape().to_vec(),
                chunks_shape,
            ))
        }
    }
}
