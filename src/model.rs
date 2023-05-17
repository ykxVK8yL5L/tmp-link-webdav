use std::ops;
use ::time::{format_description::well_known::Rfc3339, OffsetDateTime};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Utc};


use anyhow::{bail, Context, Result};
use bytes::Bytes;
use futures_util::future::FutureExt;
use reqwest::{
    header::{HeaderMap, HeaderValue},
    StatusCode,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use tokio::{
    sync::{oneshot, RwLock},
    time,
};
use tracing::{debug, error, info, warn};
use dav_server::fs::{DavDirEntry, DavMetaData, FsFuture, FsResult};
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct Credentials {
    pub token: String,
}


#[derive(Debug, Clone, Serialize)]
pub struct CreateFolderRequest<'a> {
    pub kind: &'a str,
    pub name: &'a str,
    pub parent_id: &'a str,
}

#[derive(Debug, Clone, Serialize)]
pub struct DelFileRequest {
    pub ids: Vec<String>,
}


#[derive(Debug, Clone, Serialize)]
pub struct MoveFileRequest {
    pub ids: Vec<String>,
    pub to: MoveTo,
}


#[derive(Debug, Clone, Serialize)]
pub struct MoveTo {
    pub parent_id: String,
}

mod my_date_format {
    use chrono::{DateTime, Utc, TimeZone};
    use serde::{self, Deserialize, Serializer, Deserializer};

    const FORMAT: &'static str = "%Y-%m-%d %H:%M:%S";
    pub fn serialize<S>(
        date: &DateTime<Utc>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let s = format!("{}", date.format(FORMAT));
        serializer.serialize_str(&s)
    }

    // The signature of a deserialize_with function must follow the pattern:
    //
    //    fn deserialize<'de, D>(D) -> Result<T, D::Error>
    //    where
    //        D: Deserializer<'de>
    //
    // although it may also be generic over the output types T.
    pub fn deserialize<'de, D>(
        deserializer: D,
    ) -> Result<DateTime<Utc>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Utc.datetime_from_str(&s, FORMAT).map_err(serde::de::Error::custom)
    }
}


#[derive(Debug, Clone, Serialize)]
pub struct RenameFileRequest<'a>{
    pub name: &'a str,
}




#[derive(Debug, Clone, Deserialize)]
pub struct RefreshTokenResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: u64,
    pub token_type: String,
}





#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileType {
    Folder,
    File,
}


#[derive(Debug, Clone,Serialize, Deserialize)]
pub struct QuotaResponse {
    pub kind: String,
    pub expires_at: String,
    pub quota: Quota,
}

#[derive(Debug, Clone,Serialize, Deserialize)]
pub struct Quota {
    pub kind: String,
    pub limit: u64,
    pub usage: u64,
    pub usage_in_trash: u64,
    pub play_times_limit: u64,
    pub play_times_usage:u64,
}

#[derive(Debug, Clone,Serialize, Deserialize)]
pub struct UploadRequest {
    pub kind: String,
    pub name: String,
    pub size: u64,
    pub hash: String,
    pub upload_type: String,
    pub objProvider:ObjProvider,
    pub parent_id: String,
}


#[derive(Debug, Clone,Serialize, Deserialize)]
pub struct ObjProvider {
    pub provider: String,
}

#[derive(Debug, Clone,Serialize, Deserialize)]
pub struct OssArgs {
    pub uploader:String,
    pub utoken:String,
}


#[derive(Debug, Clone,Serialize, Deserialize)]
pub struct CompleteMultipartUpload {
    pub Part: Vec<PartInfo>,
}


#[derive(Debug, Clone,Serialize, Deserialize)]
pub struct CompleteFileUpload {
    pub data: FileUploadInfo,
    pub status:u64,
}


#[derive(Debug, Clone,Serialize, Deserialize)]
pub struct FileUploadInfo {
    pub size: u64,
    pub nums: u64,
}



#[derive(Debug, Clone,Serialize, Deserialize)]
pub struct PartInfo {
    pub status: u64,
    pub data: String,
}

#[derive(Debug, Clone,Serialize, Deserialize)]
pub struct PartNumber {
    pub next: u64,
    pub total: u64,
    pub wait: u64,
    pub uploading: u64,
    pub success: u64,
}

// #[derive(Debug, Serialize, Deserialize)]
// struct Example {
//     next: serde_json::Value,
// }



#[derive(Debug, Clone,Serialize, Deserialize)]
pub struct UploadResponse {
    pub upload_type: String,
    pub resumable: Resumable,
    pub file: WebdavFile,
}



#[derive(Debug, Clone,Serialize, Deserialize)]
pub struct PrepareFileResponse {
    pub data: PrepareInfo,
    pub status: u64,
}



#[derive(Debug, Clone,Serialize, Deserialize)]
pub struct CompleteUploadResponse {
    pub data: String,
    pub status: u64,
}


#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepareInfo {
    pub next: u64,
    pub total: u64,
    pub wait: u64,
    pub uploading: u64,
    pub success: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SliceNextResult {
    Bool(bool),
    Int(i64),
}


#[derive(Debug, Clone,Serialize, Deserialize)]
pub struct FileResponse {
    pub data: String,
    pub status: u64,
}

#[derive(Debug, Clone,Serialize, Deserialize)]
pub struct FileUploadResponse {
    pub data: UploaderResponse,
    pub status: u64,
}

#[derive(Debug, Clone,Serialize, Deserialize)]
pub struct UploaderResponse {
    pub utoken: String,
    pub uploader: String,
    pub src:String,
}








#[derive(Debug, Clone,Serialize, Deserialize)]
pub struct Resumable {
    pub kind: String,
    pub provider: String,
    pub params: UploadParams,
}

#[derive(Debug, Clone,Serialize, Deserialize)]
pub struct UploadParams {
    pub access_key_id: String,
    pub access_key_secret: String,
    pub bucket: String,
    pub endpoint: String,
    pub expiration: String,
    pub key: String,
    pub security_token: String,
}

#[derive(Debug, Deserialize, PartialEq)]
pub struct InitiateMultipartUploadResult {
    pub Bucket: String,
    pub Key: String,
    pub UploadId: String,
}




#[derive(Debug, Clone,Serialize, Deserialize)]
pub struct WebdavFile {
    pub mrid: String,
    pub model: String,
    pub ukey: String,
    pub sha1: String,
    pub fname: String,
    pub fsize: String,
    pub ftype: String,
    #[serde(with = "my_date_format")]
    pub ctime: DateTime<Utc>,
    // pub ltime: String,
    // pub like: String,
    // pub dir_name: Option<String>,
    // pub fsize_formated: String,
    // pub hp: i64,
    // pub hp_time: i64,
    // pub hp_percent: i64,
    // pub lefttime: i64,
    // pub fname_ex: String,
    //pub cctime: DateTime,
    pub sync: i64,
    pub owner: String,
    pub download_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Link {
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Media {
    pub media_name: String,
    pub link:Link,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilesList {
    pub data: Vec<WebdavFile>,
    pub status: i64,
    //pub debug: Vec<Value>,
}





impl DavMetaData for WebdavFile {
    fn len(&self) -> u64 {
        //self.size
        self.fsize.parse::<u64>().unwrap()
    }

    fn modified(&self) -> FsResult<SystemTime> {
        let timestamp = self.ctime.timestamp();
        let system_time = UNIX_EPOCH + std::time::Duration::from_secs(timestamp as u64);
        Ok(system_time)
    }

    fn is_dir(&self) -> bool {
        //matches!(self.kind, String::from("drive#folder") )
        self.ftype.eq("")
    }

    fn created(&self) -> FsResult<SystemTime> {
        let timestamp = self.ctime.timestamp();
        let system_time = UNIX_EPOCH + std::time::Duration::from_secs(timestamp as u64);
        Ok(system_time)
    }
}

impl DavDirEntry for WebdavFile {
    fn name(&self) -> Vec<u8> {
        self.fname.as_bytes().to_vec()
    }

    fn metadata(&self) -> FsFuture<Box<dyn DavMetaData>> {
        async move { Ok(Box::new(self.clone()) as Box<dyn DavMetaData>) }.boxed()
    }
}

impl WebdavFile {
    pub fn new_root() -> Self {
        Self {
            mrid: "0".to_string(),
            model: "1".to_string(),
            ukey: "".to_string(),
            sha1: "".to_string(),
            fname: "".to_string(),
            fsize: "".to_string(),
            ftype: "".to_string(),
            ctime: chrono::offset::Utc::now(),
            // ltime: "".to_string(),
            // like: "".to_string(),
            // dir_name: Some("".to_string()),
            // fsize_formated: "".to_string(),
            // hp: 0,
            // hp_time: 0,
            // hp_percent: 0,
            // lefttime: 0,
            // fname_ex: "".to_string(),
            //cctime: DateTime(now),
            sync: 0,
            owner: "".to_string(),
            download_url:None,
        }
    }
}


