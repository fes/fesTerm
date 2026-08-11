FROM alpine:3.22

RUN apk add --no-cache openssh \
    && adduser -D -s /bin/ash festerm

COPY sshd_config /etc/ssh/sshd_config
COPY sshd_config.ecdsa-p256 /etc/ssh/sshd_config.ecdsa-p256
COPY entrypoint.sh /usr/local/sbin/festerm-openssh-entrypoint

RUN chmod 700 /usr/local/sbin/festerm-openssh-entrypoint

EXPOSE 22

HEALTHCHECK --interval=1s --timeout=2s --retries=20 \
    CMD netstat -ltn | grep -q ':22 ' || exit 1

ENTRYPOINT ["/usr/local/sbin/festerm-openssh-entrypoint"]
