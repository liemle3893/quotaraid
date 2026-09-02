# Containerised rather than run under systemd: on a box without passwordless
# sudo and without lingering enabled, a --user unit does not survive logout.
# Docker needs neither, and --restart unless-stopped covers reboots.
FROM rust:slim AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release

FROM debian:stable-slim
# No TLS features are enabled, so there is nothing to link against here —
# agents reach the hub over Tailscale, and the public side is terminated by
# whatever reverse proxy fronts the host.
COPY --from=build /src/target/release/quotaraid /usr/local/bin/quotaraid
EXPOSE 7777
ENTRYPOINT ["quotaraid", "hub", "--listen", "0.0.0.0:7777"]
