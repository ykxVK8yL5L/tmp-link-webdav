# tmp.link的webdav服务 目前支持列文件及通过curl上传，需要上传16m以上文件【技术上可以全部支持，但自用，无所吊谓】  

# 使用方法 【token可通过tmp.link后台获取】
1、命令行
```
tmp-link-webdav --tmp-link-token='XXXXXXXXXXXXX'
```
2、Dokcer【推荐使用】
```
docker run  --name="tmp-link-webdav" -p 10018:9867 -e TMP_LINK_TOKEN="XXXXXXXXXXXXX" ykxvk8yl5l/tmp-link-webdav:latest
```

文件上传命令:
```
curl -T "文件路径" "http://IP:PORT/" 
```
