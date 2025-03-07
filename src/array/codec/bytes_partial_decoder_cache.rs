//! A cache for partial decoders.

use std::marker::PhantomData;

use crate::{
    array::MaybeBytes,
    byte_range::{extract_byte_ranges, ByteRange},
};

use super::{BytesPartialDecoderTraits, CodecError, CodecOptions};

#[cfg(feature = "async")]
use super::AsyncBytesPartialDecoderTraits;

/// A bytes partial decoder cache.
pub struct BytesPartialDecoderCache<'a> {
    cache: MaybeBytes,
    phantom: PhantomData<&'a ()>,
}

impl<'a> BytesPartialDecoderCache<'a> {
    /// Create a new partial decoder cache.
    ///
    /// # Errors
    /// Returns a [`CodecError`] if caching fails.
    pub fn new(
        input_handle: &dyn BytesPartialDecoderTraits,
        options: &CodecOptions,
    ) -> Result<Self, CodecError> {
        let cache = input_handle
            .partial_decode(&[ByteRange::FromStart(0, None)], options)?
            .map(|mut bytes| bytes.remove(0));
        Ok(Self {
            cache,
            phantom: PhantomData,
        })
    }

    #[cfg(feature = "async")]
    /// Create a new asynchronous partial decoder cache.
    ///
    /// # Errors
    /// Returns a [`CodecError`] if caching fails.
    pub async fn async_new(
        input_handle: &dyn AsyncBytesPartialDecoderTraits,
        options: &CodecOptions,
    ) -> Result<BytesPartialDecoderCache<'a>, CodecError> {
        let cache = input_handle
            .partial_decode(&[ByteRange::FromStart(0, None)], options)
            .await?
            .map(|mut bytes| bytes.remove(0));
        Ok(Self {
            cache,
            phantom: PhantomData,
        })
    }
}

impl BytesPartialDecoderTraits for BytesPartialDecoderCache<'_> {
    fn partial_decode(
        &self,
        decoded_regions: &[ByteRange],
        _options: &CodecOptions,
    ) -> Result<Option<Vec<Vec<u8>>>, CodecError> {
        Ok(match &self.cache {
            Some(bytes) => Some(
                extract_byte_ranges(bytes, decoded_regions)
                    .map_err(CodecError::InvalidByteRangeError)?,
            ),
            None => None,
        })
    }
}

#[cfg(feature = "async")]
#[async_trait::async_trait]
impl AsyncBytesPartialDecoderTraits for BytesPartialDecoderCache<'_> {
    async fn partial_decode(
        &self,
        decoded_regions: &[ByteRange],
        options: &CodecOptions,
    ) -> Result<Option<Vec<Vec<u8>>>, CodecError> {
        BytesPartialDecoderTraits::partial_decode(self, decoded_regions, options)
    }
}
