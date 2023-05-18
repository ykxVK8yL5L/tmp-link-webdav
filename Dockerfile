FROM alpine:latest
ARG TARGETARCH
ARG TARGETVARIANT
RUN apk --no-cache add ca-certificates tini
RUN apk add tzdata && \
	cp /usr/share/zoneinfo/Asia/Shanghai /etc/localtime && \
	echo "Asia/Shanghai" > /etc/timezone && \
	apk del tzdata

RUN mkdir -p /etc/tmp-link-webdav
WORKDIR /root/
ADD tmp-link-webdav-$TARGETARCH$TARGETVARIANT /usr/bin/tmp-link-webdav

ENTRYPOINT ["/sbin/tini", "--"]
CMD ["/usr/bin/tmp-link-webdav", "--auto-index", "--workdir", "/etc/tmp-link-webdav"]
