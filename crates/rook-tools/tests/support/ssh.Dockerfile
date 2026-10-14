FROM alpine:3
RUN apk add --no-cache openssh python3 && mkdir -p /run/sshd && passwd -d root
ENTRYPOINT ["/usr/sbin/sshd", "-D", "-e", "-f", "/fixture/sshd_config"]
LABEL rook.fixture=ssh-20261014
