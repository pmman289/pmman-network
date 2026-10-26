# The binary is built on the local WSL musl toolchain. The production host
# only pulls/runs this image and never compiles Rust.
FROM alpine:3.20

RUN addgroup -S -g 1000 pmman && adduser -S -u 1000 -G pmman -h /nonexistent pmman

COPY server/target/x86_64-unknown-linux-musl/release/pmman-control-plane /usr/local/bin/pmman-control-plane
RUN chmod 0555 /usr/local/bin/pmman-control-plane

USER pmman
WORKDIR /var/lib/pmman
EXPOSE 8787

HEALTHCHECK --interval=10s --timeout=3s --start-period=10s --retries=6 \
  CMD wget -q -O - http://127.0.0.1:8787/healthz | grep -qx ok

ENTRYPOINT ["/usr/local/bin/pmman-control-plane"]
