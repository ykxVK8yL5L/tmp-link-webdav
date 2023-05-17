use std::path::Path;
use std::time::Duration;

use anyhow::Error;
use moka::future::{Cache as MokaCache, CacheBuilder};
use tracing::{trace,debug};

use crate::model::WebdavFile;

#[derive(Clone)]
pub struct Cache {
    inner: MokaCache<String, Vec<WebdavFile>>,
}

impl Cache {
    pub fn new(max_capacity: u64, ttl: u64) -> Self {
        let inner = CacheBuilder::new(max_capacity)
            .time_to_live(Duration::from_secs(ttl))
            .build();
        Self { inner }
    }

    #[allow(clippy::ptr_arg)]
    pub fn get(&self, key: &String) -> Option<Vec<WebdavFile>> {
        trace!(key = %key, "cache: get");
        self.inner.get(key)
    }

    pub async fn insert(&self, key: String, value: Vec<WebdavFile>) {
        trace!(key = %key, "cache: insert");
        self.inner.insert(key, value).await;
    }

    pub async fn set_download_url(&self,parent_key:String, key: String, download_url:String ) {
        trace!(key = %key, "修改文件的下载地址");
        debug!(download_url = %download_url,"修改文件的下载地址");
        let mut files = self.inner.get(&parent_key).unwrap();
        for element in files.iter_mut() {
            if element.ukey == key {
                element.download_url = Some(download_url);
                break;
            }
        }
        self.inner.insert(parent_key, files).await;
    }


    // pub async fn find_file_in_cache(&self,parent_key:&str, key: &str)-> Option<WebdavFile> {
    //     trace!(key = %key, "find_file_in_cache");
    //     let davfiles = self.inner.get(parent_key).unwrap();
    //     let found = davfiles.iter().find(|p| p.ukey == key);
    //     return found.cloned();
    // }



    pub async fn invalidate(&self, path: &Path) {
        let key = path.to_string_lossy().into_owned();
        debug!(path = %path.display(), key = %key, "cache: invalidate");
        self.inner.invalidate(&key).await;
    }

    pub async fn invalidate_parent(&self, path: &Path) {
        if let Some(parent) = path.parent() {
            self.invalidate(parent).await;
        }
    }


    pub fn invalidate_all(&self) {
        trace!("cache: invalidate all");
        self.inner.invalidate_all();
    }
}
