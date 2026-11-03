# Build a musl binary. The default image is the Coolify runtime.
# Home Assistant add-on: docker build --target addon
FROM rust:1-alpine AS builder
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release -p ocpp-proxy

FROM ghcr.io/hassio-addons/base:21.0.10 AS addon
COPY --from=builder /src/target/release/ocpp-proxy /usr/bin/ocpp-proxy
COPY .hacs/run.sh /run.sh
RUN chmod a+x /run.sh /usr/bin/ocpp-proxy
EXPOSE 9000
LABEL \
    io.hass.name="OCPP Proxy" \
    io.hass.description="Multi-version OCPP proxy for secure EV charger sharing" \
    io.hass.type="addon" \
    maintainer="OpenChargeHub"
CMD ["/run.sh"]

FROM alpine:3 AS runtime
RUN apk add --no-cache wget ca-certificates \
    && mkdir -p /data
COPY --from=builder /src/target/release/ocpp-proxy /usr/bin/ocpp-proxy
ENV PORT=9000 \
    LOG_DB_PATH=/data/usage_log.db
EXPOSE 9000
HEALTHCHECK CMD wget -qO- http://127.0.0.1:9000/status || exit 1
CMD ["ocpp-proxy"]
