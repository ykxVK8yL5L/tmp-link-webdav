# tmp.link有每日上传容量限制 如果上传失败就是达到限制了 一天之内就不要上传了   
# 演示视频:[https://youtu.be/rLXMaLq7vrQ](https://youtu.be/_2UPhxu3Nng)   
[如需使用alist-encrypt需要设置webdav的用户名和密码，详情见命令行]
# tmp.link的webdav服务 目前支持列文件及通过curl上传，需要上传16m以上文件【技术上可以全部支持，但自用，无所吊谓】  

Docker主页: https://hub.docker.com/r/ykxvk8yl5l/tmp-link-webdav   

# 使用方法 【token可通过tmp.link后台获取】
1、命令行
```
tmp-link-webdav --tmp-link-token='XXXXXXXXXXXXX' --auth-user='admin' --auth-password='admin' 
```
2、Dokcer【推荐使用，如不使用alist-encrypt可不设置用户名和密码】
```
docker run  --name="tmp-link-webdav" -p 10018:9867 -e TMP_LINK_TOKEN="XXXXXXXXXXXXX" -e WEBDAV_AUTH_USER="admin" -e WEBDAV_AUTH_PASSWORD="admin" ykxvk8yl5l/tmp-link-webdav:latest
```

文件上传命令:
```
curl -T "文件路径" "http://IP:PORT/" 
```
