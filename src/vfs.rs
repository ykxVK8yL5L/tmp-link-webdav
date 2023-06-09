use std::collections::HashMap;
use std::fmt::{Debug, Formatter};
use std::io::SeekFrom;
use std::path::{Path, PathBuf};
use std::slice::SliceIndex;
use std::sync::Arc;
use std::time::{Duration,SystemTime, UNIX_EPOCH};
use headers::Range;
use reqwest::multipart::{Form, Part};
use tokio::time::sleep;
use url::form_urlencoded;
use httpdate;
use hmacsha::HmacSha;
use sha1::{Sha1, Digest};
use hex_literal::hex;
use base64::encode;
use std::str::from_utf8;
use anyhow::{Result, Context};
use bytes::{Buf, BufMut, Bytes, BytesMut};
use dashmap::DashMap;
use futures_util::future::{ready, ok, FutureExt};
use tracing::{debug, error, trace,info};
use dav_server::{
    davpath::DavPath,
    fs::{
        DavDirEntry, DavFile, DavFileSystem, DavMetaData, FsError, FsFuture, FsStream, OpenOptions,
        ReadDirMeta,DavProp
    },
};
use moka::future::{Cache as AuthCache};
use tracing_subscriber::fmt::format;
use crate::cache::Cache;
use reqwest::{
    header::{HeaderMap, HeaderValue},
    StatusCode,
};
use tokio::{
    sync::{oneshot, RwLock},
    time,
};
use serde::de::DeserializeOwned;
use serde::{Serialize,Deserialize};
use quick_xml::de::from_str;
use quick_xml::Writer;
use quick_xml::se::Serializer as XmlSerializer;
use serde_json::json;
use reqwest::header::RANGE;

pub use crate::model::*;


const ORIGIN: &str = "https://tmp.link";
const REFERER: &str = "https://tmp.link/?tmpui_page=/app&listview=workspace";
const TMPUPLOADURL:&str = "https://connect.tmp.link/api_v2/cli_uploader";
const TMPFILEURL:&str    = "https://tmp-api.vx-cdn.com/api_v2/file";
const TMPTOKENURL:&str  = "https://tmp-api.vx-cdn.com/api_v2/token";
const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/92.0.4515.131 Safari/537.36";
const UPLOAD_CHUNK_SIZE: u64 = 16 * 1024 * 1024; // 16MB

#[derive(Clone)]
pub struct WebdavDriveFileSystem {
    credentials:Credentials,
    auth_cache:AuthCache<String, String>,
    dir_cache: Cache,
    uploading: Arc<DashMap<String, Vec<WebdavFile>>>,
    root: PathBuf,
    client:reqwest::Client,
    upload_buffer_size: usize,
    skip_upload_same_size: bool,
    prefer_http_download: bool,
}
impl WebdavDriveFileSystem {
    pub async fn new(
        credentials:Credentials,
        root: String,
        cache_size: u64,
        cache_ttl: u64,
        upload_buffer_size: usize,
        skip_upload_same_size: bool,
        prefer_http_download: bool,
    ) -> Result<Self> {
        let dir_cache = Cache::new(cache_size, cache_ttl);
        debug!("dir cache initialized");
        let root = if root.starts_with('/') {
            PathBuf::from(root)
        } else {
            Path::new("/").join(root)
        };

        let mut headers = HeaderMap::new();
        headers.insert("Origin", HeaderValue::from_static(ORIGIN));
        headers.insert("Referer", HeaderValue::from_static(REFERER));
        headers.insert("Host", "tmp-api.vx-cdn.com".parse()?);
        headers.insert("content-type", "application/x-www-form-urlencoded; charset=UTF-8".parse()?);
        headers.insert("user-agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/113.0.0.0 Safari/537.36".parse()?);
        headers.insert("Cookie", format!("PHPSESSID={}",credentials.token).parse()?);
        //headers.insert("Referer", HeaderValue::from_static(REFERER));
        let client = reqwest::Client::builder()
            .user_agent(UA)
            .default_headers(headers)
            .pool_idle_timeout(Duration::from_secs(50))
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()?;
        let auth_cache = AuthCache::new(2);

        let driver = Self {
            credentials,
            auth_cache,
            dir_cache,
            uploading: Arc::new(DashMap::new()),
            root,
            client,
            upload_buffer_size,
            skip_upload_same_size,
            prefer_http_download,
        };
        driver.dir_cache.invalidate_all();
        Ok(driver)
    }

 
   
    async fn post_request<T, U>(&self, url: String, req: &T) -> Result<Option<U>>
    where
        T: Serialize + ?Sized,
        U: DeserializeOwned,
    {
        let url = reqwest::Url::parse(&url)?;
        let res = self
            .client
            .post(url.clone())
            .form(&req)
            .send()
            .await?
            .error_for_status();
        match res {
            Ok(res) => {
                if res.status() == StatusCode::NO_CONTENT {
                    return Ok(None);
                }
                let res = res.json::<U>().await?;
                Ok(Some(res))
            }
            Err(err) => {
                Err(err.into())
            }
        }
    }



    async fn create_folder(&self, parent_id:&str, folder_name: &str) -> Result<WebdavFile> {
        let root = WebdavFile::new_root();
        Ok(root)
    }

    pub async fn remove_file(&self, file_id: &str) -> Result<()> {
        let mut params = HashMap::new();
        params.insert("action", "remove_from_workspace");
        params.insert("token", &self.credentials.token);
        params.insert("ukey", &file_id);
        let delRes:FileResponse = match  self.post_request(TMPFILEURL.to_string(),&params).await{
            Ok(res)=>res.unwrap(),
            Err(err)=>{
                panic!("删除文件失败: {:?}", err)
                //return Err(err);
            }
        };
        Ok(())
    }

    pub async fn rename_file(&self, file_id: &str, new_name: &str) -> Result<()> {
        let mut params = HashMap::new();
        params.insert("action", "rename");
        params.insert("token", &self.credentials.token);
        params.insert("ukey", &file_id);
        params.insert("name", new_name);
        let delRes:FileResponse = match  self.post_request(TMPFILEURL.to_string(),&params).await{
            Ok(res)=>res.unwrap(),
            Err(err)=>{
                panic!("重命名文件失败: {:?}", err)
                //return Err(err);
            }
        };
        Ok(())
    }


    pub async fn move_file(&self, file_id: &str, new_parent_id: &str) -> Result<()> {
        Ok(())
    }

    pub async fn copy_file(&self, file_id: &str, new_parent_id: &str) -> Result<()> {
        Ok(())
    }

    pub async fn get_useage_quota(&self) -> Result<(u64, u64)> {
        Ok((1024, 1000000000000))
    }

    async fn list_files_and_cache( &self, path_str: String, parent_file_id: String)-> Result<Vec<WebdavFile>>{
        info!(path = %path_str, parent_ukey=%parent_file_id,"cache dir");
        let mut file_list = Vec::new();
        let mut params = HashMap::new();
        params.insert("action", "total");
        params.insert("token", &self.credentials.token);
        let fileListResponse:FileListResponse = match  self.post_request(TMPFILEURL.to_string(),&params).await{
            Ok(res)=>res.unwrap(),
            Err(err)=>{
                error!("文件信息获取错误: {:?}", err);
                //return Err(err);
                FileListResponse{
                    data:FileTotal { size: "100".to_string(), nums: "50".to_string() },
                    status:0,
                }
            }
        };
        let page: f64 = 50 as f64;
        let total_page = (fileListResponse.data.nums.parse::<f64>().unwrap()/page).ceil() as u64;
        println!("文件共有{}页",&total_page);
        for current_page in 0..total_page{
            println!("请求文件列表{}",&current_page);
            let current_page_string = format!("{}",current_page);
            let mut file_params: HashMap<&str, &str> = HashMap::new();
            file_params.insert("token", &self.credentials.token);
            file_params.remove("action");
            file_params.insert("action", "workspace_filelist_page");
            file_params.insert("page", current_page_string.as_str());
            file_params.insert("sort_type", "0");
            file_params.insert("sort_by", "0");
            file_params.insert("photo", "0");
            file_params.insert("search", "");
            let filelist:FilesList = match  self.post_request(TMPFILEURL.to_string(),&file_params).await{
                Ok(res)=>res.unwrap(),
                Err(err)=>{
                    error!("文件列表获取错误: {:?}", err);
                    //return Err(err);
                    FilesList{
                        data:vec![],
                        status:0,
                    }
                }
            };
            file_list.extend(filelist.data);
        }
        self.cache_dir(path_str,file_list.clone()).await;
        Ok(file_list)

    }

    async fn cache_dir(&self, dir_path: String, files: Vec<WebdavFile>) {
        trace!(path = %dir_path, count = files.len(), "cache dir");
        self.dir_cache.insert(dir_path, files).await;
    }

    fn find_in_cache(&self, path: &Path) -> Result<Option<WebdavFile>, FsError> {
        if let Some(parent) = path.parent() {
            let parent_str = parent.to_string_lossy().into_owned();
            let file_name = path
                .file_name()
                .ok_or(FsError::NotFound)?
                .to_string_lossy()
                .into_owned();
            let file = self.dir_cache.get(&parent_str).and_then(|files| {
                for file in &files {
                    if file.fname == file_name {
                        return Some(file.clone());
                    }
                }
                None
            });
            Ok(file)
        } else {
            let root = WebdavFile::new_root();
            Ok(Some(root))
        }
    }


    fn find_file_in_cache(&self, parent_path: &Path,file_id:&str) -> Result<Option<WebdavFile>, FsError> {
        let parent_str = parent_path.to_string_lossy().into_owned();
        let file = self.dir_cache.get(&parent_str).and_then(|files| {
            for file in &files {
                if file.ukey == file_id {
                    return Some(file.clone());
                }
            }
            None
        });
        Ok(file)
    }

    

   
  
    async fn read_dir_and_cache(&self, path: PathBuf) -> Result<Vec<WebdavFile>, FsError> {
        let path_str = path.to_string_lossy().into_owned();
        debug!(path = %path_str, "read_dir and cache");
        let parent_file_id = if path_str == "/" {
            "".to_string()
        } else {
            match self.find_in_cache(&path) {
                Ok(Some(file)) => file.ukey,
                _ => {
                    if let Ok(Some(file)) = self.get_by_path(&path_str).await {
                        file.ukey
                    } else {
                        return Err(FsError::NotFound);
                    }
                }
            }
        };
        let mut files = if let Some(files) = self.dir_cache.get(&path_str) {
            files
        } else {
            self.list_files_and_cache(path_str, parent_file_id.clone()).await.map_err(|_| FsError::NotFound)?
        };

        let uploading_files = self.list_uploading_files(&parent_file_id);
        if !uploading_files.is_empty() {
            debug!("added {} uploading files", uploading_files.len());
            files.extend(uploading_files);
        }

        Ok(files)
    }


    fn list_uploading_files(&self, parent_file_id: &str) -> Vec<WebdavFile> {
        self.uploading
            .get(parent_file_id)
            .map(|val_ref| val_ref.value().clone())
            .unwrap_or_default()
    }


    fn remove_uploading_file(&self, parent_file_id: &str, name: &str) {
        if let Some(mut files) = self.uploading.get_mut(parent_file_id) {
            if let Some(index) = files.iter().position(|x| x.fname == name) {
                files.swap_remove(index);
            }
        }
    }

    pub async fn get_by_path(&self, path: &str) -> Result<Option<WebdavFile>> {
        debug!(path = %path, "get file by path");
        if path == "/" || path.is_empty() {
            return Ok(Some(WebdavFile::new_root()));
        }
        let tpath = PathBuf::from(path);
        let path_str = tpath.to_string_lossy().into_owned();
        let file = self.find_in_cache(&tpath)?;
        if let Some(file) = file {
            Ok(Some(file))
        } else {
            let parts: Vec<&str> = path_str.split('/').collect();
            let parts_len = parts.len();
            let filename = parts[parts_len - 1];
            let mut prefix = PathBuf::from("/");
            for part in &parts[0..parts_len - 1] {
                let parent = prefix.join(part);
                prefix = parent.clone();
                let files = self.dir_cache.get(&parent.to_string_lossy().into_owned()).unwrap();
                if let Some(file) = files.iter().find(|f| &f.fname == filename) {
                    return Ok(Some(file.clone()));
                }
            }
            Ok(Some(WebdavFile::new_root()))
        }
    
    }


    async fn get_file(&self, path: PathBuf) -> Result<Option<WebdavFile>, FsError> {

        let path_str = path.to_string_lossy().into_owned();
        debug!(path = %path_str, "get_file");

        // let pos = path_str.rfind('/').unwrap();
        // let path_length = path_str.len()-pos;
        // let path_name: String = path_str.chars().skip(pos+1).take(path_length).collect();

        let parts: Vec<&str> = path_str.split('/').collect();
        let parts_len = parts.len();
        let path_name = parts[parts_len - 1];

        // 忽略 macOS 上的一些特殊文件
        if path_name == ".DS_Store" || path_name.starts_with("._") {
            return Err(FsError::NotFound);
        }

        let file = self.find_in_cache(&path)?;
        if let Some(file) = file {
            trace!(path = %path.display(), file_id = %file.ukey, "file found in cache");
            Ok(Some(file))
        } else {

            debug!(path = %path.display(), "file not found in cache");
            // trace!(path = %path.display(), "file not found in cache");
            // if let Ok(Some(file)) = self.get_by_path(&path_str).await {
            //     return Ok(Some(file));
            // }
            let parts: Vec<&str> = path_str.split('/').collect();
            let parts_len = parts.len();
            let filename = parts[parts_len - 1];
            let mut prefix = PathBuf::from("/");
            for part in &parts[0..parts_len - 1] {
                let parent = prefix.join(part);
                prefix = parent.clone();
                let files = self.read_dir_and_cache(parent).await?;
                if let Some(file) = files.iter().find(|f| f.fname == filename) {
                    trace!(path = %path.display(), file_id = %file.ukey, "file found in cache");
                    return Ok(Some(file.clone()));
                }
            }
            Ok(None)
        }

    }

    async fn get_download_url(&self,parent_dir:&PathBuf,file_id: &str) -> Result<String> {
        debug!("get_download_url from request");
        //需要修改 第一次的时候download_url为None去请求，成功后缓存，不为None的话判断是否过期如果过期则请求不过期则从缓存读取
        //需要修改缓存的方法
        let davfile = self.find_file_in_cache(parent_dir, file_id).unwrap().unwrap();
        match davfile.download_url {
            Some(u)=>{
                if (!is_url_expired(&u)) {
                    return Ok(u);
                }
            },
            None=>{
                debug!("下载地址为空，开始请求新下载地址");
            }
        }
        //第一步获取下载的Token
        let mut params = HashMap::new();
        params.insert("action", "challenge");
        params.insert("token", &self.credentials.token);

        let res:FileResponse = match  self.post_request(TMPTOKENURL.to_string(),&params).await{
            Ok(res)=>res.unwrap(),
            Err(err)=>{
                panic!("下载Token获取失败: {:?}", err)
                //return Err(err);
            }
        };
        let detail_token = res.data;
        //第二步获取下载地址
        params.remove("action");
        params.insert("action", "download_req");
        params.insert("ukey", file_id);
        params.insert("captcha", &detail_token);

        let furl:FileResponse = match  self.post_request(TMPFILEURL.to_string(),&params).await{
            Ok(res)=>res.unwrap(),
            Err(err)=>{
                panic!("下载地二获取失败: {:?}", err)
                //return Err(err);
            }
        };
        let current_ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards")
        .as_secs();
        //三小时后过期
        let expiretime = current_ts+10800;
        let file_url = format!("{}?x-oss-expires={}",furl.data,expiretime);
        let parent_str = parent_dir.to_string_lossy().into_owned();
        self.dir_cache.set_download_url(parent_str, file_id.to_string(), file_url.clone()).await;
        Ok(file_url)
    }


    pub async fn download(&self, url: &str, start_pos: u64, size: usize) -> Result<Bytes> {
        let end_pos = start_pos + size as u64 - 1;
        debug!(url = %url, start = start_pos, end = end_pos, "download file");
        let range = format!("bytes={}-{}", start_pos, end_pos);
        let res = self.client
            .get(url)
            .header(RANGE, range)
            .timeout(Duration::from_secs(120))
            .send()
            .await?
            .error_for_status()?;
        Ok(res.bytes().await?)

    }


    pub async fn create_file_with_proof(&self,name: &str, parent_file_id: &str, hash:&str, size: u64,chunk_count: u64) ->  Result<FileUploadResponse> {
        debug!(size = size, chunk_count = chunk_count,"create_file_with_proof");
        let sizeStr=size.to_string(); 
        let mut params = HashMap::new();
        params.insert("sha1", hash);
        params.insert("filename", &name);
        params.insert("filesize", &sizeStr);
        params.insert("model", "2");
        params.insert("mr_id", "0");
        params.insert("skip_upload", "0");
        params.insert("action", "prepare_v4");
        params.insert("token", &self.credentials.token);

        let fileRes:FileResponse = match  self.post_request(TMPFILEURL.to_string(),&params).await{
            Ok(res)=>res.unwrap(),
            Err(err)=>{
                debug!("请求上传失败: {:?},并不影响什么", err);
                FileResponse{
                    data:"结果为flase而且没有用".to_string(),
                    status:1
                }
                //return Err(err);
            }
        };
        params.clear();
        params.insert("action", "challenge");

        let captchaRes:FileResponse = match  self.post_request(TMPTOKENURL.to_string(),&params).await{
            Ok(res)=>res.unwrap(),
            Err(err)=>{
                panic!("请求验证码失败: {:?}", err)
                //return Err(err);
            }
        };
        let captcha = captchaRes.data;
        debug!("获取的captcha为:{}",captcha);
        params.clear();
        params.insert("action", "upload_request_select2");
        params.insert("token", &self.credentials.token);
        params.insert("filesize", &sizeStr);
        params.insert("captcha", &captcha);

        let fileUploadRes:FileUploadResponse = match  self.post_request(TMPFILEURL.to_string(),&params).await{
            Ok(res)=>res.unwrap(),
            Err(err)=>{
                panic!("请求创建文件失败: {:?}", err)
                //return Err(err);
            }
        };

        debug!("输出创建文件信息开始");
        println!("{:?}",fileUploadRes);
        debug!("输出创建文件信息结束");
        
        Ok(fileUploadRes)

    }


    pub async fn get_pre_upload_info(&self,oss_args:&OssArgs) -> Result<String> {
        Ok(oss_args.utoken.clone())
    }

    pub async fn upload_chunk(&self, file:&WebdavFile, oss_args:&OssArgs, upload_id:&str, current_chunk:u64,body: Bytes) -> Result<(PartInfo)> {
        debug!(file_name=%file.fname,upload_id = upload_id,current_chunk=current_chunk, "upload_chunk");

        let uploader_url = format!("{}/app/upload_slice",oss_args.uploader);
        let slice_size = format!("{}",&self.upload_buffer_size);


        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("authority", oss_args.uploader.parse()?);
        headers.insert("accept", "application/json, text/javascript, */*; q=0.01".parse()?);
        headers.insert("accept-language", "zh-CN,zh;q=0.9,en;q=0.8".parse()?);
        headers.insert("content-type", "application/x-www-form-urlencoded; charset=UTF-8".parse()?);
        headers.insert("origin", "https://tmp.link".parse()?);
        headers.insert("referer", "https://tmp.link/".parse()?);
        headers.insert("sec-ch-ua", "\"Google Chrome\";v=\"113\", \"Chromium\";v=\"113\", \"Not-A.Brand\";v=\"24\"".parse()?);
        headers.insert("sec-ch-ua-mobile", "?0".parse()?);
        headers.insert("sec-ch-ua-platform", "\"macOS\"".parse()?);
        headers.insert("sec-fetch-dest", "empty".parse()?);
        headers.insert("sec-fetch-mode", "cors".parse()?);
        headers.insert("sec-fetch-site", "cross-site".parse()?);
        headers.insert("user-agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/113.0.0.0 Safari/537.36".parse()?);
        let client = reqwest::Client::builder()
        .default_headers(headers)
        .pool_idle_timeout(Duration::from_secs(50))
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()?;


        let uptoken_str = format!("{}{}{}",self.credentials.uid,file.fname,file.fsize);
        let mut hasher = Sha1::default();
        hasher.update(uptoken_str.as_bytes());
        let hash_code = hasher.finalize();
        let uptoken = format!("{:X}",&hash_code);


        let mut params = HashMap::new();
        params.insert("action", "prepare");
        params.insert("uptoken", &uptoken);
        params.insert("utoken", &oss_args.utoken);
        params.insert("sha1", &file.sha1);
        params.insert("token", &self.credentials.token);
        params.insert("filename", &file.fname);
        params.insert("filesize", &file.fsize);
        params.insert("slice_size", &slice_size);
        params.insert("mr_id", "0");
        params.insert("model", "2");

        let response = client
            .post(uploader_url.clone())
            .form(&params)
            .send()
            .await?;
        let pbody = response.text().await.unwrap();
        debug!(body = pbody);
        let prepareInfo:PrepareFileResponse = serde_json::from_str(&pbody).unwrap();

        debug!(uploadurl=&uploader_url,"分片上传网址:");
        let mut params = HashMap::new();
        params.insert("action", "challenge");
        params.insert("token", &self.credentials.token);
        let res:FileResponse = match  self.post_request(TMPTOKENURL.to_string(),&params).await{
            Ok(res)=>res.unwrap(),
            Err(err)=>{
                panic!("下载Token获取失败: {:?}", err)
                //return Err(err);
            }
        };


        let upload_file = file.clone();
        let captcha_token = res.data;
        let upload_index_str = format!("{}",prepareInfo.data.next);
        let slice_size = format!("{}",&self.upload_buffer_size);
        let file_sha1 = upload_file.sha1;


        let upload_client = reqwest::Client::builder()
        .pool_idle_timeout(Duration::from_secs(50))
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()?;


        //需要先option请求下



        let option_response = upload_client
            .request(reqwest::Method::OPTIONS, &uploader_url)
            .send()
            .await?;


        let formfiledata: Part = Part::bytes(body.to_vec()).file_name("slice");
        let form = reqwest::multipart::Form::new()
            .text("uptoken", uptoken.clone())
            .text("sha1", file_sha1)
            .text("index", upload_index_str)
            .text("action", "upload_slice")
            .text("slice_size", slice_size)
            .text("captcha", captcha_token)
            .part("filedata",formfiledata);
            

        let response = upload_client
            .post(&uploader_url)
            .multipart(form)
            .send()
            .await?;

        let body = &response.text().await?;
        let parininfo: PartInfo = match serde_json::from_str(body) {
            Ok(p)=>{
                p
            },
            Err(err)=>{
                    panic!("error")
            }
        };

        Ok(parininfo)
    }


    pub async fn complete_upload(&self,file:&WebdavFile, upload_tags:String, oss_args:&OssArgs, upload_id:&str)-> Result<()> {
        let uploader_url = format!("{}/app/upload_slice",oss_args.uploader);
        let slice_size = format!("{}",&self.upload_buffer_size);
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("authority", oss_args.uploader.parse()?);
        headers.insert("accept", "application/json, text/javascript, */*; q=0.01".parse()?);
        headers.insert("accept-language", "zh-CN,zh;q=0.9,en;q=0.8".parse()?);
        headers.insert("content-type", "application/x-www-form-urlencoded; charset=UTF-8".parse()?);
        headers.insert("origin", "https://tmp.link".parse()?);
        headers.insert("referer", "https://tmp.link/".parse()?);
        headers.insert("sec-ch-ua", "\"Google Chrome\";v=\"113\", \"Chromium\";v=\"113\", \"Not-A.Brand\";v=\"24\"".parse()?);
        headers.insert("sec-ch-ua-mobile", "?0".parse()?);
        headers.insert("sec-ch-ua-platform", "\"macOS\"".parse()?);
        headers.insert("sec-fetch-dest", "empty".parse()?);
        headers.insert("sec-fetch-mode", "cors".parse()?);
        headers.insert("sec-fetch-site", "cross-site".parse()?);
        headers.insert("user-agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/113.0.0.0 Safari/537.36".parse()?);
        let client = reqwest::Client::builder()
        .default_headers(headers)
        .pool_idle_timeout(Duration::from_secs(50))
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()?;


        let uptoken_str = format!("{}{}{}",self.credentials.uid,file.fname,file.fsize);
        let mut hasher = Sha1::default();
        hasher.update(uptoken_str.as_bytes());
        let hash_code = hasher.finalize();
        let uptoken = format!("{:X}",&hash_code);


        let mut params = HashMap::new();
        params.insert("action", "prepare");
        params.insert("utoken", &oss_args.utoken);
        params.insert("uptoken", &uptoken);
        params.insert("sha1", &file.sha1);
        params.insert("token", &self.credentials.token);
        params.insert("filename", &file.fname);
        params.insert("filesize", &file.fsize);
        params.insert("slice_size", &slice_size);
        params.insert("mr_id", "0");
        params.insert("model", "2");

        let response = client
            .post(uploader_url.clone())
            .form(&params)
            .send()
            .await?;
        let body = response.text().await.unwrap();
        debug!(body = body);
        let completeInfo:CompleteUploadResponse = serde_json::from_str(&body).unwrap();

        debug!("输出获取到的最终上传完成信息开始");
        println!("上传后的文件地址为:https://tmp.link/f/{}",completeInfo.data);
        debug!("输出获取到的最终上传完成信息结束");

        Ok(())
    }


    pub fn hmac_authorization(&self, req:&reqwest::Request,time:&str,oss_args:&OssArgs)->String{
        "hello".to_string()
    }
   

    fn normalize_dav_path(&self, dav_path: &DavPath) -> PathBuf {
        let path = dav_path.as_pathbuf();
        if self.root.parent().is_none() || path.starts_with(&self.root) {
            return path;
        }
        let rel_path = dav_path.as_rel_ospath();
        if rel_path == Path::new("") {
            return self.root.clone();
        }
        self.root.join(rel_path)
    }
}

impl DavFileSystem for WebdavDriveFileSystem {
    fn open<'a>(
        &'a self,
        dav_path: &'a DavPath,
        options: OpenOptions,
    ) -> FsFuture<Box<dyn DavFile>> {
        let path = self.normalize_dav_path(dav_path);
        let mode = if options.write { "write" } else { "read" };
        debug!(path = %path.display(), mode = %mode, "fs: open");
        async move {
            if options.append {
                // Can't support open in write-append mode
                error!(path = %path.display(), "unsupported write-append mode");
                return Err(FsError::NotImplemented);
            }
            let parent_path = path.parent().ok_or(FsError::NotFound)?;
            let parent_file = self
                .get_file(parent_path.to_path_buf())
                .await?
                .ok_or(FsError::NotFound)?;
            let sha1 = options.checksum.and_then(|c| {
                if let Some((algo, hash)) = c.split_once(':') {
                    if algo.eq_ignore_ascii_case("sha1") {
                        Some(hash.to_string())
                    } else {
                        None
                    }
                } else {
                    None
                }
            });


            let dav_file = if let Some(mut file) = self.get_file(path.clone()).await? {
                if options.write && options.create_new {
                    return Err(FsError::Exists);
                }
                AliyunDavFile::new(self.clone(), file, parent_file.ukey,parent_path.to_path_buf(),options.size.unwrap_or_default(),sha1)
            } else if options.write && (options.create || options.create_new) {


                let size = options.size;
                let name = dav_path
                    .file_name()
                    .ok_or(FsError::GeneralFailure)?
                    .to_string();

                // 忽略 macOS 上的一些特殊文件
                if name == ".DS_Store" || name.starts_with("._") {
                    return Err(FsError::NotFound);
                }
                let now = SystemTime::now();

                let file_path = dav_path.as_url_string();
                let mut hasher = Sha1::default();
                hasher.update(file_path.as_bytes());
                let hash_code = hasher.finalize();
                let file_hash = format!("{:X}",&hash_code);
                let parent_folder_id = parent_file.ukey.clone();

                let file = WebdavFile {
                    mrid: "0".to_string(),
                    model: "2".to_string(),
                    ukey: "".to_string(),
                    sha1: file_hash,
                    fname: name,
                    fsize: size.unwrap_or(0).to_string(),
                    ftype: "".to_string(),
                    ctime: chrono::offset::Utc::now(),
                    // ltime: "".to_string(),
                    // like: "".to_string(),
                    // dir_name:Some("".to_string()),
                    // fsize_formated: "".to_string(),
                    // hp: 0,
                    // hp_time: 0,
                    // hp_percent: 0,
                    // lefttime: 0,
                    // fname_ex: "".to_string(),
                    //cctime: DateTime::new(now),
                    sync: 0,
                    owner: "".to_string(),
                    download_url:None,
                };
                let mut uploading = self.uploading.entry(parent_file.ukey.clone()).or_default();
                uploading.push(file.clone());

                AliyunDavFile::new(self.clone(), file, parent_file.ukey,parent_path.to_path_buf(),size.unwrap_or(0),sha1)
            } else {
                println!("FsError::NotFound");
                return Err(FsError::NotFound);
            };
            Ok(Box::new(dav_file) as Box<dyn DavFile>)
        }
        .boxed()
    }

    fn read_dir<'a>(
        &'a  self,
        path: &'a DavPath,
        _meta: ReadDirMeta,
    ) -> FsFuture<FsStream<Box<dyn DavDirEntry>>> {
        let path = self.normalize_dav_path(path);
        debug!(path = %path.display(), "fs: read_dir");
        async move {
            let files = self.read_dir_and_cache(path.clone()).await?;
            let mut v: Vec<Box<dyn DavDirEntry>> = Vec::with_capacity(files.len());
            for file in files {
                v.push(Box::new(file));
            }
            let stream = futures_util::stream::iter(v);
            Ok(Box::pin(stream) as FsStream<Box<dyn DavDirEntry>>)
        }
        .boxed()
    }

   


    fn create_dir<'a>(&'a self, dav_path: &'a DavPath) -> FsFuture<()> {
        let path = self.normalize_dav_path(dav_path);
        async move {
            let parent_path = path.parent().ok_or(FsError::NotFound)?;
            let parent_file = self
                .get_file(parent_path.to_path_buf())
                .await?
                .ok_or(FsError::NotFound)?;
            
            if !(parent_file.ftype.eq("")) {
                return Err(FsError::Forbidden);
            }
            if let Some(name) = path.file_name() {
                let name = name.to_string_lossy().into_owned();
                self.create_folder(&parent_file.ukey,&name).await;
                self.dir_cache.invalidate(parent_path).await;
                Ok(())
            } else {
                Err(FsError::Forbidden)
            }
        }
        .boxed()
    }


    fn remove_dir<'a>(&'a self, dav_path: &'a DavPath) -> FsFuture<()> {
        let path = self.normalize_dav_path(dav_path);
        debug!(path = %path.display(), "fs: remove_dir");
        async move {

            let file = self
                .get_file(path.clone())
                .await?
                .ok_or(FsError::NotFound)?;

            if !(file.ftype.eq("")) {
                return Err(FsError::Forbidden);
            }

            self.remove_file(&file.ukey)
                .await
                .map_err(|err| {
                    error!(path = %path.display(), error = %err, "remove directory failed");
                    FsError::GeneralFailure
                })?;
            self.dir_cache.invalidate(&path).await;
            self.dir_cache.invalidate_parent(&path).await;
            Ok(())
        }
        .boxed()
    }


    fn remove_file<'a>(&'a self, dav_path: &'a DavPath) -> FsFuture<()> {
        let path = self.normalize_dav_path(dav_path);
        debug!(path = %path.display(), "fs: remove_file");
        async move {
            let file = self
                .get_file(path.clone())
                .await?
                .ok_or(FsError::NotFound)?;

            self.remove_file(&file.ukey)
                .await
                .map_err(|err| {
                    error!(path = %path.display(), error = %err, "remove file failed");
                    FsError::GeneralFailure
                })?;
            self.dir_cache.invalidate_parent(&path).await;
            Ok(())
        }
        .boxed()
    }


    fn rename<'a>(&'a self, from_dav: &'a DavPath, to_dav: &'a DavPath) -> FsFuture<()> {
        let from = self.normalize_dav_path(from_dav);
        let to = self.normalize_dav_path(to_dav);
        debug!(from = %from.display(), to = %to.display(), "fs: rename");
        async move {
            let is_dir;
            if from.parent() == to.parent() {
                // rename
                if let Some(name) = to.file_name() {
                    let file = self
                        .get_file(from.clone())
                        .await?
                        .ok_or(FsError::NotFound)?;
                    is_dir = if file.ftype == "" {
                        true
                    } else {
                        false
                    };
                    let name = name.to_string_lossy().into_owned();
                    self.rename_file(&file.ukey, &name).await;
                } else {
                    return Err(FsError::Forbidden);
                }
            } else {
                // move
                let file = self
                    .get_file(from.clone())
                    .await?
                    .ok_or(FsError::NotFound)?;
                is_dir = if file.ftype == "" {
                    true
                } else {
                    false
                };
                let to_parent_file = self
                    .get_file(to.parent().unwrap().to_path_buf())
                    .await?
                    .ok_or(FsError::NotFound)?;
                let new_name = to_dav.file_name();
                self.move_file(&file.ukey, &to_parent_file.ukey).await;
            }


            if is_dir {
                self.dir_cache.invalidate(&from).await;
            }
            self.dir_cache.invalidate_parent(&from).await;
            self.dir_cache.invalidate_parent(&to).await;
            Ok(())
        }
        .boxed()
    }


    fn copy<'a>(&'a self, from_dav: &'a DavPath, to_dav: &'a DavPath) -> FsFuture<()> {
        let from = self.normalize_dav_path(from_dav);
        let to = self.normalize_dav_path(to_dav);
        debug!(from = %from.display(), to = %to.display(), "fs: copy");
        async move {
            let file = self
                .get_file(from.clone())
                .await?
                .ok_or(FsError::NotFound)?;
            let to_parent_file = self
                .get_file(to.parent().unwrap().to_path_buf())
                .await?
                .ok_or(FsError::NotFound)?;
            let new_name = to_dav.file_name();
            self.copy_file(&file.ukey, &to_parent_file.ukey).await;
            self.dir_cache.invalidate(&to).await;
            self.dir_cache.invalidate_parent(&to).await;
            Ok(())
        }
        .boxed()
    }



    fn get_quota(&self) -> FsFuture<(u64, Option<u64>)> {
        async move {
            let (used, total) = self.get_useage_quota().await.map_err(|err| {
                error!(error = %err, "get quota failed");
                FsError::GeneralFailure
            })?;
            Ok((used, Some(total)))
        }
        .boxed()
    }

    fn metadata<'a>(&'a self, path: &'a DavPath) -> FsFuture<Box<dyn DavMetaData>> {
        let path = self.normalize_dav_path(path);
        debug!(path = %path.display(), "fs: metadata");
        async move {
            let file = self.get_file(path).await?.ok_or(FsError::NotFound)?;
            Ok(Box::new(file) as Box<dyn DavMetaData>)
        }
        .boxed()
    }


    fn have_props<'a>(
        &'a self,
        _path: &'a DavPath,
    ) -> std::pin::Pin<Box<dyn futures_util::Future<Output = bool> + Send + 'a>> {
        Box::pin(ready(true))
    }

    fn get_prop(&self, dav_path: &DavPath, prop:DavProp) -> FsFuture<Vec<u8>> {
        let path = self.normalize_dav_path(dav_path);
        let prop_name = match prop.prefix.as_ref() {
            Some(prefix) => format!("{}:{}", prefix, prop.name),
            None => prop.name.to_string(),
        };
        debug!(path = %path.display(), prop = %prop_name, "fs: get_prop");
        async move {
            if prop.namespace.as_deref() == Some("http://owncloud.org/ns")
                && prop.name == "checksums"
            {
                let file = self.get_file(path).await?.ok_or(FsError::NotFound)?;
                if let sha1 = file.sha1 {
                    let xml = format!(
                        r#"<?xml version="1.0"?>
                        <oc:checksums xmlns:d="DAV:" xmlns:nc="http://nextcloud.org/ns" xmlns:oc="http://owncloud.org/ns">
                            <oc:checksum>sha1:{}</oc:checksum>
                        </oc:checksums>
                    "#,
                        sha1
                    );
                    return Ok(xml.into_bytes());
                }
            }
            Err(FsError::NotImplemented)
        }
        .boxed()
    }





}

#[derive(Debug, Clone)]
struct UploadState {
    size: u64,
    buffer: BytesMut,
    chunk_count: u64,
    chunk: u64,
    upload_id: String,
    oss_args: Option<OssArgs>,
    sha1: Option<String>,
    upload_tags:CompleteMultipartUpload,
}

impl Default for UploadState {
    fn default() -> Self {
        let mut upload_tags = CompleteMultipartUpload{Part:vec![]};
        Self {
            size: 0,
            buffer: BytesMut::new(),
            chunk_count: 0,
            chunk: 1,
            upload_id: String::new(),
            oss_args: None,
            sha1: None,
            upload_tags: upload_tags,
        }
    }
}

#[derive(Clone)]
struct AliyunDavFile {
    fs: WebdavDriveFileSystem,
    file: WebdavFile,
    parent_file_id: String,
    parent_dir: PathBuf,
    current_pos: u64,
    download_url: Option<String>,
    upload_state: UploadState,
}

impl Debug for AliyunDavFile {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AliyunDavFile")
            .field("file", &self.file)
            .field("parent_file_id", &self.parent_file_id)
            .field("current_pos", &self.current_pos)
            .field("upload_state", &self.upload_state)
            .finish()
    }
}

impl AliyunDavFile {
    fn new(fs: WebdavDriveFileSystem, file: WebdavFile, parent_file_id: String,parent_dir: PathBuf,size: u64,sha1: Option<String>,) -> Self {
        Self {
            fs,
            file,
            parent_file_id,
            parent_dir,
            current_pos: 0,
            upload_state: UploadState {
                size,
                sha1,
                ..Default::default()
            },
            download_url: None,
        }
    }

    async fn get_download_url(&self,parent_dir: &PathBuf) -> Result<String, FsError> {
        debug!("get_download_url from cache or request");
        match &self.download_url {
            None=> { 
                debug!("下载地址为NONE第一次请求");
                self.fs.get_download_url(parent_dir,&self.file.ukey).await.map_err(|err| {
                    error!(file_id = %self.file.ukey, file_name = %self.file.fname, error = %err, "get download url failed");
                    FsError::GeneralFailure
                })
             },
             Some(url) => { 
                debug!(url=%url,"下载地址不为NONE判断是否过期");
                if (is_url_expired(&url)) {
                    debug!(url=%url,"下载地址过期重新请求");
                    self.fs.get_download_url(parent_dir,&self.file.ukey).await.map_err(|err| {
                        error!(file_id = %self.file.ukey, file_name = %self.file.fname, error = %err, "get download url failed");
                        FsError::GeneralFailure
                    })
                }else {
                    debug!(url=%url,"下载地址不过期直接返回");
                    Ok(url.to_string())
                }
             }
        }



        
    }

    async fn prepare_for_upload(&mut self) -> Result<bool, FsError> {
        if self.upload_state.chunk_count == 0 {
            let size = self.upload_state.size;
            debug!(file_name = %self.file.fname, size = size, "prepare for upload");

            if !self.file.ukey.is_empty() {
                if let content_hash = self.file.sha1.to_string() {
                    if let Some(sha1) = self.upload_state.sha1.as_ref() {
                        if content_hash.eq_ignore_ascii_case(sha1) {
                            debug!(file_name = %self.file.fname, sha1 = %sha1, "skip uploading same content hash file");
                            return Ok(false);
                        }
                    }
                }

                if self.fs.skip_upload_same_size && self.file.fsize.parse::<u64>().unwrap() == size {
                    debug!(file_name = %self.file.fname, size = size, "skip uploading same size file");
                    return Ok(false);
                }
                // existing file, delete before upload
                if let Err(err) = self
                    .fs
                    .remove_file(&self.file.ukey)
                    .await
                {
                    error!(file_name = %self.file.fname, error = %err, "delete file before upload failed");
                }
            }
            // TODO: create parent folders?

            let upload_buffer_size = self.fs.upload_buffer_size as u64;
            let chunk_count =
                size / upload_buffer_size + if size % upload_buffer_size != 0 { 1 } else { 0 };
            self.upload_state.chunk_count = chunk_count;
            debug!("uploading {} ({} bytes)...", self.file.fname, size);
            if size>0 {
                let hash = &self.file.clone().sha1;
                let res = self
                    .fs
                    .create_file_with_proof(&self.file.fname, &self.parent_file_id, hash, size, chunk_count)
                    .await;
                let upload_response = match res {
                    Ok(upload_response_info) => upload_response_info,
                    Err(err) => {
                        error!(file_name = %self.file.fname, error = %err, "create file with proof failed");
                        return Ok(false);
                    }
                };


                let oss_args: OssArgs = OssArgs {
                    uploader:upload_response.data.uploader,
                    utoken:upload_response.data.utoken,
                };
                self.upload_state.oss_args = Some(oss_args);
    
                let oss_args = self.upload_state.oss_args.as_ref().unwrap();
                let pre_upload_info = self.fs.get_pre_upload_info(&oss_args).await;
                if let Err(err) = pre_upload_info {
                    error!(file_name = %self.file.fname, error = %err, "get pre upload info failed");
                    return Ok(false);
                }
               
                self.upload_state.upload_id = match pre_upload_info {
                    Ok(upload_id) => upload_id,
                    Err(err) => {
                        error!(file_name = %self.file.fname, error = %err, "get pre upload info failed");
                        return Ok(false);
                    }
                };
                debug!(file_name = %self.file.fname, upload_id = %self.upload_state.upload_id, "pre upload info get upload_id success");
            }
        }
        Ok(true)
    }

    async fn maybe_upload_chunk(&mut self, remaining: bool) -> Result<(), FsError> {
        let chunk_size = if remaining {
            // last chunk size maybe less than upload_buffer_size
            self.upload_state.buffer.remaining()
        } else {
            self.fs.upload_buffer_size
        };
        let current_chunk = self.upload_state.chunk;

        if chunk_size > 0
            && self.upload_state.buffer.remaining() >= chunk_size
            && current_chunk <= self.upload_state.chunk_count
        {
            let chunk_data = self.upload_state.buffer.split_to(chunk_size);
            debug!(
                file_id = %self.file.ukey,
                file_name = %self.file.fname,
                size = self.upload_state.size,
                "upload part {}/{}",
                current_chunk,
                self.upload_state.chunk_count
            );
            let upload_data = chunk_data.freeze();
            let oss_args = match self.upload_state.oss_args.as_ref() {
                Some(oss_args) => oss_args,
                None => {
                    error!(file_name = %self.file.fname, "获取文件上传信息错误");
                    return Err(FsError::GeneralFailure);
                }
            };
            let res = self.fs.upload_chunk(&self.file,oss_args,&self.upload_state.upload_id,current_chunk,upload_data.clone()).await;
            
            let part = match res {
                Ok(part) => part,
                Err(err) => {
                    error!(file_name = %self.file.fname, error = %err, "上传分片失败，无法获取上传信息");
                    return Err(FsError::GeneralFailure);
                }
            };
            println!("文件上传结果:{:?}",part);
            debug!(chunk_count = %self.upload_state.chunk_count, current_chunk=current_chunk, "upload chunk info");
            self.upload_state.upload_tags.Part.push(part);
            if current_chunk == self.upload_state.chunk_count{
                debug!(file_name = %self.file.fname, "upload finished");
                let mut buffer = Vec::new();
                let mut ser = XmlSerializer::with_root(Writer::new_with_indent(&mut buffer, b' ', 4), Some("CompleteMultipartUpload"));
                self.upload_state.upload_tags.serialize(&mut ser).unwrap();
                let upload_tags = String::from_utf8(buffer).unwrap();
                self.fs.complete_upload(&self.file,upload_tags,oss_args,&self.upload_state.upload_id).await;
                self.upload_state = UploadState::default();
                // self.upload_state.buffer.clear();
                // self.upload_state.chunk = 0;
                self.fs.dir_cache.invalidate(&self.parent_dir).await;
                info!("parent dir is  {} parent_file_id is {}", self.parent_dir.to_string_lossy().to_string(), &self.parent_file_id.to_string());
                self.fs.list_files_and_cache(self.parent_dir.to_string_lossy().to_string(), self.parent_file_id.to_string());
            }
            self.upload_state.chunk += 1;
        }
        Ok(())
    }

}

impl DavFile for AliyunDavFile {
    fn metadata(&'_ mut self) -> FsFuture<'_, Box<dyn DavMetaData>> {
        debug!(file_id = %self.file.ukey, file_name = %self.file.fname, "file: metadata");
        async move {
            let file = self.file.clone();
            Ok(Box::new(file) as Box<dyn DavMetaData>)
        }
        .boxed()
    }

    fn write_buf(&'_ mut self, buf: Box<dyn Buf + Send>) -> FsFuture<'_, ()> {
        debug!(file_id = %self.file.ukey, file_name = %self.file.fname, "file: write_buf");
        async move {
            if self.prepare_for_upload().await? {
                self.upload_state.buffer.put(buf);
                self.maybe_upload_chunk(false).await?;
            }
            Ok(())
        }
        .boxed()
    }

    fn write_bytes(&mut self, buf: Bytes) -> FsFuture<()> {
        debug!(file_id = %self.file.ukey, file_name = %self.file.fname, "file: write_bytes");
        async move {
            if self.prepare_for_upload().await? {
                self.upload_state.buffer.extend_from_slice(&buf);
                self.maybe_upload_chunk(false).await?;
            }
            Ok(())
        }
        .boxed()
    }

    fn flush(&mut self) -> FsFuture<()> {
        debug!(file_id = %self.file.ukey, file_name = %self.file.fname, "file: flush");
        async move {
            if self.prepare_for_upload().await? {
                self.maybe_upload_chunk(true).await?;
                self.fs.remove_uploading_file(&self.parent_file_id, &self.file.fname);
                self.fs.dir_cache.invalidate(&self.parent_dir).await;
            }
            Ok(())
        }
        .boxed()
    }

    fn read_bytes(&mut self, count: usize) -> FsFuture<Bytes> {
        debug!(
            file_id = %self.file.ukey,
            file_name = %self.file.fname,
            pos = self.current_pos,
            download_url = self.download_url,
            count = count,
            parent_id = %self.parent_file_id,
            "file: read_bytes",
        );
        async move {
            if self.file.ukey.is_empty() {
                // upload in progress
                return Err(FsError::NotFound);
            }
        
            let download_url = self.download_url.take();
            let download_url = if let Some(mut url) = download_url {
                if is_url_expired(&url) {
                    debug!(url = %url, "下载地址已经过期重新请求");
                    url = self.get_download_url(&self.parent_dir).await?;
                }
                url
            } else {
                debug!("获取文件的下载地址");
                self.get_download_url(&self.parent_dir).await?
            };

            let content = self
                .fs
                .download(&download_url, self.current_pos, count)
                .await
                .map_err(|err| {
                    error!(url = %download_url, error = %err, "download file failed");
                    FsError::NotFound
                })?;
            self.current_pos += content.len() as u64;
            self.download_url = Some(download_url);
            Ok(content)
        }
        .boxed()
    }

    fn seek(&mut self, pos: SeekFrom) -> FsFuture<u64> {
        debug!(
            file_id = %self.file.ukey,
            file_name = %self.file.fname,
            pos = ?pos,
            "file: seek"
        );
        async move {
            let new_pos = match pos {
                SeekFrom::Start(pos) => pos,
                SeekFrom::End(pos) => (self.file.fsize.parse::<u64>().unwrap() as i64 - pos) as u64,
                SeekFrom::Current(size) => self.current_pos + size as u64,
            };
            self.current_pos = new_pos;
            Ok(new_pos)
        }
        .boxed()
    }

   
}

fn is_url_expired(url: &str) -> bool {
    debug!(url=url,"is_url_expired:");
    if let Ok(oss_url) = ::url::Url::parse(url) {
        let expires = oss_url.query_pairs().find_map(|(k, v)| {
            if k == "x-oss-expires" {
                if let Ok(expires) = v.parse::<u64>() {
                    return Some(expires);
                }
            }
            None
        });
        if let Some(expires) = expires {
            let current_ts = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("Time went backwards")
                .as_secs();
            // 预留 1s
            return current_ts >= expires - 1;
        }
    }
    false
}



