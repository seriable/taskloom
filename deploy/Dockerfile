# No target/, state directory, credential, or model weight is copied.
ARG RUST_IMAGE=rust:1.86.0-bookworm
ARG RUNTIME_IMAGE=debian:bookworm-slim
FROM scratch AS context-audit
# Optional local validation target: exports only the allowlisted build context.
COPY . /

FROM ${RUST_IMAGE} AS build
ARG TASKLOOM_FEATURES=app,database
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY examples ./examples
COPY sandbox ./sandbox
COPY docs ./docs
COPY README.md README.en.md CHANGELOG.md CONTRIBUTING.md LICENSE-* ./
RUN cargo build --locked --release --features "${TASKLOOM_FEATURES}" --bin taskloom --example multiuser_host

FROM ${RUNTIME_IMAGE} AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates libgcc-s1 \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 taskloom \
    && useradd --uid 10001 --gid 10001 --no-create-home --shell /usr/sbin/nologin taskloom \
    && mkdir -p /var/lib/taskloom/project && chown -R 10001:10001 /var/lib/taskloom
COPY --from=build /build/target/release/taskloom /usr/local/bin/taskloom
# Trusted one-shot identity/quota administration, never mounted as an HTTP API.
COPY --from=build /build/target/release/examples/multiuser_host /usr/local/bin/taskloom-admin
COPY --chmod=0555 deploy/entrypoint.sh /usr/local/bin/taskloom-entrypoint
USER 10001:10001
WORKDIR /var/lib/taskloom
EXPOSE 8080
STOPSIGNAL SIGTERM
ENTRYPOINT ["/usr/local/bin/taskloom-entrypoint"]
CMD ["taskloom", "service-serve", "/etc/taskloom/host.json"]
