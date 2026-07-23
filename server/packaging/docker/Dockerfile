FROM rust:trixie AS build

WORKDIR /workspace
COPY Cargo.toml Cargo.lock ./
COPY core ./core
COPY server ./server
RUN cargo build --locked --release --package wardrobe-server

FROM debian:trixie-slim

COPY --from=build /workspace/target/release/wardrobe-server /usr/local/bin/wardrobe-server
COPY server/packaging/docker/hardening.yml /tmp/hardening.yml
RUN apt-get update \
    && apt-get install --yes --no-install-recommends ansible-core ca-certificates \
    && ansible-playbook --inventory localhost, --connection local /tmp/hardening.yml \
    && apt-get purge --yes --auto-remove ansible-core \
    && rm -rf /var/lib/apt/lists/* /tmp/hardening.yml

USER wardrobe
WORKDIR /var/lib/wardrobe
VOLUME ["/var/lib/wardrobe"]
EXPOSE 24842
STOPSIGNAL SIGTERM

ENTRYPOINT ["/usr/local/bin/wardrobe-server"]
CMD ["--data-dir", "/var/lib/wardrobe", "--tcp-bind", "0.0.0.0:24842"]