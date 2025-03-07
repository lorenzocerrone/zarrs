//! An in-memory store.

use parking_lot::RwLock;
use std::sync::Mutex;

use crate::{
    array::MaybeBytes,
    byte_range::{ByteOffset, ByteRange, InvalidByteRangeError},
    storage::{
        store_lock::{DefaultStoreLocks, StoreKeyMutex, StoreLocks},
        store_set_partial_values, ListableStorageTraits, ReadableStorageTraits,
        ReadableWritableStorageTraits, StorageError, StoreKey, StoreKeyRange, StoreKeyStartValue,
        StoreKeys, StoreKeysPrefixes, StorePrefix, WritableStorageTraits,
    },
};

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

/// An in-memory store.
#[derive(Debug)]
pub struct MemoryStore {
    data_map: Mutex<BTreeMap<StoreKey, Arc<RwLock<Vec<u8>>>>>,
    locks: StoreLocks,
}

impl Default for MemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryStore {
    /// Create a new memory store at a given `base_directory`.
    #[must_use]
    pub fn new() -> Self {
        Self::new_with_locks(Arc::new(DefaultStoreLocks::default()))
    }

    /// Create a new memory store at a given `base_directory` with a non-default store lock.
    #[must_use]
    pub fn new_with_locks(store_locks: StoreLocks) -> Self {
        Self {
            data_map: Mutex::default(),
            locks: store_locks,
        }
    }

    fn set_impl(&self, key: &StoreKey, value: &[u8], offset: Option<ByteOffset>, _truncate: bool) {
        let mut data_map = self.data_map.lock().unwrap();
        let data = data_map
            .entry(key.clone())
            .or_insert_with(|| Arc::new(RwLock::default()))
            .clone();
        drop(data_map);
        let mut data = data.write();

        let offset = offset.unwrap_or(0);
        if offset == 0 && data.is_empty() {
            // fast path
            *data = value.to_vec();
        } else {
            let length = usize::try_from(offset + value.len() as u64).unwrap();
            if data.len() < length {
                data.resize(length, 0);
            }
            let offset = usize::try_from(offset).unwrap();
            data[offset..offset + value.len()].copy_from_slice(value);
        }
    }
}

impl ReadableStorageTraits for MemoryStore {
    fn get(&self, key: &StoreKey) -> Result<MaybeBytes, StorageError> {
        let data_map = self.data_map.lock().unwrap();
        let data = data_map.get(key);
        if let Some(data) = data {
            let data = data.clone();
            drop(data_map);
            let data = data.read();
            Ok(Some(data.clone()))
        } else {
            Ok(None)
        }
    }

    fn get_partial_values_key(
        &self,
        key: &StoreKey,
        byte_ranges: &[ByteRange],
    ) -> Result<Option<Vec<Vec<u8>>>, StorageError> {
        let data_map = self.data_map.lock().unwrap();
        let data = data_map.get(key);
        if let Some(data) = data {
            let data = data.clone();
            drop(data_map);
            let data = data.read();
            let mut out = Vec::with_capacity(byte_ranges.len());
            for byte_range in byte_ranges {
                let start = usize::try_from(byte_range.start(data.len() as u64)).unwrap();
                let end = usize::try_from(byte_range.end(data.len() as u64)).unwrap();
                if end > data.len() {
                    return Err(InvalidByteRangeError::new(*byte_range, data.len() as u64).into());
                }
                let bytes = data[start..end].to_vec();
                out.push(bytes);
            }
            Ok(Some(out))
        } else {
            Ok(None)
        }
    }

    fn get_partial_values(
        &self,
        key_ranges: &[StoreKeyRange],
    ) -> Result<Vec<MaybeBytes>, StorageError> {
        self.get_partial_values_batched_by_key(key_ranges)
    }

    fn size_prefix(&self, prefix: &StorePrefix) -> Result<u64, StorageError> {
        let mut size = 0;
        for key in self.list_prefix(prefix)? {
            if let Some(size_key) = self.size_key(&key)? {
                size += size_key;
            }
        }
        Ok(size)
    }

    fn size_key(&self, key: &StoreKey) -> Result<Option<u64>, StorageError> {
        let data_map = self.data_map.lock().unwrap();
        data_map
            .get(key)
            .map_or_else(|| Ok(None), |entry| Ok(Some(entry.read().len() as u64)))
    }
}

impl WritableStorageTraits for MemoryStore {
    fn set(&self, key: &StoreKey, value: &[u8]) -> Result<(), StorageError> {
        Self::set_impl(self, key, value, None, true);
        Ok(())
    }

    fn set_partial_values(
        &self,
        key_start_values: &[StoreKeyStartValue],
    ) -> Result<(), StorageError> {
        store_set_partial_values(self, key_start_values)
    }

    fn erase(&self, key: &StoreKey) -> Result<(), StorageError> {
        let mut data_map = self.data_map.lock().unwrap();
        data_map.remove(key);
        Ok(())
    }

    fn erase_prefix(&self, prefix: &StorePrefix) -> Result<(), StorageError> {
        let mut data_map = self.data_map.lock().unwrap();
        let keys: Vec<StoreKey> = data_map.keys().cloned().collect();
        for key in keys {
            if key.has_prefix(prefix) {
                data_map.remove(&key);
            }
        }
        Ok(())
    }
}

impl ReadableWritableStorageTraits for MemoryStore {
    fn mutex(&self, key: &StoreKey) -> Result<StoreKeyMutex, StorageError> {
        Ok(self.locks.mutex(key))
    }
}

impl ListableStorageTraits for MemoryStore {
    fn list(&self) -> Result<StoreKeys, StorageError> {
        let data_map = self.data_map.lock().unwrap();
        Ok(data_map.keys().cloned().collect())
    }

    fn list_prefix(&self, prefix: &StorePrefix) -> Result<StoreKeys, StorageError> {
        let data_map = self.data_map.lock().unwrap();
        Ok(data_map
            .keys()
            .filter(|&key| key.has_prefix(prefix))
            .cloned()
            .collect())
    }

    fn list_dir(&self, prefix: &StorePrefix) -> Result<StoreKeysPrefixes, StorageError> {
        let mut keys: StoreKeys = vec![];
        let mut prefixes: BTreeSet<StorePrefix> = BTreeSet::default();
        let data_map = self.data_map.lock().unwrap();
        for key in data_map.keys() {
            if key.has_prefix(prefix) {
                let key_strip = key.as_str().strip_prefix(prefix.as_str()).unwrap();
                let key_strip = key_strip.strip_prefix('/').unwrap_or(key_strip);
                let components: Vec<_> = key_strip.split('/').collect();
                if components.len() > 1 {
                    prefixes.insert(StorePrefix::new(
                        prefix.as_str().to_string() + components[0] + "/",
                    )?);
                } else {
                    let parent = key.parent();
                    if parent.eq(prefix) {
                        keys.push(key.clone());
                    }
                }
            }
        }
        let prefixes: Vec<StorePrefix> = prefixes.iter().cloned().collect();
        Ok(StoreKeysPrefixes { keys, prefixes })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;

    #[test]
    fn memory() -> Result<(), Box<dyn Error>> {
        let store = MemoryStore::new();
        super::super::test_util::store_write(&store)?;
        super::super::test_util::store_read(&store)?;
        super::super::test_util::store_list(&store)?;
        Ok(())
    }
}
