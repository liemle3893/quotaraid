# Deployed to liemlhd-devbox this way rather than under systemd: the account
# there has no passwordless sudo and no lingering enabled, so a --user unit
# would not survive logout. Docker needs neither, and --restart unless-stopped
# covers reboots.
FROM rust:slim AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release

FROM debian:stable-slim
# No TLS features are enabled, so there is nothing to link against here —
# agents reach the hub over Tailscale, and the public side is terminated by
# the devbox's own proxy.
COPY --from=build /src/target/release/bossfight /usr/local/bin/bossfight
EXPOSE 7777
ENTRYPOINT ["bossfight", "hub", "--listen", "0.0.0.0:7777"]
